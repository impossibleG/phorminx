use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use phorminx_audio::{ActiveRecording, start_default};
use phorminx_core::{
    AudioClip, DictationId, RuntimeState, RuntimeStateMachine, SpeechRecognizer, Transcript,
    TranscriptionOptions, normalize_transcript, recommended_audio_context,
};
use phorminx_whisper::WhisperRecognizer;
use phorminx_windows::{
    GlobalHoldHotkey, HoldEvent, InsertionOutcome, TargetSnapshot, copy_and_maybe_paste,
};

#[derive(Debug, Parser)]
#[command(name = "phorminx")]
#[command(about = "Local-first Windows push-to-talk dictation")]
struct Cli {
    #[arg(long, default_value = "models/ggml-base.en.bin")]
    model: PathBuf,
    #[arg(long, default_value = "en")]
    language: String,
    /// Recordings quieter than this RMS value are treated as silence.
    #[arg(long, default_value_t = 0.003)]
    minimum_rms: f32,
    /// Start every service and then exit cleanly without accepting dictation.
    #[arg(long, hide = true)]
    smoke_test: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    println!("Phorminx is loading {}...", cli.model.display());
    let worker = TranscriptionWorker::start(&cli.model)?;

    let shutting_down = Arc::new(AtomicBool::new(false));
    let shutdown_flag = Arc::clone(&shutting_down);
    ctrlc::set_handler(move || shutdown_flag.store(true, Ordering::Release))
        .context("failed to install the Ctrl+C handler")?;

    let hotkey = GlobalHoldHotkey::start().context("failed to install the global hotkey")?;
    let mut runtime = RuntimeStateMachine::default();
    runtime.mark_ready()?;
    let mut recording: Option<ActiveRecording> = None;
    let mut target: Option<TargetSnapshot> = None;
    let mut pending_id: Option<DictationId> = None;

    println!("Ready. Hold Ctrl+Alt+Space to dictate; press Ctrl+C here to exit.");
    log_state(None, runtime.state(), "ready");
    if cli.smoke_test {
        shutting_down.store(true, Ordering::Release);
    }

    while !shutting_down.load(Ordering::Acquire) {
        match hotkey.events().recv_timeout(Duration::from_millis(25)) {
            Ok(HoldEvent::Started {
                target: activation_target,
            }) => {
                if runtime.state() != RuntimeState::Idle {
                    log_state(
                        runtime.active_id(),
                        runtime.state(),
                        "activation_rejected_busy",
                    );
                    continue;
                }

                let id = runtime.begin_dictation()?;
                match start_default() {
                    Ok(active_recording) => {
                        recording = Some(active_recording);
                        target = activation_target;
                        println!("Listening...");
                        log_state(Some(id), runtime.state(), "recording_started");
                    }
                    Err(error) => {
                        fail_and_reset(&mut runtime, Some(id), "audio_start_failed");
                        eprintln!("Could not start the microphone: {error}");
                    }
                }
            }
            Ok(HoldEvent::Ended) => {
                if runtime.state() != RuntimeState::Listening {
                    continue;
                }
                let id = runtime.active_id().expect("listening state has an id");
                runtime.transition(RuntimeState::FinalizingAudio)?;
                log_state(Some(id), runtime.state(), "recording_stopped");

                let captured = match recording
                    .take()
                    .expect("listening state has a recorder")
                    .finish_with_diagnostics()
                {
                    Ok(captured) => captured,
                    Err(error) => {
                        fail_and_reset(&mut runtime, Some(id), "audio_finish_failed");
                        eprintln!("Could not finish the recording: {error}");
                        continue;
                    }
                };
                if !captured.warnings.is_empty() {
                    eprintln!(
                        "dictation_id={} state={:?} event=audio_backend_warning warning_count={}",
                        id.0,
                        runtime.state(),
                        captured.warnings.len()
                    );
                }
                let clip = captured.clip;

                if clip.duration() < Duration::from_millis(200) || clip.rms() < cli.minimum_rms {
                    runtime.cancel()?;
                    log_state(Some(id), runtime.state(), "silence_rejected");
                    runtime.transition(RuntimeState::Idle)?;
                    target = None;
                    println!("No clear speech detected.");
                    continue;
                }

                let audio_context = recommended_audio_context(clip.duration());
                runtime.transition(RuntimeState::Transcribing)?;
                log_state(Some(id), runtime.state(), "transcription_started");
                worker.transcribe(id, clip, cli.language.clone(), audio_context)?;
                pending_id = Some(id);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("the global hotkey thread stopped unexpectedly"));
            }
        }

        while let Ok(completed) = worker.results.try_recv() {
            if pending_id != Some(completed.id) || runtime.state() != RuntimeState::Transcribing {
                log_state(
                    Some(completed.id),
                    runtime.state(),
                    "stale_transcription_discarded",
                );
                continue;
            }
            pending_id = None;

            let transcript = match completed.result {
                Ok(transcript) => transcript,
                Err(message) => {
                    fail_and_reset(&mut runtime, Some(completed.id), "transcription_failed");
                    target = None;
                    eprintln!("Transcription failed: {message}");
                    continue;
                }
            };

            runtime.transition(RuntimeState::Normalizing)?;
            let normalized = normalize_transcript(&transcript.text);
            if normalized.is_empty() {
                runtime.cancel()?;
                log_state(
                    Some(completed.id),
                    runtime.state(),
                    "empty_transcript_rejected",
                );
                runtime.transition(RuntimeState::Idle)?;
                target = None;
                continue;
            }

            runtime.transition(RuntimeState::ReadyToInsert)?;
            runtime.transition(RuntimeState::Inserting)?;
            match copy_and_maybe_paste(target.take(), &normalized) {
                Ok(InsertionOutcome::Pasted) => {
                    println!(
                        "Inserted in {:.0} ms.",
                        transcript.inference_time.as_secs_f64() * 1_000.0
                    );
                    log_state(Some(completed.id), runtime.state(), "paste_injected");
                    runtime.transition(RuntimeState::Idle)?;
                }
                Ok(InsertionOutcome::ClipboardOnly(reason)) => {
                    println!("Ready to paste from the clipboard ({reason:?}).");
                    log_state(Some(completed.id), runtime.state(), "clipboard_only");
                    runtime.transition(RuntimeState::Idle)?;
                }
                Err(error) => {
                    fail_and_reset(&mut runtime, Some(completed.id), "insertion_failed");
                    eprintln!("Could not prepare the transcript for insertion: {error}");
                }
            }
        }
    }

    println!("Shutting down Phorminx.");
    hotkey.shutdown()?;
    worker.shutdown()?;
    Ok(())
}

fn fail_and_reset(runtime: &mut RuntimeStateMachine, id: Option<DictationId>, event: &'static str) {
    if runtime.fault().is_ok() {
        log_state(id, runtime.state(), event);
        let _ = runtime.transition(RuntimeState::Idle);
    }
}

fn log_state(id: Option<DictationId>, state: RuntimeState, event: &'static str) {
    let id = id.map_or(0, |id| id.0);
    eprintln!("dictation_id={id} state={state:?} event={event}");
}

struct TranscriptionWorker {
    commands: Sender<WorkerCommand>,
    results: Receiver<WorkerResult>,
    thread: Option<JoinHandle<()>>,
}

impl TranscriptionWorker {
    fn start(model: &Path) -> Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let model = model.to_path_buf();
        let thread = thread::Builder::new()
            .name("phorminx-transcription".to_owned())
            .spawn(move || {
                let recognizer = match WhisperRecognizer::load(&model) {
                    Ok(recognizer) => recognizer,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                if ready_tx.send(Ok(())).is_err() {
                    return;
                }

                while let Ok(command) = command_rx.recv() {
                    match command {
                        WorkerCommand::Transcribe {
                            id,
                            clip,
                            language,
                            audio_context,
                        } => {
                            let options = TranscriptionOptions {
                                language: Some(&language),
                                thread_count: None,
                                audio_context: Some(audio_context),
                            };
                            let result = recognizer
                                .transcribe(&clip, &options)
                                .map_err(|error| error.to_string());
                            if result_tx.send(WorkerResult { id, result }).is_err() {
                                break;
                            }
                        }
                        WorkerCommand::Shutdown => break,
                    }
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands: command_tx,
                results: result_rx,
                thread: Some(thread),
            }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(anyhow!(message))
            }
            Err(_) => {
                let _ = thread.join();
                Err(anyhow!("transcription worker exited during startup"))
            }
        }
    }

    fn transcribe(
        &self,
        id: DictationId,
        clip: AudioClip,
        language: String,
        audio_context: u32,
    ) -> Result<()> {
        self.commands
            .send(WorkerCommand::Transcribe {
                id,
                clip,
                language,
                audio_context,
            })
            .context("transcription worker is unavailable")
    }

    fn shutdown(mut self) -> Result<()> {
        self.stop()
    }

    fn stop(&mut self) -> Result<()> {
        if self.thread.is_none() {
            return Ok(());
        }
        let _ = self.commands.send(WorkerCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow!("transcription worker panicked"))?;
        }
        Ok(())
    }
}

impl Drop for TranscriptionWorker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

enum WorkerCommand {
    Transcribe {
        id: DictationId,
        clip: AudioClip,
        language: String,
        audio_context: u32,
    },
    Shutdown,
}

struct WorkerResult {
    id: DictationId,
    result: Result<Transcript, String>,
}

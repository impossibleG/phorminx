use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use phorminx_app::runtime::{
    AppIo, AppRuntime, FinishedAudio, InsertDisposition, RuntimeNotice, UiStatus,
};
use phorminx_audio::{ActiveRecording, start_default};
use phorminx_core::{
    AudioClip, DictationId, RuntimeState, SpeechRecognizer, Transcript, TranscriptionOptions,
};
use phorminx_whisper::WhisperRecognizer;
use phorminx_windows::{
    ClipboardOnlyReason, GlobalHoldHotkey, HoldEvent, InsertionOutcome, OverlayStatus,
    StatusOverlay, TargetSnapshot, copy_and_maybe_paste,
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
    /// Cycle through every fixed overlay state without loading speech recognition.
    #[arg(long, hide = true)]
    overlay_demo: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let overlay = StatusOverlay::start().context("failed to start the status overlay")?;
    if cli.overlay_demo {
        for status in [
            OverlayStatus::Loading,
            OverlayStatus::Ready,
            OverlayStatus::Listening,
            OverlayStatus::Transcribing,
            OverlayStatus::Inserted,
            OverlayStatus::ClipboardReady,
            OverlayStatus::NoSpeech,
            OverlayStatus::Error,
        ] {
            show_status(&overlay, status);
            thread::sleep(Duration::from_millis(900));
        }
        overlay.shutdown()?;
        return Ok(());
    }
    show_status(&overlay, OverlayStatus::Loading);
    println!("Phorminx is loading {}...", cli.model.display());
    let worker = TranscriptionWorker::start(&cli.model)?;

    let shutting_down = Arc::new(AtomicBool::new(false));
    let shutdown_flag = Arc::clone(&shutting_down);
    ctrlc::set_handler(move || shutdown_flag.store(true, Ordering::Release))
        .context("failed to install the Ctrl+C handler")?;

    let hotkey = GlobalHoldHotkey::start().context("failed to install the global hotkey")?;
    let mut runtime =
        AppRuntime::<TargetSnapshot, ActiveRecording>::new(cli.minimum_rms, cli.language.clone())?;

    println!("Ready. Hold Ctrl+Alt+Space to dictate; press Ctrl+C here to exit.");
    log_state(None, runtime.state(), "ready");
    {
        let mut io = ProductionIo {
            overlay: &overlay,
            worker: &worker,
        };
        report_notices(runtime.announce_ready(&mut io));
    }
    if cli.smoke_test {
        shutting_down.store(true, Ordering::Release);
    }

    while !shutting_down.load(Ordering::Acquire) {
        match hotkey.events().recv_timeout(Duration::from_millis(25)) {
            Ok(HoldEvent::Started {
                target: activation_target,
            }) => {
                let notices = {
                    let mut io = ProductionIo {
                        overlay: &overlay,
                        worker: &worker,
                    };
                    runtime.hold_started(activation_target, &mut io)?
                };
                report_notices(notices);
            }
            Ok(HoldEvent::Ended) => {
                let notices = {
                    let mut io = ProductionIo {
                        overlay: &overlay,
                        worker: &worker,
                    };
                    runtime.hold_ended(&mut io)?
                };
                report_notices(notices);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("the global hotkey thread stopped unexpectedly"));
            }
        }

        loop {
            match worker.results.try_recv() {
                Ok(completed) => {
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            worker: &worker,
                        };
                        runtime.transcription_completed(completed.id, completed.result, &mut io)?
                    };
                    report_notices(notices);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            worker: &worker,
                        };
                        runtime.worker_disconnected(&mut io)?
                    };
                    report_notices(notices);
                    return Err(anyhow!("the transcription worker stopped unexpectedly"));
                }
            }
        }
    }

    println!("Shutting down Phorminx.");
    hotkey.shutdown()?;
    worker.shutdown()?;
    overlay.shutdown()?;
    Ok(())
}

fn show_status(overlay: &StatusOverlay, status: OverlayStatus) {
    if let Err(error) = overlay.set(status) {
        eprintln!("dictation_id=0 state=Overlay event=status_update_failed error={error}");
    }
}

struct ProductionIo<'a> {
    overlay: &'a StatusOverlay,
    worker: &'a TranscriptionWorker,
}

impl AppIo for ProductionIo<'_> {
    type Target = TargetSnapshot;
    type Recording = ActiveRecording;
    type ClipboardReason = ClipboardOnlyReason;

    fn start_recording(&mut self) -> Result<Self::Recording, String> {
        start_default().map_err(|error| error.to_string())
    }

    fn finish_recording(&mut self, recording: Self::Recording) -> Result<FinishedAudio, String> {
        recording
            .finish_with_diagnostics()
            .map(|captured| FinishedAudio {
                clip: captured.clip,
                backend_warning_count: captured.backend_warning_count,
            })
            .map_err(|error| error.to_string())
    }

    fn submit_transcription(
        &mut self,
        id: DictationId,
        clip: AudioClip,
        language: &str,
        audio_context: u32,
    ) -> Result<(), String> {
        self.worker
            .transcribe(id, clip, language.to_owned(), audio_context)
            .map_err(|error| error.to_string())
    }

    fn insert(
        &mut self,
        target: Option<Self::Target>,
        text: &str,
    ) -> Result<InsertDisposition<Self::ClipboardReason>, String> {
        copy_and_maybe_paste(target, text)
            .map(|outcome| match outcome {
                InsertionOutcome::Pasted => InsertDisposition::Pasted,
                InsertionOutcome::ClipboardOnly(reason) => InsertDisposition::ClipboardOnly(reason),
            })
            .map_err(|error| error.to_string())
    }

    fn show_status(&mut self, status: UiStatus) -> Result<(), String> {
        let status = match status {
            UiStatus::Ready => OverlayStatus::Ready,
            UiStatus::Listening => OverlayStatus::Listening,
            UiStatus::Transcribing => OverlayStatus::Transcribing,
            UiStatus::Inserted => OverlayStatus::Inserted,
            UiStatus::ClipboardReady => OverlayStatus::ClipboardReady,
            UiStatus::NoSpeech => OverlayStatus::NoSpeech,
            UiStatus::Error => OverlayStatus::Error,
        };
        self.overlay.set(status).map_err(|error| error.to_string())
    }
}

fn report_notices(notices: Vec<RuntimeNotice<ClipboardOnlyReason>>) {
    for notice in notices {
        match notice {
            RuntimeNotice::BusyRejected { id, state } => {
                log_state(id, state, "activation_rejected_busy");
            }
            RuntimeNotice::RecordingStarted { id } => {
                println!("Listening...");
                log_state(Some(id), RuntimeState::Listening, "recording_started");
            }
            RuntimeNotice::RecordingStopped { id } => {
                log_state(Some(id), RuntimeState::FinalizingAudio, "recording_stopped");
            }
            RuntimeNotice::AudioBackendWarning { id, state, count } => {
                eprintln!(
                    "dictation_id={} state={state:?} event=audio_backend_warning warning_count={count}",
                    id.0
                );
            }
            RuntimeNotice::NoSpeech { id, event } => {
                log_state(Some(id), RuntimeState::Cancelled, event);
                if event == "silence_rejected" {
                    println!("No clear speech detected.");
                }
            }
            RuntimeNotice::TranscriptionStarted { id } => {
                log_state(
                    Some(id),
                    RuntimeState::Transcribing,
                    "transcription_started",
                );
            }
            RuntimeNotice::StaleTranscription { id, state } => {
                log_state(Some(id), state, "stale_transcription_discarded");
            }
            RuntimeNotice::Inserted { id, inference_time } => {
                println!(
                    "Inserted in {:.0} ms.",
                    inference_time.as_secs_f64() * 1_000.0
                );
                log_state(Some(id), RuntimeState::Inserting, "paste_injected");
            }
            RuntimeNotice::ClipboardReady { id, reason } => {
                println!("Ready to paste from the clipboard ({reason:?}).");
                log_state(Some(id), RuntimeState::Inserting, "clipboard_only");
            }
            RuntimeNotice::Failure { id, event, message } => {
                log_state(id, RuntimeState::Faulted, event);
                match event {
                    "audio_start_failed" => {
                        eprintln!("Could not start the microphone: {message}");
                    }
                    "audio_finish_failed" => {
                        eprintln!("Could not finish the recording: {message}");
                    }
                    "transcription_failed" => eprintln!("Transcription failed: {message}"),
                    "transcription_submit_failed" => {
                        eprintln!("Could not submit the transcription: {message}");
                    }
                    "insertion_failed" => {
                        eprintln!("Could not prepare the transcript for insertion: {message}");
                    }
                    _ => eprintln!("Phorminx runtime failure: {message}"),
                }
            }
            RuntimeNotice::StatusUpdateFailed { message } => {
                eprintln!(
                    "dictation_id=0 state=Overlay event=status_update_failed error={message}"
                );
            }
        }
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

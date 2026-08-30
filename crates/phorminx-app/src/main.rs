#![cfg_attr(all(windows, feature = "desktop"), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, ValueEnum};
use phorminx_app::runtime::{
    AppIo, AppRuntime, FinishedAudio, InsertDisposition, RuntimeNotice, UiStatus,
};
use phorminx_app::settings::{FormattingStrength, RuntimeFormatting, Settings, SettingsStore};
use phorminx_audio::{ActiveRecording, input_devices, start_default};
use phorminx_core::{
    AudioClip, DictationId, RuntimeState, SpeechRecognizer, Transcript, TranscriptionOptions,
};
use phorminx_whisper::WhisperRecognizer;
use phorminx_windows::{
    ClipboardOnlyReason, GlobalHoldHotkey, HoldEvent, InsertionOutcome, OverlayStatus,
    SettingsForm, SettingsFormatting, SettingsWindow, SettingsWindowEvent, StatusOverlay,
    SystemTray, TargetSnapshot, TrayEvent, TrayStatus, copy_and_maybe_paste,
};

#[cfg(feature = "desktop")]
use phorminx_windows::show_error_dialog;

#[derive(Debug, Parser)]
#[command(name = "phorminx")]
#[command(about = "Local-first Windows push-to-talk dictation")]
struct Cli {
    /// Override the settings file for this run (relative paths use the current directory).
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    model: Option<PathBuf>,
    #[arg(long)]
    language: Option<String>,
    /// Recordings quieter than this RMS value are treated as silence.
    #[arg(long)]
    minimum_rms: Option<f32>,
    /// Override transcript formatting for this run.
    #[arg(long, value_enum)]
    formatting: Option<FormattingCli>,
    /// Start every service and then exit cleanly without accepting dictation.
    #[arg(long, hide = true)]
    smoke_test: bool,
    /// Cycle through every fixed overlay state without loading speech recognition.
    #[arg(long, hide = true)]
    overlay_demo: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FormattingCli {
    Raw,
    Light,
    Balanced,
    Strong,
    Custom,
}

impl From<FormattingCli> for FormattingStrength {
    fn from(value: FormattingCli) -> Self {
        match value {
            FormattingCli::Raw => Self::Raw,
            FormattingCli::Light => Self::Light,
            FormattingCli::Balanced => Self::Balanced,
            FormattingCli::Strong => Self::Strong,
            FormattingCli::Custom => Self::Custom,
        }
    }
}

fn main() {
    if let Err(error) = run() {
        report_fatal_error(&error);
        std::process::exit(1);
    }
}

fn report_fatal_error(error: &anyhow::Error) {
    #[cfg(feature = "desktop")]
    show_error_dialog("Phorminx could not start", &format!("{error:#}"));

    #[cfg(not(feature = "desktop"))]
    eprintln!("Phorminx failed: {error:#}");
}

fn run() -> Result<()> {
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

    let settings_store = match cli.config.clone() {
        Some(path) => SettingsStore::new(path),
        None => SettingsStore::default_for_current_user(),
    }
    .context("failed to resolve the settings file")?;
    let settings_file_exists = settings_store.path().is_file();
    let mut settings = settings_store.load().with_context(|| {
        format!(
            "failed to load settings from {}",
            settings_store.path().display()
        )
    })?;
    if let Some(language) = cli.language {
        settings.recognition.language = language;
    }
    if let Some(minimum_rms) = cli.minimum_rms {
        settings.recognition.minimum_rms = minimum_rms;
    }
    if let Some(formatting) = cli.formatting {
        settings.formatting.strength = formatting.into();
    }
    settings
        .validate_and_normalize()
        .context("invalid effective settings")?;
    let formatting = RuntimeFormatting::try_from(settings.formatting.strength)?;
    let model = match cli.model {
        Some(model) => model,
        None if settings_file_exists => {
            settings_store.resolve_model_path(&settings.recognition.model_path)
        }
        None => settings.recognition.model_path.clone(),
    };
    let model = if model.is_absolute() {
        model
    } else {
        std::env::current_dir()
            .context("failed to resolve the model path from the current directory")?
            .join(model)
    };

    let tray = SystemTray::start().context("failed to start the system tray")?;
    show_shell_status(&overlay, &tray, OverlayStatus::Loading, TrayStatus::Loading);
    println!("Phorminx is loading {}...", model.display());
    let worker = TranscriptionWorker::start(&model)?;

    let shutting_down = Arc::new(AtomicBool::new(false));
    #[cfg(not(feature = "desktop"))]
    {
        let shutdown_flag = Arc::clone(&shutting_down);
        ctrlc::set_handler(move || shutdown_flag.store(true, Ordering::Release))
            .context("failed to install the Ctrl+C handler")?;
    }

    let hotkey = GlobalHoldHotkey::start().context("failed to install the global hotkey")?;
    let mut runtime = AppRuntime::<TargetSnapshot, ActiveRecording>::new_with_formatting(
        settings.recognition.minimum_rms,
        settings.recognition.language.clone(),
        formatting,
    )?;

    println!("Ready. Hold Ctrl+Alt+Space to dictate; press Ctrl+C here to exit.");
    log_state(None, runtime.state(), "ready");
    {
        let mut io = ProductionIo {
            overlay: &overlay,
            tray: &tray,
            worker: &worker,
        };
        report_notices(runtime.announce_ready(&mut io));
    }
    if cli.smoke_test {
        shutting_down.store(true, Ordering::Release);
    }

    let mut settings_window = None;
    let mut restart_requested = false;

    let run_result: Result<()> = 'event_loop: loop {
        if shutting_down.load(Ordering::Acquire) {
            break Ok(());
        }
        match poll_shell_events(
            &tray,
            &overlay,
            &mut settings_window,
            &settings_store,
            &mut settings,
            &model,
        ) {
            Ok(ShellAction::Quit) => break Ok(()),
            Ok(ShellAction::Restart) => {
                restart_requested = true;
                break Ok(());
            }
            Ok(ShellAction::Continue) => {}
            Err(error) => break Err(error),
        }

        let hotkey_event = hotkey.events().recv_timeout(Duration::from_millis(25));

        // Tray Quit has priority over an activation or completed transcription that
        // arrived during the wait, preventing any new work or insertion after Quit.
        match poll_shell_events(
            &tray,
            &overlay,
            &mut settings_window,
            &settings_store,
            &mut settings,
            &model,
        ) {
            Ok(ShellAction::Quit) => break Ok(()),
            Ok(ShellAction::Restart) => {
                restart_requested = true;
                break Ok(());
            }
            Ok(ShellAction::Continue) => {}
            Err(error) => break Err(error),
        }

        match hotkey_event {
            Ok(HoldEvent::Started {
                target: activation_target,
            }) => {
                let notices = {
                    let mut io = ProductionIo {
                        overlay: &overlay,
                        tray: &tray,
                        worker: &worker,
                    };
                    match runtime.hold_started(activation_target, &mut io) {
                        Ok(notices) => notices,
                        Err(error) => break 'event_loop Err(error.into()),
                    }
                };
                report_notices(notices);
            }
            Ok(HoldEvent::Ended) => {
                let notices = {
                    let mut io = ProductionIo {
                        overlay: &overlay,
                        tray: &tray,
                        worker: &worker,
                    };
                    match runtime.hold_ended(&mut io) {
                        Ok(notices) => notices,
                        Err(error) => break 'event_loop Err(error.into()),
                    }
                };
                report_notices(notices);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break Err(anyhow!("the global hotkey thread stopped unexpectedly"));
            }
        }

        match poll_shell_events(
            &tray,
            &overlay,
            &mut settings_window,
            &settings_store,
            &mut settings,
            &model,
        ) {
            Ok(ShellAction::Quit) => break Ok(()),
            Ok(ShellAction::Restart) => {
                restart_requested = true;
                break Ok(());
            }
            Ok(ShellAction::Continue) => {}
            Err(error) => break Err(error),
        }

        loop {
            match worker.results.try_recv() {
                Ok(completed) => {
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            tray: &tray,
                            worker: &worker,
                        };
                        match runtime.transcription_completed(
                            completed.id,
                            completed.result,
                            &mut io,
                        ) {
                            Ok(notices) => notices,
                            Err(error) => break 'event_loop Err(error.into()),
                        }
                    };
                    report_notices(notices);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            tray: &tray,
                            worker: &worker,
                        };
                        match runtime.worker_disconnected(&mut io) {
                            Ok(notices) => notices,
                            Err(error) => break 'event_loop Err(error.into()),
                        }
                    };
                    report_notices(notices);
                    break 'event_loop Err(anyhow!(
                        "the transcription worker stopped unexpectedly"
                    ));
                }
            }
        }
    };

    println!("Shutting down Phorminx.");
    let mut final_error = run_result.err();
    drop(runtime);
    if let Some(window) = settings_window {
        preserve_first_error(
            &mut final_error,
            window
                .shutdown()
                .context("failed to stop the settings window"),
        );
    }
    preserve_first_error(
        &mut final_error,
        hotkey
            .shutdown()
            .context("failed to stop the global hotkey"),
    );
    preserve_first_error(
        &mut final_error,
        worker.shutdown().context("failed to stop transcription"),
    );
    preserve_first_error(
        &mut final_error,
        tray.shutdown().context("failed to stop the system tray"),
    );
    preserve_first_error(
        &mut final_error,
        overlay
            .shutdown()
            .context("failed to stop the status overlay"),
    );
    if final_error.is_none() && restart_requested {
        preserve_first_error(
            &mut final_error,
            restart_with_settings(settings_store.path()),
        );
    }
    match final_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellAction {
    Continue,
    Quit,
    Restart,
}

fn poll_shell_events(
    tray: &SystemTray,
    overlay: &StatusOverlay,
    settings_window: &mut Option<SettingsWindow>,
    settings_store: &SettingsStore,
    settings: &mut Settings,
    effective_model: &Path,
) -> Result<ShellAction> {
    loop {
        match tray.events().try_recv() {
            Ok(TrayEvent::QuitRequested) => return Ok(ShellAction::Quit),
            Ok(TrayEvent::OpenSettings) => {
                if let Some(window) = settings_window {
                    window.focus().context("failed to focus settings")?;
                } else {
                    *settings_window = Some(
                        SettingsWindow::start(settings_form(settings, effective_model))
                            .context("failed to open settings")?,
                    );
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                return Err(anyhow!("the system tray thread stopped unexpectedly"));
            }
        }
    }

    let mut window_events = Vec::new();
    if let Some(window) = settings_window.as_ref() {
        loop {
            match window.events().try_recv() {
                Ok(event) => window_events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !window_events
                        .iter()
                        .any(|event| matches!(event, SettingsWindowEvent::Closed))
                    {
                        window_events.push(SettingsWindowEvent::Closed);
                    }
                    break;
                }
            }
        }
    }

    for event in window_events {
        match event {
            SettingsWindowEvent::Closed => {
                if let Some(window) = settings_window.take() {
                    window
                        .shutdown()
                        .context("failed to join the settings window")?;
                }
            }
            SettingsWindowEvent::SaveAndRestart(form) => {
                let save_result = apply_settings_form(settings_store, settings, form);
                match save_result {
                    Ok(()) => return Ok(ShellAction::Restart),
                    Err(error) => {
                        if let Some(window) = settings_window.as_ref() {
                            window
                                .show_error(format!("{error:#}"))
                                .context("failed to display the settings error")?;
                        }
                        show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                    }
                }
            }
        }
    }

    Ok(ShellAction::Continue)
}

fn settings_form(settings: &Settings, effective_model: &Path) -> SettingsForm {
    let model_status = match std::fs::metadata(effective_model) {
        Ok(metadata) if metadata.is_file() => format!(
            "Model ready ({:.1} MiB)",
            metadata.len() as f64 / (1024.0 * 1024.0)
        ),
        Ok(_) => "The selected model path is not a file".to_owned(),
        Err(_) => "Model not found - choose a local .bin file".to_owned(),
    };
    let microphone_status = match input_devices() {
        Ok(devices) => devices
            .iter()
            .find(|device| device.is_default)
            .map(|device| format!("Default microphone: {}", device.name))
            .unwrap_or_else(|| {
                if devices.is_empty() {
                    "No microphone input devices were found".to_owned()
                } else {
                    format!(
                        "{} microphone(s) found; Windows has no default",
                        devices.len()
                    )
                }
            }),
        Err(error) => format!("Microphone check failed: {error}"),
    };
    SettingsForm {
        model_path: effective_model.display().to_string(),
        model_status,
        microphone_status,
        language: settings.recognition.language.clone(),
        minimum_rms: settings.recognition.minimum_rms.to_string(),
        formatting: match settings.formatting.strength {
            FormattingStrength::Raw => SettingsFormatting::Raw,
            FormattingStrength::Light => SettingsFormatting::Light,
            FormattingStrength::Balanced => SettingsFormatting::Balanced,
            FormattingStrength::Strong => SettingsFormatting::Strong,
            FormattingStrength::Custom => SettingsFormatting::Custom,
        },
        custom_instructions: settings
            .formatting
            .custom_instructions
            .clone()
            .unwrap_or_default(),
    }
}

fn apply_settings_form(
    store: &SettingsStore,
    settings: &mut Settings,
    form: SettingsForm,
) -> Result<()> {
    let mut candidate = settings.clone();
    candidate.recognition.model_path = PathBuf::from(form.model_path.trim());
    candidate.recognition.language = form.language;
    candidate.recognition.minimum_rms = form
        .minimum_rms
        .trim()
        .parse::<f32>()
        .context("Minimum speech level must be a number between 0 and 1")?;
    candidate.formatting.strength = match form.formatting {
        SettingsFormatting::Raw => FormattingStrength::Raw,
        SettingsFormatting::Light => FormattingStrength::Light,
        SettingsFormatting::Balanced => FormattingStrength::Balanced,
        SettingsFormatting::Strong => FormattingStrength::Strong,
        SettingsFormatting::Custom => FormattingStrength::Custom,
    };
    candidate.formatting.custom_instructions = if form.custom_instructions.trim().is_empty() {
        None
    } else {
        Some(form.custom_instructions)
    };
    candidate
        .validate_and_normalize()
        .context("The settings are not valid")?;
    candidate
        .ensure_runtime_supported()
        .context("This formatting profile is not ready")?;
    store.save(&candidate).context("Could not write settings")?;
    *settings = candidate;
    Ok(())
}

fn restart_with_settings(settings_path: &Path) -> Result<()> {
    let executable = std::env::current_exe().context("failed to locate the Phorminx executable")?;
    Command::new(executable)
        .arg("--config")
        .arg(settings_path)
        .spawn()
        .context("failed to restart Phorminx")?;
    Ok(())
}

fn preserve_first_error(first: &mut Option<anyhow::Error>, result: Result<()>) {
    if let Err(error) = result
        && first.is_none()
    {
        *first = Some(error);
    }
}

fn show_shell_status(
    overlay: &StatusOverlay,
    tray: &SystemTray,
    overlay_status: OverlayStatus,
    tray_status: TrayStatus,
) {
    show_status(overlay, overlay_status);
    if let Err(error) = tray.set_status(tray_status) {
        eprintln!("dictation_id=0 state=Tray event=status_update_failed error={error}");
    }
}

fn show_status(overlay: &StatusOverlay, status: OverlayStatus) {
    if let Err(error) = overlay.set(status) {
        eprintln!("dictation_id=0 state=Overlay event=status_update_failed error={error}");
    }
}

struct ProductionIo<'a> {
    overlay: &'a StatusOverlay,
    tray: &'a SystemTray,
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
        let (overlay_status, tray_status) = match status {
            UiStatus::Ready => (OverlayStatus::Ready, TrayStatus::Ready),
            UiStatus::Listening => (OverlayStatus::Listening, TrayStatus::Listening),
            UiStatus::Transcribing => (OverlayStatus::Transcribing, TrayStatus::Transcribing),
            UiStatus::Inserted => (OverlayStatus::Inserted, TrayStatus::Ready),
            UiStatus::ClipboardReady => (OverlayStatus::ClipboardReady, TrayStatus::Ready),
            UiStatus::NoSpeech => (OverlayStatus::NoSpeech, TrayStatus::Ready),
            UiStatus::Error => (OverlayStatus::Error, TrayStatus::Error),
        };
        let overlay_result = self
            .overlay
            .set(overlay_status)
            .map_err(|error| error.to_string());
        let tray_result = self
            .tray
            .set_status(tray_status)
            .map_err(|error| error.to_string());
        overlay_result.and(tray_result)
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

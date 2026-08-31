#![cfg_attr(all(windows, feature = "desktop"), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, ValueEnum};
use phorminx_app::model::{ModelDownload, ModelDownloadEvent, recommended_model};
use phorminx_app::product_shell::{ProductShell, ProductShellControl, ProductShellEvent};
use phorminx_app::runtime::{
    AppIo, AppRuntime, FinishedAudio, InsertDisposition, RuntimeNotice, UiStatus,
};
use phorminx_app::settings::{
    FormattingStrength, HistoryRetention, OllamaLifecycle, RecordingMode, RuntimeFormatting,
    Settings, SettingsStore,
};
use phorminx_app::ui_bridge::{UiMutation, UiRoute, UiRuntimeStatus};
use phorminx_audio::{ActiveRecording, input_devices, start_input};
use phorminx_core::{
    AudioClip, DictationId, RuntimeState, SpeechRecognizer, Transcript, TranscriptionOptions,
    normalize_transcript,
};
use phorminx_ollama::{
    CancellationToken, ClientTimeouts, FormatProfile, FormatResult, KeepAlive, ModelName,
    OllamaClient, OllamaEndpoint, SelectionPolicy,
};
use phorminx_persistence::{
    AppProfile, CasePolicy, DictationDraft, ExecutableIdentity, FormattingStyle,
    InsertionPreference, LexiconEntry, NewLexiconEntry, Persistence, RetentionPolicy,
    TimingMetadata,
};
use phorminx_whisper::WhisperRecognizer;
use phorminx_windows::{
    ClipboardOnlyReason, GlobalHoldHotkey, HistoryItem, HistoryWindow, HistoryWindowEvent,
    HoldEvent, InsertionOutcome, LexiconCasePolicy, LexiconDraft, LexiconItem, LexiconWindow,
    LexiconWindowEvent, OverlayStatus, ProfileFormatting, ProfileInsertion, ProfileItem,
    ProfileWindow, ProfileWindowEvent, SettingsForm, SettingsFormatting, SettingsHistoryRetention,
    SettingsOllamaLifecycle, SettingsRecordingMode, SettingsWindow, SettingsWindowEvent,
    SingleInstance, StatusOverlay, SystemTray, TargetSnapshot, TrayEvent, TrayStatus,
    copy_and_maybe_paste, set_launch_at_login,
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
    let _instance =
        SingleInstance::acquire().context("failed to acquire the Phorminx process slot")?;
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
    settings
        .ensure_runtime_supported()
        .context("invalid effective formatting settings")?;
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
    let shutting_down = Arc::new(AtomicBool::new(false));
    #[cfg(not(feature = "desktop"))]
    {
        let shutdown_flag = Arc::clone(&shutting_down);
        ctrlc::set_handler(move || shutdown_flag.store(true, Ordering::Release))
            .context("failed to install the Ctrl+C handler")?;
    }

    if !model.is_file() {
        if cli.smoke_test {
            return Err(anyhow!("Whisper model was not found: {}", model.display()));
        }
        return run_setup_mode(
            overlay,
            tray,
            settings_store,
            settings,
            model,
            shutting_down,
        );
    }

    let effective_microphone = match settings.recognition.microphone.as_deref() {
        Some(selected)
            if input_devices()
                .map(|devices| devices.iter().any(|device| device.name == selected))
                .unwrap_or(false) =>
        {
            Some(selected.to_owned())
        }
        Some(_) => {
            eprintln!(
                "dictation_id=0 state=Starting event=saved_microphone_unavailable recovery=windows_default"
            );
            None
        }
        None => None,
    };

    let database_path = settings_store
        .path()
        .parent()
        .context("settings path has no parent directory")?
        .join("phorminx.db");
    let persistence = Persistence::open(&database_path).context("failed to open local history")?;
    persistence
        .history()
        .set_retention(
            retention_policy(settings.privacy.history_retention),
            now_ms(),
        )
        .context("failed to apply history retention")?;
    let aliases = persistence
        .lexicon()
        .list()
        .context("failed to load the personal lexicon")?;
    let worker_formatting = WorkerFormatting::from_settings(&settings)?;
    let mut dictation_context = DictationContext::global(&settings)?;

    println!("Phorminx is loading {}...", model.display());
    let worker = TranscriptionWorker::start(&model, worker_formatting, aliases)?;

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
            microphone: effective_microphone.as_deref(),
            context: &dictation_context,
        };
        report_notices(runtime.announce_ready(&mut io));
    }
    if cli.smoke_test {
        shutting_down.store(true, Ordering::Release);
    }

    let mut product_shell = if cli.smoke_test {
        None
    } else {
        let route = if settings.startup.onboarding_complete {
            UiRoute::Home
        } else {
            UiRoute::Settings
        };
        Some(
            ProductShell::start(
                settings_store.clone(),
                database_path.clone(),
                route,
                UiRuntimeStatus::Ready,
            )
            .map_err(|error| anyhow!(error))
            .context("failed to start the product shell")?,
        )
    };
    let mut settings_window = None;
    let mut history_window = None;
    let mut lexicon_window = None;
    let mut profile_window = None;
    let mut model_download = None;
    let mut restart_requested = false;
    let mut last_shell_status = UiRuntimeStatus::Ready;

    let run_result: Result<()> = 'event_loop: loop {
        if shutting_down.load(Ordering::Acquire) {
            break Ok(());
        }
        match poll_shell_events(
            &tray,
            &overlay,
            ShellPoll {
                product_shell: product_shell.as_ref(),
                settings_window: &mut settings_window,
                history_window: &mut history_window,
                lexicon_window: &mut lexicon_window,
                profile_window: &mut profile_window,
                model_download: &mut model_download,
                settings_store: &settings_store,
                settings: &mut settings,
                effective_model: &model,
                persistence: Some(&persistence),
            },
        ) {
            Ok(ShellAction::Quit) => break Ok(()),
            Ok(ShellAction::Restart) => {
                restart_requested = true;
                break Ok(());
            }
            Ok(ShellAction::Resume) => {
                if let Err(error) = recover_runtime_after_resume(
                    &mut runtime,
                    &overlay,
                    &tray,
                    &worker,
                    effective_microphone.as_deref(),
                    &dictation_context,
                ) {
                    break Err(error);
                }
                continue 'event_loop;
            }
            Ok(ShellAction::TestDictation) => {
                handle_test_dictation(
                    &mut runtime,
                    &overlay,
                    &tray,
                    &worker,
                    effective_microphone.as_deref(),
                    &mut dictation_context,
                    &settings,
                )?;
            }
            Ok(ShellAction::ReloadAliases) => {
                worker.reload_aliases(
                    persistence
                        .lexicon()
                        .list()
                        .context("failed to reload the personal lexicon")?,
                )?;
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
            ShellPoll {
                product_shell: product_shell.as_ref(),
                settings_window: &mut settings_window,
                history_window: &mut history_window,
                lexicon_window: &mut lexicon_window,
                profile_window: &mut profile_window,
                model_download: &mut model_download,
                settings_store: &settings_store,
                settings: &mut settings,
                effective_model: &model,
                persistence: Some(&persistence),
            },
        ) {
            Ok(ShellAction::Quit) => break Ok(()),
            Ok(ShellAction::Restart) => {
                restart_requested = true;
                break Ok(());
            }
            Ok(ShellAction::Resume) => {
                if let Err(error) = recover_runtime_after_resume(
                    &mut runtime,
                    &overlay,
                    &tray,
                    &worker,
                    effective_microphone.as_deref(),
                    &dictation_context,
                ) {
                    break Err(error);
                }
                continue 'event_loop;
            }
            Ok(ShellAction::TestDictation) => {
                handle_test_dictation(
                    &mut runtime,
                    &overlay,
                    &tray,
                    &worker,
                    effective_microphone.as_deref(),
                    &mut dictation_context,
                    &settings,
                )?;
            }
            Ok(ShellAction::ReloadAliases) => {
                worker.reload_aliases(
                    persistence
                        .lexicon()
                        .list()
                        .context("failed to reload the personal lexicon")?,
                )?;
            }
            Ok(ShellAction::Continue) => {}
            Err(error) => break Err(error),
        }

        match hotkey_event {
            Ok(HoldEvent::Started {
                target: activation_target,
            }) => {
                if runtime.state() == RuntimeState::Idle {
                    dictation_context =
                        DictationContext::for_target(activation_target, &persistence, &settings)?;
                    if dictation_context.deny {
                        eprintln!(
                            "dictation_id=0 state=Idle event=activation_denied_by_app_profile"
                        );
                        continue 'event_loop;
                    }
                    runtime.configure_next_dictation(
                        dictation_context.language.clone(),
                        dictation_context.runtime_formatting,
                    )?;
                }
                let notices = {
                    let mut io = ProductionIo {
                        overlay: &overlay,
                        tray: &tray,
                        worker: &worker,
                        microphone: effective_microphone.as_deref(),
                        context: &dictation_context,
                    };
                    let action = if settings.interaction.recording_mode == RecordingMode::Toggle
                        && runtime.state() == RuntimeState::Listening
                    {
                        runtime.hold_ended(&mut io)
                    } else {
                        runtime.hold_started(activation_target, &mut io)
                    };
                    match action {
                        Ok(notices) => notices,
                        Err(error) => break 'event_loop Err(error.into()),
                    }
                };
                report_notices(notices);
            }
            Ok(HoldEvent::Ended) => {
                if settings.interaction.recording_mode == RecordingMode::Toggle {
                    continue 'event_loop;
                }
                let notices = {
                    let mut io = ProductionIo {
                        overlay: &overlay,
                        tray: &tray,
                        worker: &worker,
                        microphone: effective_microphone.as_deref(),
                        context: &dictation_context,
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
            ShellPoll {
                product_shell: product_shell.as_ref(),
                settings_window: &mut settings_window,
                history_window: &mut history_window,
                lexicon_window: &mut lexicon_window,
                profile_window: &mut profile_window,
                model_download: &mut model_download,
                settings_store: &settings_store,
                settings: &mut settings,
                effective_model: &model,
                persistence: Some(&persistence),
            },
        ) {
            Ok(ShellAction::Quit) => break Ok(()),
            Ok(ShellAction::Restart) => {
                restart_requested = true;
                break Ok(());
            }
            Ok(ShellAction::Resume) => {
                if let Err(error) = recover_runtime_after_resume(
                    &mut runtime,
                    &overlay,
                    &tray,
                    &worker,
                    effective_microphone.as_deref(),
                    &dictation_context,
                ) {
                    break Err(error);
                }
                continue 'event_loop;
            }
            Ok(ShellAction::TestDictation) => {
                handle_test_dictation(
                    &mut runtime,
                    &overlay,
                    &tray,
                    &worker,
                    effective_microphone.as_deref(),
                    &mut dictation_context,
                    &settings,
                )?;
            }
            Ok(ShellAction::ReloadAliases) => {
                worker.reload_aliases(
                    persistence
                        .lexicon()
                        .list()
                        .context("failed to reload the personal lexicon")?,
                )?;
            }
            Ok(ShellAction::Continue) => {}
            Err(error) => break Err(error),
        }

        loop {
            match worker.results.try_recv() {
                Ok(WorkerEvent::CleanupStarted { id }) => {
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            tray: &tray,
                            worker: &worker,
                            microphone: effective_microphone.as_deref(),
                            context: &dictation_context,
                        };
                        match runtime.cleanup_started(id, &mut io) {
                            Ok(notices) => notices,
                            Err(error) => break 'event_loop Err(error.into()),
                        }
                    };
                    report_notices(notices);
                }
                Ok(WorkerEvent::Completed(completed)) => {
                    let completed = *completed;
                    let result = completed.result.map(|processed| {
                        if let Err(error) = persist_transcript(&persistence, &processed) {
                            eprintln!(
                                "dictation_id={} state=Cleaning event=history_write_failed error={error}",
                                completed.id.0
                            );
                        }
                        processed.transcript
                    });
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            tray: &tray,
                            worker: &worker,
                            microphone: effective_microphone.as_deref(),
                            context: &dictation_context,
                        };
                        match runtime.transcription_completed(completed.id, result, &mut io) {
                            Ok(notices) => notices,
                            Err(error) => break 'event_loop Err(error.into()),
                        }
                    };
                    report_notices(notices);
                    if let Some(shell) = product_shell.as_ref() {
                        let _ = shell.send(ProductShellControl::Refresh);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            tray: &tray,
                            worker: &worker,
                            microphone: effective_microphone.as_deref(),
                            context: &dictation_context,
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
        let shell_status = runtime_shell_status(runtime.state());
        if shell_status != last_shell_status {
            if let Some(shell) = product_shell.as_ref() {
                let _ = shell.send(ProductShellControl::SetRuntimeStatus(shell_status));
            }
            last_shell_status = shell_status;
        }
    };

    println!("Shutting down Phorminx.");
    let mut final_error = run_result.err();
    drop(runtime);
    if let Some(shell) = product_shell.take() {
        preserve_first_error(
            &mut final_error,
            shell.shutdown().map_err(|error| anyhow!(error)),
        );
    }
    if let Some(window) = history_window {
        preserve_first_error(
            &mut final_error,
            window
                .shutdown()
                .context("failed to stop the history window"),
        );
    }
    if let Some(window) = lexicon_window {
        preserve_first_error(
            &mut final_error,
            window
                .shutdown()
                .context("failed to stop the lexicon window"),
        );
    }
    if let Some(window) = profile_window {
        preserve_first_error(
            &mut final_error,
            window
                .shutdown()
                .context("failed to stop the profile window"),
        );
    }
    if let Some(window) = settings_window {
        preserve_first_error(
            &mut final_error,
            window
                .shutdown()
                .context("failed to stop the settings window"),
        );
    }
    if let Some(download) = model_download {
        preserve_first_error(
            &mut final_error,
            download
                .shutdown()
                .context("failed to stop the model download"),
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

fn runtime_shell_status(state: RuntimeState) -> UiRuntimeStatus {
    match state {
        RuntimeState::Starting => UiRuntimeStatus::Starting,
        RuntimeState::Idle | RuntimeState::Cancelled => UiRuntimeStatus::Ready,
        RuntimeState::Listening | RuntimeState::FinalizingAudio => UiRuntimeStatus::Listening,
        RuntimeState::Transcribing | RuntimeState::Normalizing => UiRuntimeStatus::Transcribing,
        RuntimeState::Cleaning | RuntimeState::ReadyToInsert => UiRuntimeStatus::Refining,
        RuntimeState::Inserting => UiRuntimeStatus::Inserted,
        RuntimeState::Faulted => UiRuntimeStatus::NeedsAttention,
    }
}

fn run_setup_mode(
    overlay: StatusOverlay,
    tray: SystemTray,
    settings_store: SettingsStore,
    mut settings: Settings,
    model: PathBuf,
    shutting_down: Arc<AtomicBool>,
) -> Result<()> {
    show_shell_status(&overlay, &tray, OverlayStatus::Error, TrayStatus::Error);
    let database_path = settings_store
        .path()
        .parent()
        .context("settings path has no parent directory")?
        .join("phorminx.db");
    let mut product_shell = Some(
        ProductShell::start(
            settings_store.clone(),
            database_path,
            UiRoute::Models,
            UiRuntimeStatus::NeedsAttention,
        )
        .map_err(|error| anyhow!(error))
        .context("failed to open first-run model setup")?,
    );
    let mut settings_window = None;
    let mut history_window = None;
    let mut lexicon_window = None;
    let mut profile_window = None;
    let mut model_download = None;
    let mut restart_requested = false;
    let run_result = loop {
        if shutting_down.load(Ordering::Acquire) {
            break Ok(());
        }
        match poll_shell_events(
            &tray,
            &overlay,
            ShellPoll {
                product_shell: product_shell.as_ref(),
                settings_window: &mut settings_window,
                history_window: &mut history_window,
                lexicon_window: &mut lexicon_window,
                profile_window: &mut profile_window,
                model_download: &mut model_download,
                settings_store: &settings_store,
                settings: &mut settings,
                effective_model: &model,
                persistence: None,
            },
        ) {
            Ok(ShellAction::Quit) => break Ok(()),
            Ok(ShellAction::Restart) => {
                restart_requested = true;
                break Ok(());
            }
            Ok(
                ShellAction::Continue
                | ShellAction::Resume
                | ShellAction::TestDictation
                | ShellAction::ReloadAliases,
            ) => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => break Err(error),
        }
    };

    let mut final_error = run_result.err();
    if let Some(shell) = product_shell.take() {
        preserve_first_error(
            &mut final_error,
            shell.shutdown().map_err(|error| anyhow!(error)),
        );
    }
    if let Some(window) = history_window {
        preserve_first_error(
            &mut final_error,
            window
                .shutdown()
                .context("failed to stop the history window"),
        );
    }
    if let Some(window) = lexicon_window {
        preserve_first_error(
            &mut final_error,
            window
                .shutdown()
                .context("failed to stop the lexicon window"),
        );
    }
    if let Some(window) = profile_window {
        preserve_first_error(
            &mut final_error,
            window
                .shutdown()
                .context("failed to stop the profile window"),
        );
    }
    if let Some(window) = settings_window {
        preserve_first_error(
            &mut final_error,
            window.shutdown().context("failed to stop the setup window"),
        );
    }
    if let Some(download) = model_download {
        preserve_first_error(
            &mut final_error,
            download
                .shutdown()
                .context("failed to stop the model download"),
        );
    }
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
    Resume,
    TestDictation,
    ReloadAliases,
}

struct ShellPoll<'a> {
    product_shell: Option<&'a ProductShell>,
    settings_window: &'a mut Option<SettingsWindow>,
    history_window: &'a mut Option<HistoryWindow>,
    lexicon_window: &'a mut Option<LexiconWindow>,
    profile_window: &'a mut Option<ProfileWindow>,
    model_download: &'a mut Option<ModelDownload>,
    settings_store: &'a SettingsStore,
    settings: &'a mut Settings,
    effective_model: &'a Path,
    persistence: Option<&'a Persistence>,
}

fn poll_shell_events(
    tray: &SystemTray,
    overlay: &StatusOverlay,
    poll: ShellPoll<'_>,
) -> Result<ShellAction> {
    let ShellPoll {
        product_shell,
        settings_window,
        history_window,
        lexicon_window,
        profile_window,
        model_download,
        settings_store,
        settings,
        effective_model,
        persistence,
    } = poll;
    loop {
        match tray.events().try_recv() {
            Ok(TrayEvent::QuitRequested) => return Ok(ShellAction::Quit),
            Ok(TrayEvent::SystemResumed) => return Ok(ShellAction::Resume),
            Ok(TrayEvent::OpenSettings) => {
                if let Some(shell) = product_shell {
                    shell
                        .focus(UiRoute::Settings)
                        .map_err(|error| anyhow!(error))?;
                    continue;
                }
                if let Some(window) = settings_window {
                    window.focus().context("failed to focus settings")?;
                } else {
                    *settings_window = Some(
                        SettingsWindow::start(settings_form(settings, effective_model))
                            .context("failed to open settings")?,
                    );
                }
            }
            Ok(TrayEvent::OpenHistory) => {
                if let Some(shell) = product_shell {
                    shell
                        .focus(UiRoute::History)
                        .map_err(|error| anyhow!(error))?;
                    continue;
                }
                let Some(persistence) = persistence else {
                    show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                    continue;
                };
                if let Some(window) = history_window {
                    window.focus().context("failed to focus history")?;
                } else {
                    let items = persistence
                        .history()
                        .recent(100)
                        .context("failed to load local history")?
                        .into_iter()
                        .map(|record| HistoryItem {
                            id: record.id,
                            created_label: format!(
                                "record {} · {}",
                                record.id, record.dictation.created_at_ms
                            ),
                            raw: record.dictation.raw_text,
                            normalized: record.dictation.normalized_text,
                            cleaned: record.dictation.cleaned_text,
                            selected: record.dictation.selected_output,
                        })
                        .collect();
                    *history_window =
                        Some(HistoryWindow::start(items).context("failed to open history")?);
                }
            }
            Ok(TrayEvent::OpenLexicon) => {
                if let Some(shell) = product_shell {
                    shell
                        .focus(UiRoute::Lexicon)
                        .map_err(|error| anyhow!(error))?;
                    continue;
                }
                let Some(persistence) = persistence else {
                    show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                    continue;
                };
                if let Some(window) = lexicon_window {
                    window.focus().context("failed to focus personal lexicon")?;
                } else {
                    let items = persistence
                        .lexicon()
                        .list()
                        .context("failed to load the personal lexicon")?
                        .into_iter()
                        .map(|entry| LexiconItem {
                            id: entry.id,
                            canonical: entry.entry.canonical,
                            alias: entry.entry.alias,
                            language: entry.entry.language,
                            app_executable: entry.entry.app_executable,
                            case_policy: to_window_case_policy(entry.entry.case_policy),
                            enabled: entry.entry.enabled,
                        })
                        .collect();
                    *lexicon_window = Some(
                        LexiconWindow::start(items)
                            .context("failed to open the personal lexicon")?,
                    );
                }
            }
            Ok(TrayEvent::OpenProfiles) => {
                if let Some(shell) = product_shell {
                    shell
                        .focus(UiRoute::Profiles)
                        .map_err(|error| anyhow!(error))?;
                    continue;
                }
                let Some(persistence) = persistence else {
                    show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                    continue;
                };
                if let Some(window) = profile_window {
                    window
                        .focus()
                        .context("failed to focus application profiles")?;
                } else {
                    let items = persistence
                        .app_profiles()
                        .list()
                        .context("failed to load application profiles")?
                        .into_iter()
                        .map(profile_item)
                        .collect();
                    *profile_window = Some(
                        ProfileWindow::start(items)
                            .context("failed to open application profiles")?,
                    );
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                return Err(anyhow!("the system tray thread stopped unexpectedly"));
            }
        }
    }

    if let Some(shell) = product_shell {
        loop {
            match shell.events().try_recv() {
                Ok(ProductShellEvent::TestDictation) => {
                    return Ok(ShellAction::TestDictation);
                }
                Ok(ProductShellEvent::ChangeWhisperModel) => {
                    if model_download.is_none() {
                        let directory = settings_store
                            .path()
                            .parent()
                            .context("the settings path has no parent directory")?
                            .join("models");
                        match ModelDownload::start(&directory) {
                            Ok(download) => *model_download = Some(download),
                            Err(error) => {
                                eprintln!("model_download_failed error={error}");
                                let _ = shell.send(ProductShellControl::ModelDownloadFailed);
                            }
                        }
                    }
                }
                Ok(ProductShellEvent::RuntimeReloadRequested(mutation)) => match mutation {
                    UiMutation::SettingsSaved => return Ok(ShellAction::Restart),
                    UiMutation::LexiconSaved { .. }
                    | UiMutation::LexiconDeleted { .. }
                    | UiMutation::LexiconEnabled { .. } => {
                        return Ok(ShellAction::ReloadAliases);
                    }
                    UiMutation::HistoryCleared { .. }
                    | UiMutation::ProfileSaved { .. }
                    | UiMutation::ProfileDeleted { .. } => {}
                },
                Ok(ProductShellEvent::ApplyLaunchAtLogin(enabled)) => {
                    let executable = std::env::current_exe()
                        .context("failed to resolve the Phorminx executable")?;
                    if let Err(error) = set_launch_at_login(&executable, enabled) {
                        eprintln!("launch_at_login_update_failed error={error}");
                        show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                    }
                }
                Ok(ProductShellEvent::Hidden) => {}
                Ok(ProductShellEvent::Failed(message)) => {
                    eprintln!("product_shell_failed error={message}");
                    show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    let mut history_events = Vec::new();
    if let Some(window) = history_window.as_ref() {
        loop {
            match window.events().try_recv() {
                Ok(event) => history_events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !history_events
                        .iter()
                        .any(|event| matches!(event, HistoryWindowEvent::Closed))
                    {
                        history_events.push(HistoryWindowEvent::Closed);
                    }
                    break;
                }
            }
        }
    }
    for event in history_events {
        match event {
            HistoryWindowEvent::CopyRaw(text) | HistoryWindowEvent::CopySelected(text) => {
                copy_and_maybe_paste(None, &text)
                    .context("failed to copy the recovered transcript")?;
                show_shell_status(
                    overlay,
                    tray,
                    OverlayStatus::ClipboardReady,
                    TrayStatus::Ready,
                );
            }
            HistoryWindowEvent::ClearRequested => {
                if let Some(persistence) = persistence {
                    persistence
                        .history()
                        .clear()
                        .context("failed to clear local history")?;
                }
                if let Some(window) = history_window.take() {
                    window
                        .shutdown()
                        .context("failed to close cleared history")?;
                }
            }
            HistoryWindowEvent::Closed => {
                if let Some(window) = history_window.take() {
                    window
                        .shutdown()
                        .context("failed to join the history window")?;
                }
            }
        }
    }

    let mut profile_events = Vec::new();
    if let Some(window) = profile_window.as_ref() {
        loop {
            match window.events().try_recv() {
                Ok(event) => profile_events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !profile_events
                        .iter()
                        .any(|event| matches!(event, ProfileWindowEvent::Closed))
                    {
                        profile_events.push(ProfileWindowEvent::Closed);
                    }
                    break;
                }
            }
        }
    }
    for event in profile_events {
        match event {
            ProfileWindowEvent::Save(item) => {
                let Some(persistence) = persistence else {
                    continue;
                };
                match app_profile(item).and_then(|profile| {
                    persistence
                        .app_profiles()
                        .upsert(&profile)
                        .context("application profile was rejected")
                }) {
                    Ok(()) => return Ok(ShellAction::Restart),
                    Err(error) => {
                        eprintln!(
                            "dictation_id=0 state=Idle event=profile_save_rejected error={error}"
                        );
                        show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                    }
                }
            }
            ProfileWindowEvent::Delete(executable) => {
                let result = ExecutableIdentity::new(executable)
                    .context("invalid application profile executable")
                    .and_then(|identity| {
                        persistence
                            .context("application profiles are unavailable")?
                            .app_profiles()
                            .delete(&identity)
                            .context("failed to delete application profile")
                    });
                match result {
                    Ok(_) => return Ok(ShellAction::Restart),
                    Err(error) => {
                        eprintln!(
                            "dictation_id=0 state=Idle event=profile_delete_failed error={error}"
                        );
                        show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                    }
                }
            }
            ProfileWindowEvent::Closed => {
                if let Some(window) = profile_window.take() {
                    window
                        .shutdown()
                        .context("failed to join the profile window")?;
                }
            }
        }
    }

    let mut lexicon_events = Vec::new();
    if let Some(window) = lexicon_window.as_ref() {
        loop {
            match window.events().try_recv() {
                Ok(event) => lexicon_events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !lexicon_events
                        .iter()
                        .any(|event| matches!(event, LexiconWindowEvent::Closed))
                    {
                        lexicon_events.push(LexiconWindowEvent::Closed);
                    }
                    break;
                }
            }
        }
    }
    for event in lexicon_events {
        match event {
            LexiconWindowEvent::Save(draft) => {
                let Some(persistence) = persistence else {
                    continue;
                };
                let entry = lexicon_entry(draft);
                let result = if let Some(id) = entry.0 {
                    persistence.lexicon().update(id, &entry.1).map(|_| ())
                } else {
                    persistence.lexicon().insert(&entry.1).map(|_| ())
                };
                if let Err(error) = result {
                    eprintln!(
                        "dictation_id=0 state=Idle event=lexicon_save_rejected error={error}"
                    );
                    show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                } else {
                    return Ok(ShellAction::Restart);
                }
            }
            LexiconWindowEvent::Delete(id) => {
                if let Some(persistence) = persistence {
                    match persistence.lexicon().delete(id) {
                        Ok(_) => return Ok(ShellAction::Restart),
                        Err(error) => {
                            eprintln!(
                                "dictation_id=0 state=Idle event=lexicon_delete_failed error={error}"
                            );
                            show_shell_status(
                                overlay,
                                tray,
                                OverlayStatus::Error,
                                TrayStatus::Error,
                            );
                        }
                    }
                }
            }
            LexiconWindowEvent::Closed => {
                if let Some(window) = lexicon_window.take() {
                    window
                        .shutdown()
                        .context("failed to join the lexicon window")?;
                }
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
                if let Some(download) = model_download.take() {
                    download
                        .shutdown()
                        .context("failed to cancel the model download")?;
                }
            }
            SettingsWindowEvent::SaveAndRestart(form) => {
                let save_result = apply_settings_form(settings_store, settings, *form);
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
            SettingsWindowEvent::DownloadRecommended => {
                if model_download.is_some() {
                    if let Some(window) = settings_window.as_ref() {
                        window
                            .update_model(None, "Model download is already running".to_owned())
                            .context("failed to update download status")?;
                    }
                    continue;
                }
                let directory = settings_store
                    .path()
                    .parent()
                    .ok_or_else(|| anyhow!("the settings path has no parent directory"))?
                    .join("models");
                match ModelDownload::start(&directory) {
                    Ok(download) => {
                        *model_download = Some(download);
                        if let Some(window) = settings_window.as_ref() {
                            window
                                .update_model(
                                    None,
                                    "Starting verified model download...".to_owned(),
                                )
                                .context("failed to update download status")?;
                        }
                    }
                    Err(error) => {
                        if let Some(window) = settings_window.as_ref() {
                            window
                                .show_error(format!("Could not start the model download: {error}"))
                                .context("failed to display the download error")?;
                        }
                    }
                }
            }
        }
    }

    let mut download_events = Vec::new();
    if let Some(download) = model_download.as_ref() {
        loop {
            match download.events().try_recv() {
                Ok(event) => download_events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !download_events.iter().any(|event| {
                        matches!(
                            event,
                            ModelDownloadEvent::Completed { .. }
                                | ModelDownloadEvent::Cancelled
                                | ModelDownloadEvent::Failed(_)
                        )
                    }) {
                        download_events.push(ModelDownloadEvent::Failed(
                            "the model download thread stopped unexpectedly".to_owned(),
                        ));
                    }
                    break;
                }
            }
        }
    }

    let mut download_finished = false;
    for event in download_events {
        match event {
            ModelDownloadEvent::Progress { downloaded, total } => {
                let percent = downloaded.saturating_mul(100) / total.max(1);
                if let Some(shell) = product_shell {
                    let _ = shell.send(ProductShellControl::ModelDownloadProgress(percent));
                }
                if let Some(window) = settings_window.as_ref() {
                    window
                        .update_model(
                            None,
                            format!("Downloading and verifying model... {percent}%"),
                        )
                        .context("failed to update download progress")?;
                }
            }
            ModelDownloadEvent::Completed { path } => {
                if let Some(shell) = product_shell {
                    let _ = shell.send(ProductShellControl::ModelDownloaded(path.clone()));
                }
                if let Some(window) = settings_window.as_ref() {
                    let size = std::fs::metadata(&path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
                    window
                        .update_model(
                            Some(path.display().to_string()),
                            format!(
                                "Model downloaded and verified ({:.1} MiB)",
                                size as f64 / (1024.0 * 1024.0)
                            ),
                        )
                        .context("failed to show the downloaded model")?;
                }
                download_finished = true;
            }
            ModelDownloadEvent::Cancelled => {
                if let Some(shell) = product_shell {
                    let _ = shell.send(ProductShellControl::ModelDownloadFailed);
                }
                if let Some(window) = settings_window.as_ref() {
                    window
                        .update_model(None, "Model download cancelled".to_owned())
                        .context("failed to update download status")?;
                }
                download_finished = true;
            }
            ModelDownloadEvent::Failed(message) => {
                if let Some(shell) = product_shell {
                    let _ = shell.send(ProductShellControl::ModelDownloadFailed);
                }
                if let Some(window) = settings_window.as_ref() {
                    window
                        .update_model(None, "Model download failed".to_owned())
                        .context("failed to update download status")?;
                    window
                        .show_error(message)
                        .context("failed to display the download failure")?;
                }
                download_finished = true;
            }
        }
    }
    if download_finished && let Some(download) = model_download.take() {
        download
            .shutdown()
            .context("failed to join the model download")?;
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
    let (microphones, microphone_status) = match input_devices() {
        Ok(devices) => {
            let selected_available = settings
                .recognition
                .microphone
                .as_ref()
                .is_none_or(|name| devices.iter().any(|device| &device.name == name));
            let status = if !selected_available {
                "Saved microphone is unavailable; dictation will recover to the Windows default"
                    .to_owned()
            } else if let Some(selected) = &settings.recognition.microphone {
                format!("Selected microphone ready: {selected}")
            } else {
                devices
                    .iter()
                    .find(|device| device.is_default)
                    .map(|device| format!("Windows default: {}", device.name))
                    .unwrap_or_else(|| {
                        if devices.is_empty() {
                            "No microphone input devices were found".to_owned()
                        } else {
                            format!(
                                "{} microphone(s) found; Windows has no default",
                                devices.len()
                            )
                        }
                    })
            };
            (
                devices.into_iter().map(|device| device.name).collect(),
                status,
            )
        }
        Err(error) => (Vec::new(), format!("Microphone check failed: {error}")),
    };
    let cancel = CancellationToken::new();
    let (mut ollama_models, ollama_status) = match production_ollama_client().discover(&cancel) {
        Ok(catalog) => {
            let names = catalog
                .models()
                .iter()
                .map(|model| model.name.to_string())
                .collect::<Vec<_>>();
            let status = if names.is_empty() {
                "Ollama is running but has no installed models".to_owned()
            } else {
                format!("Ollama ready: {} installed model(s)", names.len())
            };
            (names, status)
        }
        Err(error) => (
            Vec::new(),
            format!("Ollama unavailable; deterministic formatting remains ready ({error})"),
        ),
    };
    if let Some(selected) = &settings.formatting.ollama_model
        && !ollama_models.contains(selected)
    {
        ollama_models.push(selected.clone());
    }
    SettingsForm {
        model_path: effective_model.display().to_string(),
        model_status,
        microphone_status,
        microphones,
        microphone: settings.recognition.microphone.clone(),
        recommended_download_label: recommended_model()
            .map(|model| {
                format!(
                    "Download recommended English model ({:.0} MiB)",
                    model.bytes as f64 / (1024.0 * 1024.0)
                )
            })
            .unwrap_or_else(|_| "Recommended model download unavailable".to_owned()),
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
        ollama_models,
        ollama_model: settings.formatting.ollama_model.clone(),
        ollama_status,
        ollama_lifecycle: match settings.formatting.ollama_lifecycle {
            OllamaLifecycle::Instant => SettingsOllamaLifecycle::Instant,
            OllamaLifecycle::Balanced => SettingsOllamaLifecycle::Balanced,
            OllamaLifecycle::MemorySaver => SettingsOllamaLifecycle::MemorySaver,
        },
        recording_mode: match settings.interaction.recording_mode {
            RecordingMode::Hold => SettingsRecordingMode::Hold,
            RecordingMode::Toggle => SettingsRecordingMode::Toggle,
        },
        history_retention: match settings.privacy.history_retention {
            HistoryRetention::Disabled => SettingsHistoryRetention::Disabled,
            HistoryRetention::OneDay => SettingsHistoryRetention::OneDay,
            HistoryRetention::SevenDays => SettingsHistoryRetention::SevenDays,
            HistoryRetention::ThirtyDays => SettingsHistoryRetention::ThirtyDays,
            HistoryRetention::Indefinite => SettingsHistoryRetention::Indefinite,
        },
        launch_at_login: settings.startup.launch_at_login,
    }
}

fn to_window_case_policy(policy: CasePolicy) -> LexiconCasePolicy {
    match policy {
        CasePolicy::PreserveInput => LexiconCasePolicy::PreserveInput,
        CasePolicy::UseCanonical => LexiconCasePolicy::UseCanonical,
        CasePolicy::Lowercase => LexiconCasePolicy::Lowercase,
        CasePolicy::Uppercase => LexiconCasePolicy::Uppercase,
    }
}

fn lexicon_entry(draft: LexiconDraft) -> (Option<i64>, NewLexiconEntry) {
    let case_policy = match draft.case_policy {
        LexiconCasePolicy::PreserveInput => CasePolicy::PreserveInput,
        LexiconCasePolicy::UseCanonical => CasePolicy::UseCanonical,
        LexiconCasePolicy::Lowercase => CasePolicy::Lowercase,
        LexiconCasePolicy::Uppercase => CasePolicy::Uppercase,
    };
    (
        draft.id,
        NewLexiconEntry {
            canonical: draft.canonical,
            alias: draft.alias,
            language: draft.language,
            app_executable: draft.app_executable,
            case_policy,
            enabled: draft.enabled,
        },
    )
}

fn profile_item(profile: AppProfile) -> ProfileItem {
    ProfileItem {
        executable: profile.executable.to_string(),
        formatting: match profile.formatting_style {
            FormattingStyle::Raw => ProfileFormatting::Raw,
            FormattingStyle::Light => ProfileFormatting::Light,
            FormattingStyle::Balanced => ProfileFormatting::Balanced,
            FormattingStyle::Strong => ProfileFormatting::Strong,
            FormattingStyle::Custom => ProfileFormatting::Custom,
        },
        custom_instructions: profile.custom_instructions,
        language: profile.language,
        insertion: match profile.insertion_preference {
            InsertionPreference::Automatic => ProfileInsertion::Automatic,
            InsertionPreference::Direct => ProfileInsertion::Direct,
            InsertionPreference::Clipboard => ProfileInsertion::Clipboard,
        },
        deny: profile.deny,
    }
}

fn app_profile(item: ProfileItem) -> Result<AppProfile> {
    Ok(AppProfile {
        executable: ExecutableIdentity::new(item.executable)
            .context("Executable must be a basename such as code.exe")?,
        formatting_style: match item.formatting {
            ProfileFormatting::Raw => FormattingStyle::Raw,
            ProfileFormatting::Light => FormattingStyle::Light,
            ProfileFormatting::Balanced => FormattingStyle::Balanced,
            ProfileFormatting::Strong => FormattingStyle::Strong,
            ProfileFormatting::Custom => FormattingStyle::Custom,
        },
        custom_instructions: item.custom_instructions,
        language: item.language,
        insertion_preference: match item.insertion {
            ProfileInsertion::Automatic => InsertionPreference::Automatic,
            ProfileInsertion::Direct => InsertionPreference::Direct,
            ProfileInsertion::Clipboard => InsertionPreference::Clipboard,
        },
        deny: item.deny,
    })
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
    candidate.recognition.microphone = form.microphone;
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
    candidate.formatting.ollama_model = form.ollama_model;
    candidate.formatting.ollama_lifecycle = match form.ollama_lifecycle {
        SettingsOllamaLifecycle::Instant => OllamaLifecycle::Instant,
        SettingsOllamaLifecycle::Balanced => OllamaLifecycle::Balanced,
        SettingsOllamaLifecycle::MemorySaver => OllamaLifecycle::MemorySaver,
    };
    candidate.interaction.recording_mode = match form.recording_mode {
        SettingsRecordingMode::Hold => RecordingMode::Hold,
        SettingsRecordingMode::Toggle => RecordingMode::Toggle,
    };
    candidate.privacy.history_retention = match form.history_retention {
        SettingsHistoryRetention::Disabled => HistoryRetention::Disabled,
        SettingsHistoryRetention::OneDay => HistoryRetention::OneDay,
        SettingsHistoryRetention::SevenDays => HistoryRetention::SevenDays,
        SettingsHistoryRetention::ThirtyDays => HistoryRetention::ThirtyDays,
        SettingsHistoryRetention::Indefinite => HistoryRetention::Indefinite,
    };
    candidate.startup.launch_at_login = form.launch_at_login;
    candidate.startup.onboarding_complete = true;
    candidate
        .validate_and_normalize()
        .context("The settings are not valid")?;
    candidate
        .ensure_runtime_supported()
        .context("This formatting profile is not ready")?;
    if matches!(
        candidate.formatting.strength,
        FormattingStrength::Balanced | FormattingStrength::Strong | FormattingStrength::Custom
    ) {
        let selected = candidate
            .formatting
            .ollama_model
            .as_ref()
            .context("Select an installed local Ollama model")?;
        let policy = SelectionPolicy::exact(selected.clone())
            .context("The selected Ollama model name is invalid")?;
        let catalog = production_ollama_client()
            .discover(&CancellationToken::new())
            .context("Could not verify the selected model with local Ollama")?;
        catalog
            .select(&policy)
            .context("The selected Ollama model is not installed")?;
    }
    let resolved_model = store.resolve_model_path(&candidate.recognition.model_path);
    if !resolved_model.is_file() {
        return Err(anyhow!(
            "The selected Whisper model does not exist or is not a file: {}",
            resolved_model.display()
        ));
    }
    store.save(&candidate).context("Could not write settings")?;
    if let Err(error) = std::env::current_exe()
        .context("Could not locate Phorminx for launch-at-login")
        .and_then(|executable| {
            set_launch_at_login(&executable, candidate.startup.launch_at_login)
                .context("Could not update launch-at-login")
        })
    {
        let _ = store.save(settings);
        return Err(error);
    }
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
    microphone: Option<&'a str>,
    context: &'a DictationContext,
}

impl AppIo for ProductionIo<'_> {
    type Target = TargetSnapshot;
    type Recording = ActiveRecording;
    type ClipboardReason = ClipboardOnlyReason;

    fn start_recording(&mut self) -> Result<Self::Recording, String> {
        match start_input(self.microphone) {
            Ok(recording) => Ok(recording),
            Err(selected_error) if self.microphone.is_some() => {
                eprintln!(
                    "dictation_id=0 state=Listening event=selected_microphone_failed recovery=windows_default"
                );
                start_input(None).map_err(|fallback_error| {
                    format!(
                        "selected microphone failed ({selected_error}); Windows default recovery failed ({fallback_error})"
                    )
                })
            }
            Err(error) => Err(error.to_string()),
        }
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
            .transcribe(
                id,
                clip,
                language.to_owned(),
                audio_context,
                self.context.formatting.clone(),
                self.context.app_executable.clone(),
            )
            .map_err(|error| error.to_string())
    }

    fn insert(
        &mut self,
        target: Option<Self::Target>,
        text: &str,
    ) -> Result<InsertDisposition<Self::ClipboardReason>, String> {
        let target = if self.context.insertion_preference == InsertionPreference::Clipboard {
            None
        } else {
            target
        };
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
            UiStatus::Cleaning => (OverlayStatus::Cleaning, TrayStatus::Cleaning),
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

fn recover_runtime_after_resume(
    runtime: &mut AppRuntime<TargetSnapshot, ActiveRecording>,
    overlay: &StatusOverlay,
    tray: &SystemTray,
    worker: &TranscriptionWorker,
    microphone: Option<&str>,
    context: &DictationContext,
) -> Result<()> {
    let mut io = ProductionIo {
        overlay,
        tray,
        worker,
        microphone,
        context,
    };
    let notices = runtime
        .recover_after_system_resume(&mut io)
        .context("failed to recover dictation after system resume")?;
    report_notices(notices);
    Ok(())
}

fn handle_test_dictation(
    runtime: &mut AppRuntime<TargetSnapshot, ActiveRecording>,
    overlay: &StatusOverlay,
    tray: &SystemTray,
    worker: &TranscriptionWorker,
    microphone: Option<&str>,
    context: &mut DictationContext,
    settings: &Settings,
) -> Result<()> {
    if runtime.state() == RuntimeState::Idle {
        *context = DictationContext::global(settings)?;
        runtime.configure_next_dictation(context.language.clone(), context.runtime_formatting)?;
    }
    let mut io = ProductionIo {
        overlay,
        tray,
        worker,
        microphone,
        context,
    };
    let notices = match runtime.state() {
        RuntimeState::Idle => runtime.hold_started(None, &mut io)?,
        RuntimeState::Listening => runtime.hold_ended(&mut io)?,
        _ => Vec::new(),
    };
    report_notices(notices);
    Ok(())
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
            RuntimeNotice::CleanupStarted { id } => {
                log_state(Some(id), RuntimeState::Cleaning, "cleanup_started");
            }
            RuntimeNotice::RecoveredAfterResume { cancelled_id } => {
                log_state(
                    cancelled_id,
                    RuntimeState::Idle,
                    "recovered_after_system_resume",
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
    results: Receiver<WorkerEvent>,
    thread: Option<JoinHandle<()>>,
}

impl TranscriptionWorker {
    fn start(
        model: &Path,
        formatting: WorkerFormatting,
        aliases: Vec<LexiconEntry>,
    ) -> Result<Self> {
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

                let ollama = formatting.model.as_ref().map(|_| production_ollama_client());
                let mut aliases = aliases;
                if let (Some(client), Some(model)) = (&ollama, &formatting.model) {
                    let cancel = CancellationToken::new();
                    if let Err(error) = client.warm_up(model, formatting.keep_alive.clone(), &cancel)
                    {
                        eprintln!(
                            "dictation_id=0 state=Starting event=ollama_warmup_degraded error={error}"
                        );
                    }
                }

                while let Ok(command) = command_rx.recv() {
                    match command {
                        WorkerCommand::Transcribe {
                            id,
                            clip,
                            language,
                            audio_context,
                            formatting: dictation_formatting,
                            app_executable,
                        } => {
                            let options = TranscriptionOptions {
                                language: Some(&language),
                                thread_count: None,
                                audio_context: Some(audio_context),
                            };
                            let result = match recognizer.transcribe(&clip, &options) {
                                Ok(transcript) => {
                                    if dictation_formatting.uses_ollama()
                                        && result_tx
                                            .send(WorkerEvent::CleanupStarted { id })
                                            .is_err()
                                    {
                                        break;
                                    }
                                    Ok(process_transcript(
                                        transcript,
                                        &language,
                                        &dictation_formatting,
                                        ollama.as_ref(),
                                        &aliases,
                                        app_executable.as_deref(),
                                    ))
                                }
                                Err(error) => Err(error.to_string()),
                            };
                            if result_tx
                                .send(WorkerEvent::Completed(Box::new(WorkerResult { id, result })))
                                .is_err()
                            {
                                break;
                            }
                        }
                        WorkerCommand::ReloadAliases(updated) => aliases = updated,
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
        formatting: WorkerFormatting,
        app_executable: Option<String>,
    ) -> Result<()> {
        self.commands
            .send(WorkerCommand::Transcribe {
                id,
                clip,
                language,
                audio_context,
                formatting,
                app_executable,
            })
            .context("transcription worker is unavailable")
    }

    fn shutdown(mut self) -> Result<()> {
        self.stop()
    }

    fn reload_aliases(&self, aliases: Vec<LexiconEntry>) -> Result<()> {
        self.commands
            .send(WorkerCommand::ReloadAliases(aliases))
            .context("transcription worker is unavailable")
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
        formatting: WorkerFormatting,
        app_executable: Option<String>,
    },
    ReloadAliases(Vec<LexiconEntry>),
    Shutdown,
}

struct WorkerResult {
    id: DictationId,
    result: Result<ProcessedTranscript, String>,
}

enum WorkerEvent {
    CleanupStarted { id: DictationId },
    Completed(Box<WorkerResult>),
}

struct DictationContext {
    app_executable: Option<String>,
    language: String,
    formatting: WorkerFormatting,
    runtime_formatting: RuntimeFormatting,
    insertion_preference: InsertionPreference,
    deny: bool,
}

impl DictationContext {
    fn global(settings: &Settings) -> Result<Self> {
        Ok(Self {
            app_executable: None,
            language: settings.recognition.language.clone(),
            formatting: WorkerFormatting::from_settings(settings)?,
            runtime_formatting: RuntimeFormatting::try_from(settings.formatting.strength)?,
            insertion_preference: InsertionPreference::Automatic,
            deny: false,
        })
    }

    fn for_target(
        target: Option<TargetSnapshot>,
        persistence: &Persistence,
        settings: &Settings,
    ) -> Result<Self> {
        let mut context = Self::global(settings)?;
        let Some(executable) = target.and_then(TargetSnapshot::executable_name) else {
            return Ok(context);
        };
        let identity = ExecutableIdentity::new(executable.clone())
            .context("resolved target executable identity is invalid")?;
        context.app_executable = Some(executable);
        let Some(profile) = persistence
            .app_profiles()
            .get(&identity)
            .context("failed to load the application profile")?
        else {
            return Ok(context);
        };
        context.language = profile.language.unwrap_or(context.language);
        context.insertion_preference = profile.insertion_preference;
        context.deny = profile.deny;
        context.runtime_formatting = match profile.formatting_style {
            FormattingStyle::Raw => RuntimeFormatting::Raw,
            FormattingStyle::Light => RuntimeFormatting::Light,
            FormattingStyle::Balanced => RuntimeFormatting::Balanced,
            FormattingStyle::Strong => RuntimeFormatting::Strong,
            FormattingStyle::Custom => RuntimeFormatting::Custom,
        };
        context.formatting.profile = match profile.formatting_style {
            FormattingStyle::Raw => FormatProfile::Raw,
            FormattingStyle::Light => FormatProfile::Light,
            FormattingStyle::Balanced => FormatProfile::Balanced,
            FormattingStyle::Strong => FormatProfile::Strong,
            FormattingStyle::Custom => FormatProfile::Custom(
                profile
                    .custom_instructions
                    .context("custom application profile has no instructions")?,
            ),
        };
        Ok(context)
    }
}

#[derive(Clone)]
struct WorkerFormatting {
    profile: FormatProfile,
    model: Option<ModelName>,
    keep_alive: KeepAlive,
}

impl WorkerFormatting {
    fn from_settings(settings: &Settings) -> Result<Self> {
        let profile = match settings.formatting.strength {
            FormattingStrength::Raw => FormatProfile::Raw,
            FormattingStrength::Light => FormatProfile::Light,
            FormattingStrength::Balanced => FormatProfile::Balanced,
            FormattingStrength::Strong => FormatProfile::Strong,
            FormattingStrength::Custom => FormatProfile::Custom(
                settings
                    .formatting
                    .custom_instructions
                    .clone()
                    .context("custom formatting instructions are missing")?,
            ),
        };
        let model = settings
            .formatting
            .ollama_model
            .clone()
            .map(ModelName::parse)
            .transpose()
            .context("invalid selected Ollama model")?;
        let keep_alive = match settings.formatting.ollama_lifecycle {
            OllamaLifecycle::Instant => KeepAlive::Indefinite,
            OllamaLifecycle::Balanced => KeepAlive::For(Duration::from_secs(15 * 60)),
            OllamaLifecycle::MemorySaver => KeepAlive::UnloadAfterRequest,
        };
        Ok(Self {
            profile,
            model,
            keep_alive,
        })
    }

    fn uses_ollama(&self) -> bool {
        matches!(
            &self.profile,
            FormatProfile::Balanced | FormatProfile::Strong | FormatProfile::Custom(_)
        )
    }
}

struct ProcessedTranscript {
    transcript: Transcript,
    raw_text: String,
    normalized_text: String,
    cleaned_text: Option<String>,
    formatting_time: Duration,
    warnings: Vec<String>,
    language: String,
    app_executable: Option<String>,
}

fn process_transcript(
    mut transcript: Transcript,
    language: &str,
    formatting: &WorkerFormatting,
    ollama: Option<&OllamaClient>,
    aliases: &[LexiconEntry],
    app_executable: Option<&str>,
) -> ProcessedTranscript {
    let raw_text = transcript.text.clone();
    let normalized_text = apply_aliases(
        &normalize_transcript(&raw_text),
        aliases,
        language,
        app_executable,
    );
    let formatting_started = Instant::now();
    let mut warnings = Vec::new();
    let (selected, cleaned_text) = match &formatting.profile {
        FormatProfile::Raw => (raw_text.clone(), None),
        FormatProfile::Light => (normalized_text.clone(), None),
        FormatProfile::Balanced | FormatProfile::Strong | FormatProfile::Custom(_) => {
            match (ollama, formatting.model.as_ref()) {
                (Some(client), Some(model)) => {
                    let cancel = CancellationToken::new();
                    match client.format(
                        model,
                        &normalized_text,
                        &formatting.profile,
                        formatting.keep_alive.clone(),
                        &cancel,
                    ) {
                        FormatResult::Formatted { text, .. } => (text.clone(), Some(text)),
                        FormatResult::Fallback { reason, .. } => {
                            warnings.push(format!("ollama_fallback:{}", fallback_reason(&reason)));
                            (normalized_text.clone(), None)
                        }
                    }
                }
                _ => {
                    warnings.push("ollama_fallback:model_not_configured".to_owned());
                    (normalized_text.clone(), None)
                }
            }
        }
    };
    let formatting_time = formatting_started.elapsed();
    transcript.text = selected;
    ProcessedTranscript {
        transcript,
        raw_text,
        normalized_text,
        cleaned_text,
        formatting_time,
        warnings,
        language: language.to_owned(),
        app_executable: app_executable.map(str::to_owned),
    }
}

fn fallback_reason(reason: &phorminx_ollama::FallbackReason) -> &'static str {
    match reason {
        phorminx_ollama::FallbackReason::RawProfile => "raw_profile",
        phorminx_ollama::FallbackReason::PromptRejected(_) => "prompt_rejected",
        phorminx_ollama::FallbackReason::Cancelled => "cancelled",
        phorminx_ollama::FallbackReason::ServiceUnavailable(_) => "service_unavailable",
        phorminx_ollama::FallbackReason::OutputRejected(_) => "output_rejected",
    }
}

fn apply_aliases(
    input: &str,
    aliases: &[LexiconEntry],
    language: &str,
    app_executable: Option<&str>,
) -> String {
    let mut applicable = aliases
        .iter()
        .filter(|entry| {
            entry.entry.enabled
                && entry.entry.app_executable.as_deref().is_none_or(|scope| {
                    app_executable.is_some_and(|app| scope.eq_ignore_ascii_case(app))
                })
                && entry
                    .entry
                    .language
                    .as_deref()
                    .is_none_or(|scope| scope.eq_ignore_ascii_case(language))
        })
        .collect::<Vec<_>>();
    applicable.sort_by_key(|entry| std::cmp::Reverse(entry.entry.alias.len()));

    let mut output = input.to_owned();
    for entry in applicable {
        let replacement = match entry.entry.case_policy {
            CasePolicy::PreserveInput | CasePolicy::UseCanonical => entry.entry.canonical.clone(),
            CasePolicy::Lowercase => entry.entry.canonical.to_lowercase(),
            CasePolicy::Uppercase => entry.entry.canonical.to_uppercase(),
        };
        output = replace_bounded(&output, &entry.entry.alias, &replacement);
    }
    output
}

fn replace_bounded(input: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return input.to_owned();
    }
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find(needle) {
        let start = cursor + relative;
        let end = start + needle.len();
        let left_is_word = input[..start]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
        let right_is_word = input[end..]
            .chars()
            .next()
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
        let needle_starts_word = needle
            .chars()
            .next()
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
        let needle_ends_word = needle
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
        if (needle_starts_word && left_is_word) || (needle_ends_word && right_is_word) {
            output.push_str(&input[cursor..end]);
        } else {
            output.push_str(&input[cursor..start]);
            output.push_str(replacement);
        }
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn persist_transcript(
    persistence: &Persistence,
    processed: &ProcessedTranscript,
) -> phorminx_persistence::Result<()> {
    let draft = DictationDraft {
        created_at_ms: now_ms(),
        raw_text: processed.raw_text.clone(),
        normalized_text: Some(processed.normalized_text.clone()),
        cleaned_text: processed.cleaned_text.clone(),
        selected_output: processed.transcript.text.clone(),
        language: Some(processed.language.clone()),
        target_executable: processed.app_executable.clone(),
        timings: TimingMetadata {
            audio_duration_ms: Some(
                processed
                    .transcript
                    .audio_duration
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
            ),
            stt_duration_ms: Some(
                processed
                    .transcript
                    .inference_time
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
            ),
            formatting_duration_ms: Some(
                processed
                    .formatting_time
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
            ),
            insertion_duration_ms: None,
        },
        warnings: processed.warnings.clone(),
    };
    persistence.history().insert(&draft).map(|_| ())
}

fn retention_policy(retention: HistoryRetention) -> RetentionPolicy {
    match retention {
        HistoryRetention::Disabled => RetentionPolicy::Disabled,
        HistoryRetention::OneDay => RetentionPolicy::Hours24,
        HistoryRetention::SevenDays => RetentionPolicy::Days7,
        HistoryRetention::ThirtyDays => RetentionPolicy::Days30,
        HistoryRetention::Indefinite => RetentionPolicy::Indefinite,
    }
}

fn production_ollama_client() -> OllamaClient {
    OllamaClient::new(
        OllamaEndpoint::default(),
        ClientTimeouts {
            connect: Duration::from_secs(1),
            response_headers: Duration::from_secs(4),
            response_body: Duration::from_secs(2),
            overall: Duration::from_secs(5),
        },
    )
    .expect("the fixed production Ollama timeout policy is valid")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod composition_tests {
    use super::*;
    use phorminx_persistence::NewLexiconEntry;

    fn alias(id: i64, spoken: &str, written: &str, language: Option<&str>) -> LexiconEntry {
        LexiconEntry {
            id,
            entry: NewLexiconEntry {
                canonical: written.to_owned(),
                alias: spoken.to_owned(),
                language: language.map(str::to_owned),
                app_executable: None,
                case_policy: CasePolicy::UseCanonical,
                enabled: true,
            },
        }
    }

    #[test]
    fn aliases_are_exact_bounded_scoped_and_longest_first() {
        let aliases = [
            alias(1, "open ai", "OpenAI", Some("en")),
            alias(2, "ai", "AI", None),
            alias(3, "não usar", "ERRADO", Some("pt-br")),
        ];

        assert_eq!(
            apply_aliases("open ai uses ai, not said.", &aliases, "en", None),
            "OpenAI uses AI, not said."
        );
        assert_eq!(
            replace_bounded("said ai chair", "ai", "AI"),
            "said AI chair"
        );
    }

    #[test]
    fn formatting_settings_map_to_local_profiles_and_lifecycles() {
        let mut settings = Settings::default();
        settings.formatting.strength = FormattingStrength::Strong;
        settings.formatting.ollama_model = Some("qwen2.5:3b".to_owned());
        settings.formatting.ollama_lifecycle = OllamaLifecycle::MemorySaver;

        let worker = WorkerFormatting::from_settings(&settings).unwrap();

        assert_eq!(worker.profile, FormatProfile::Strong);
        assert_eq!(worker.model.unwrap().as_str(), "qwen2.5:3b");
        assert_eq!(worker.keep_alive, KeepAlive::UnloadAfterRequest);
    }

    #[test]
    fn every_history_retention_maps_without_weakening_policy() {
        assert_eq!(
            retention_policy(HistoryRetention::Disabled),
            RetentionPolicy::Disabled
        );
        assert_eq!(
            retention_policy(HistoryRetention::OneDay),
            RetentionPolicy::Hours24
        );
        assert_eq!(
            retention_policy(HistoryRetention::SevenDays),
            RetentionPolicy::Days7
        );
        assert_eq!(
            retention_policy(HistoryRetention::ThirtyDays),
            RetentionPolicy::Days30
        );
        assert_eq!(
            retention_policy(HistoryRetention::Indefinite),
            RetentionPolicy::Indefinite
        );
    }
}

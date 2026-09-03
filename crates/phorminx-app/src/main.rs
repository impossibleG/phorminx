#![cfg_attr(all(windows, feature = "desktop"), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, ValueEnum};
use phorminx_app::incremental::{
    BoundaryKind, CHUNK_OVERLAP, IncrementalPlanner, MergeExpectation, SILENCE_PROBE_DURATION,
    merge_overlapping, strip_known_non_speech_annotations,
};
use phorminx_app::model::{
    ModelDownload, ModelDownloadEvent, identify_pinned_model, model_for_variant,
};
use phorminx_app::product_shell::{ProductShell, ProductShellControl, ProductShellEvent};
use phorminx_app::runtime::{
    AppIo, AppRuntime, FinishedAudio, InsertDisposition, ReleaseTiming, RuntimeNotice, UiStatus,
};
use phorminx_app::settings::{
    AccurateBackendPreference, AccurateModelVariant, FormattingStrength, HistoryRetention,
    OllamaLifecycle, RecognitionMode, RecordingMode, RuntimeFormatting, Settings, SettingsStore,
};
use phorminx_app::ui_bridge::{UiMutation, UiRoute, UiRuntimeStatus, UiVoskProbe};
use phorminx_audio::{
    DeferredCapturedAudio, ExtendedCaptureConfig, ExtendedCaptureFactory, ExtendedCaptureFault,
    ExtendedCapturedAudio, ExtendedRecording, ExtendedStorageKind, input_devices,
};
#[cfg(test)]
use phorminx_core::recommended_audio_context;
use phorminx_core::{
    AudioClip, DictationId, RuntimeState, SpeechRecognizer, Transcript, TranscriptionOptions,
    normalize_transcript,
};
use phorminx_ollama::{
    CancellationToken, ClientTimeouts, DocumentFormatDisposition, FormatProfile, FormatResult,
    KeepAlive, ModelName, OllamaClient, OllamaEndpoint, SelectionPolicy,
};
use phorminx_persistence::{
    AppProfile, CasePolicy, DictationDraft, ExecutableIdentity, FormattingStyle,
    InsertionPreference, LexiconEntry, NewLexiconEntry, Persistence, RetentionPolicy,
    TerminalMetadata, TimingMetadata,
};
use phorminx_session::SampleRange;
use phorminx_vosk::{AssetLayout as VoskAssetLayout, VoskModel, VoskSession};
use phorminx_whisper::{
    WhisperBackendPreference, WhisperError, WhisperReadiness, WhisperRecognizer,
};
use phorminx_windows::{
    ClipboardOnlyReason, GlobalHoldHotkey, HistoryItem, HistoryWindow, HistoryWindowEvent,
    HoldEvent, InsertionOutcome, LexiconCasePolicy, LexiconDraft, LexiconItem, LexiconWindow,
    LexiconWindowEvent, OverlayStatus, ProfileFormatting, ProfileInsertion, ProfileItem,
    ProfileWindow, ProfileWindowEvent, SettingsAccurateBackend, SettingsAccurateModel,
    SettingsForm, SettingsFormatting, SettingsHistoryRetention, SettingsOllamaLifecycle,
    SettingsRecognitionMode, SettingsRecordingMode, SettingsWindow, SettingsWindowEvent,
    SingleInstance, SingleInstanceError, StatusOverlay, SystemTray, TargetSnapshot, TrayEvent,
    TrayStatus, activate_existing_window, choose_zip_archive, copy_and_maybe_paste,
    set_launch_at_login,
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
    /// Start ready in the tray without showing the product shell.
    #[arg(long, hide = true)]
    background: bool,
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

const FATAL_STARTUP_MESSAGE: &str = "Phorminx could not initialize its local services. Your settings and recordings were not changed. Restart Phorminx; if the problem continues, reinstall the current release.";

fn report_fatal_error(error: &anyhow::Error) {
    // Fatal startup surfaces are intentionally content-free. The underlying
    // error chain may contain a user name, model path, or settings path.
    let _ = error;
    #[cfg(feature = "desktop")]
    show_error_dialog("Phorminx could not start", FATAL_STARTUP_MESSAGE);

    #[cfg(not(feature = "desktop"))]
    eprintln!("{FATAL_STARTUP_MESSAGE}");
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let instance = match SingleInstance::acquire() {
        Ok(instance) => instance,
        Err(SingleInstanceError::AlreadyRunning) => {
            for _ in 0..20 {
                if activate_existing_window() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(50));
            }
            return Ok(());
        }
        Err(error) => {
            return Err(error).context("failed to acquire the Phorminx process slot");
        }
    };
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
    let model_override = cli.model.is_some();
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
    let instant_model = settings_store.resolve_asset_path(&settings.recognition.instant_model_path);
    let instant_runtime =
        settings_store.resolve_asset_path(&settings.recognition.instant_runtime_path);

    let tray = SystemTray::start().context("failed to start the system tray")?;
    show_shell_status(&overlay, &tray, OverlayStatus::Loading, TrayStatus::Loading);
    let shutting_down = Arc::new(AtomicBool::new(false));
    #[cfg(not(feature = "desktop"))]
    {
        let shutdown_flag = Arc::clone(&shutting_down);
        ctrlc::set_handler(move || shutdown_flag.store(true, Ordering::Release))
            .context("failed to install the Ctrl+C handler")?;
    }

    let recognition_ready = match settings.recognition.mode {
        RecognitionMode::Accurate => model.is_file(),
        // This gate performs no native/model load. The worker below takes sole
        // ownership of the one full load and retains it for the process lifetime.
        RecognitionMode::Instant => matches!(
            phorminx_vosk::validate_asset_layout(
                &instant_runtime,
                &instant_model,
                &settings.recognition.language,
            ),
            VoskAssetLayout::Present { .. }
        ),
    };
    if !recognition_ready {
        if cli.smoke_test {
            return Err(anyhow!(
                "the selected local recognition engine is not ready"
            ));
        }
        return run_setup_mode(
            overlay,
            tray,
            settings_store,
            settings,
            model,
            shutting_down,
            instance,
        );
    }
    let verified_variant = if settings.recognition.mode == RecognitionMode::Accurate {
        let verified_variant =
            identify_pinned_model(&model).context("failed to verify Whisper model")?;
        if !model_override && settings.recognition.accurate_model != AccurateModelVariant::Custom {
            model_for_variant(settings.recognition.accurate_model)
                .context("the selected pinned Whisper model is unavailable")?;
            if verified_variant != Some(settings.recognition.accurate_model) {
                return Err(anyhow!(
                    "the selected pinned Whisper model does not match its verified manifest identity"
                ));
            }
        }
        if let Some(variant) = verified_variant
            && !variant.supports_language(&settings.recognition.language)
        {
            return Err(anyhow!(
                "the verified English-only Whisper model cannot transcribe language {}",
                settings.recognition.language
            ));
        }
        verified_variant
    } else {
        None
    };

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
    let mut dictation_context = DictationContext::global(&settings, verified_variant)?;

    println!("Phorminx is loading its local recognition engine...");
    let recognizer = match settings.recognition.mode {
        RecognitionMode::Accurate => RecognizerConfig::Accurate {
            model: model.clone(),
            backend: whisper_backend_preference(settings.recognition.accurate_backend),
        },
        RecognitionMode::Instant => RecognizerConfig::Instant {
            runtime_bundle: instant_runtime,
            model: instant_model,
            language: settings.recognition.language.clone(),
        },
    };
    let worker = match TranscriptionWorker::start(recognizer, worker_formatting, aliases) {
        Ok(worker) => worker,
        Err(error) if settings.recognition.mode == RecognitionMode::Instant => {
            eprintln!(
                "dictation_id=0 state=Starting event=instant_startup_probe_failed recovery=setup"
            );
            drop(persistence);
            return run_setup_mode(
                overlay,
                tray,
                settings_store,
                settings,
                model,
                shutting_down,
                instance,
            )
            .with_context(|| format!("Instant recovery setup failed after: {error}"));
        }
        Err(error) => return Err(error),
    };

    let hotkey = GlobalHoldHotkey::start().context("failed to install the global hotkey")?;
    let capture = ExtendedCaptureFactory::new(ExtendedCaptureConfig::new(
        settings_store
            .path()
            .parent()
            .context("settings path has no parent directory")?
            .join("temp")
            .join("audio-spool"),
    ))
    .context("failed to prepare extended audio capture")?;
    let mut runtime = AppRuntime::<TargetSnapshot, ExtendedRecording>::new_with_formatting(
        settings.recognition.minimum_rms,
        settings.recognition.language.clone(),
        formatting,
    )?;
    let mut incremental = IncrementalPlanner::default();
    let mut instant_pump = InstantPump::default();

    println!("Ready. Hold Ctrl+Alt+Space to dictate; press Ctrl+C here to exit.");
    log_state(None, runtime.state(), "ready");
    {
        let mut io = ProductionIo {
            overlay: &overlay,
            tray: &tray,
            worker: &worker,
            microphone: effective_microphone.as_deref(),
            context: &dictation_context,
            capture: &capture,
            released_at: None,
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
                active_shell_vosk_probe(settings.recognition.mode),
                !cli.background,
                worker.readiness().cloned(),
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
                loaded_whisper: worker.readiness(),
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
                    &mut incremental,
                    effective_microphone.as_deref(),
                    &dictation_context,
                    &capture,
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
                    &capture,
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
                loaded_whisper: worker.readiness(),
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
                    &mut incremental,
                    effective_microphone.as_deref(),
                    &dictation_context,
                    &capture,
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
                    &capture,
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
                let release_received_at = (settings.interaction.recording_mode
                    == RecordingMode::Toggle
                    && runtime.state() == RuntimeState::Listening)
                    .then(Instant::now);
                if let (Some(released_at), Some(id)) = (release_received_at, runtime.active_id()) {
                    log_release_received(id, released_at);
                }
                if runtime.state() == RuntimeState::Idle {
                    dictation_context = match DictationContext::for_target(
                        activation_target,
                        &persistence,
                        &settings,
                        verified_variant,
                    ) {
                        Ok(context) => context,
                        Err(error) => {
                            eprintln!(
                                "dictation_id=0 state=Idle event=dictation_context_rejected error={error}"
                            );
                            show_shell_status(
                                &overlay,
                                &tray,
                                OverlayStatus::Error,
                                TrayStatus::Error,
                            );
                            continue 'event_loop;
                        }
                    };
                    if let Some(reason) = dictation_context.activation_block {
                        eprintln!(
                            "dictation_id=0 state=Idle event=activation_blocked reason={}",
                            reason.label()
                        );
                        show_shell_status(&overlay, &tray, OverlayStatus::Error, TrayStatus::Error);
                        if let Some(shell) = product_shell.as_ref() {
                            let _ = shell.send(ProductShellControl::ActivationBlocked(
                                reason.message().to_owned(),
                            ));
                        }
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
                        capture: &capture,
                        released_at: release_received_at,
                    };
                    let action = if let Some(released_at) = release_received_at {
                        runtime.hold_ended_at(released_at, &mut io)
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
                // This is the user-observable release boundary. Capture it
                // before stopping/resampling the recording so telemetry
                // includes all finalization work.
                if settings.interaction.recording_mode == RecordingMode::Toggle {
                    continue 'event_loop;
                }
                let released_at = Instant::now();
                if let Some(id) = runtime.active_id() {
                    log_release_received(id, released_at);
                }
                let notices = {
                    let mut io = ProductionIo {
                        overlay: &overlay,
                        tray: &tray,
                        worker: &worker,
                        microphone: effective_microphone.as_deref(),
                        context: &dictation_context,
                        capture: &capture,
                        released_at: Some(released_at),
                    };
                    match runtime.hold_ended_at(released_at, &mut io) {
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

        auto_stop_failed_capture(
            &mut runtime,
            &overlay,
            &tray,
            &worker,
            effective_microphone.as_deref(),
            &dictation_context,
            &capture,
        )?;

        match settings.recognition.mode {
            RecognitionMode::Accurate => poll_incremental_transcription(
                &runtime,
                &mut incremental,
                &worker,
                &dictation_context.language,
                settings.recognition.minimum_rms,
            ),
            RecognitionMode::Instant => {
                poll_instant_transcription(&mut runtime, &mut instant_pump, &worker)
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
                loaded_whisper: worker.readiness(),
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
                    &mut incremental,
                    effective_microphone.as_deref(),
                    &dictation_context,
                    &capture,
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
                    &capture,
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
                Ok(WorkerEvent::PartialCompleted {
                    id,
                    sequence,
                    succeeded,
                    compute_time,
                    audio_duration,
                }) => {
                    incremental.partial_completed(id, sequence, succeeded);
                    eprintln!(
                        "dictation_id={} state={:?} event=incremental_partial_completed sequence={} success={} audio_ms={} partial_compute_ms={}",
                        id.0,
                        runtime.state(),
                        sequence,
                        succeeded,
                        audio_duration.as_millis(),
                        compute_time.as_millis()
                    );
                }
                Ok(WorkerEvent::CleanupStarted { id }) => {
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            tray: &tray,
                            worker: &worker,
                            microphone: effective_microphone.as_deref(),
                            context: &dictation_context,
                            capture: &capture,
                            released_at: None,
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
                    let is_current = runtime.pending_id() == Some(completed.id);
                    let mut processed_for_history = None;
                    let result = completed.result.map(|processed| {
                        if is_current {
                            if let Some(lifecycle) = processed.lifecycle {
                                eprintln!(
                                    "uptime_ms={} dictation_id={} state=Cleaning event=worker_completed release_to_worker_ms={} audio_finalize_ms={} worker_queue_ms={}",
                                    monotonic_uptime_ms(),
                                    completed.id.0,
                                    lifecycle
                                        .worker_completed_at
                                        .saturating_duration_since(lifecycle.release.released_at)
                                        .as_millis(),
                                    lifecycle.release.audio_finalization_time().as_millis(),
                                    lifecycle
                                        .worker_started_at
                                        .saturating_duration_since(
                                            lifecycle.release.audio_finalized_at,
                                        )
                                        .as_millis()
                                );
                            }
                            let transcript = processed.transcript.clone();
                            processed_for_history = Some(processed);
                            transcript
                        } else {
                            processed.transcript
                        }
                    });
                    let insertion_started_at = Instant::now();
                    let notices = {
                        let mut io = ProductionIo {
                            overlay: &overlay,
                            tray: &tray,
                            worker: &worker,
                            microphone: effective_microphone.as_deref(),
                            context: &dictation_context,
                            capture: &capture,
                            released_at: None,
                        };
                        match runtime.transcription_completed(completed.id, result, &mut io) {
                            Ok(notices) => notices,
                            Err(error) => break 'event_loop Err(error.into()),
                        }
                    };
                    let insertion_completed_at = Instant::now();
                    if is_current && let Some(processed) = processed_for_history.as_ref() {
                        let outcome = terminal_outcome(&notices);
                        if outcome.is_success() {
                            if let Some(lifecycle) = processed.lifecycle {
                                eprintln!(
                                    "uptime_ms={} dictation_id={} state=Inserting event=release_terminal outcome={} release_to_insert_ms={} insertion_ms={}",
                                    monotonic_uptime_ms(),
                                    completed.id.0,
                                    outcome.label(),
                                    insertion_completed_at
                                        .saturating_duration_since(lifecycle.release.released_at)
                                        .as_millis(),
                                    insertion_completed_at
                                        .saturating_duration_since(insertion_started_at)
                                        .as_millis()
                                );
                            }
                            if let Err(error) = persist_transcript(
                                &persistence,
                                processed,
                                insertion_started_at,
                                insertion_completed_at,
                            ) {
                                eprintln!(
                                    "dictation_id={} state=Cleaning event=history_write_failed error={error}",
                                    completed.id.0
                                );
                            }
                        } else {
                            record_terminal_failure(completed.id, outcome);
                        }
                    }
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
                            capture: &capture,
                            released_at: None,
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
        // The replacement process acquires this same mutex during startup. Release
        // our guard before spawning it so a settings restart cannot reject itself.
        drop(instance);
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

fn active_shell_vosk_probe(mode: RecognitionMode) -> UiVoskProbe {
    match mode {
        RecognitionMode::Instant => UiVoskProbe::ResidentReady,
        RecognitionMode::Accurate => UiVoskProbe::LayoutOnly,
    }
}

fn setup_shell_vosk_probe() -> UiVoskProbe {
    UiVoskProbe::FullValidation
}

fn run_setup_mode(
    overlay: StatusOverlay,
    tray: SystemTray,
    settings_store: SettingsStore,
    mut settings: Settings,
    model: PathBuf,
    shutting_down: Arc<AtomicBool>,
    instance: SingleInstance,
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
            setup_shell_vosk_probe(),
            true,
            None,
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
                loaded_whisper: None,
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
        drop(instance);
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
    loaded_whisper: Option<&'a WhisperReadiness>,
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
        loaded_whisper,
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
                        SettingsWindow::start(settings_form(
                            settings,
                            effective_model,
                            loaded_whisper,
                        ))
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
                Ok(ProductShellEvent::ChangeWhisperModel(variant)) => {
                    if model_download.is_none() {
                        let directory = settings_store
                            .path()
                            .parent()
                            .context("the settings path has no parent directory")?
                            .join("models");
                        match ModelDownload::start_variant(&directory, variant) {
                            Ok(download) => *model_download = Some(download),
                            Err(error) => {
                                eprintln!("model_download_failed error={error}");
                                let _ = shell.send(ProductShellControl::ModelDownloadFailed);
                            }
                        }
                    }
                }
                Ok(ProductShellEvent::InstallVerifiedVoskAssets) => {
                    match install_verified_vosk_assets(settings_store) {
                        Ok(Some((runtime_path, model_path))) => {
                            settings.recognition.mode = RecognitionMode::Instant;
                            settings.recognition.language = "en".to_owned();
                            settings.recognition.instant_runtime_path = runtime_path;
                            settings.recognition.instant_model_path = model_path;
                            settings_store
                                .save(settings)
                                .context("failed to save verified Vosk asset paths")?;
                            return Ok(ShellAction::Restart);
                        }
                        Ok(None) => {}
                        Err(error) => {
                            eprintln!(
                                "dictation_id=0 state=Setup event=vosk_verified_import_failed"
                            );
                            let _ = shell.send(ProductShellControl::VoskInstallFailed(format!(
                                "Verified Vosk import failed: {error}"
                            )));
                        }
                    }
                }
                Ok(ProductShellEvent::CancelWhisperModelDownload) => {
                    if let Some(download) = model_download.as_ref() {
                        download.cancel();
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
                        // The UI bridge has already persisted the candidate. Restore only
                        // the external preference while preserving every other saved field.
                        let mut persisted = settings_store.load().context(
                            "failed to reload settings after startup registration failure",
                        )?;
                        persisted.startup.launch_at_login = settings.startup.launch_at_login;
                        settings_store
                            .save(&persisted)
                            .context("failed to restore launch-at-login preference")?;
                        show_shell_status(overlay, tray, OverlayStatus::Error, TrayStatus::Error);
                    } else {
                        settings.startup.launch_at_login = enabled;
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
                    let verified_variant = identify_pinned_model(effective_model)
                        .context("failed to verify the active Whisper model")?;
                    if profile.language.as_deref().is_some_and(|language| {
                        verified_variant.is_some_and(|variant| !variant.supports_language(language))
                    }) {
                        return Err(anyhow!(
                            "select a multilingual Whisper model before using this profile language"
                        ));
                    }
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
            SettingsWindowEvent::DownloadModel(variant) => {
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
                match ModelDownload::start_variant(&directory, from_window_accurate_model(variant))
                {
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
            ModelDownloadEvent::Completed { path, variant } => {
                if let Some(shell) = product_shell {
                    let _ = shell.send(ProductShellControl::ModelDownloaded {
                        path: path.clone(),
                        variant,
                    });
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

fn settings_form(
    settings: &Settings,
    effective_model: &Path,
    loaded_whisper: Option<&WhisperReadiness>,
) -> SettingsForm {
    let model_status = match (std::fs::metadata(effective_model), loaded_whisper) {
        (Ok(metadata), Some(loaded)) if metadata.is_file() => {
            let fallback = loaded
                .fallback_from
                .map(|backend| format!(" after {} fallback", backend.as_str()))
                .unwrap_or_default();
            format!(
                "Model loaded on {}{fallback} ({:.1} MiB)",
                loaded.backend.as_str(),
                metadata.len() as f64 / (1024.0 * 1024.0)
            )
        }
        (Ok(metadata), None) if metadata.is_file() => format!(
            "Model available but recognizer not loaded ({:.1} MiB)",
            metadata.len() as f64 / (1024.0 * 1024.0)
        ),
        (Ok(_), _) => "The selected model path is not a file".to_owned(),
        (Err(_), _) => "Model not found - choose a local .bin file".to_owned(),
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
        recognition_mode: match settings.recognition.mode {
            RecognitionMode::Instant => SettingsRecognitionMode::Instant,
            RecognitionMode::Accurate => SettingsRecognitionMode::Accurate,
        },
        model_path: effective_model.display().to_string(),
        instant_model_path: settings
            .recognition
            .instant_model_path
            .display()
            .to_string(),
        instant_runtime_path: settings
            .recognition
            .instant_runtime_path
            .display()
            .to_string(),
        model_status,
        microphone_status,
        microphones,
        microphone: settings.recognition.microphone.clone(),
        model_download_labels: accurate_model_download_labels(),
        accurate_model: to_window_accurate_model(settings.recognition.accurate_model),
        accurate_backend: match settings.recognition.accurate_backend {
            AccurateBackendPreference::Auto => SettingsAccurateBackend::Auto,
            AccurateBackendPreference::Vulkan => SettingsAccurateBackend::Vulkan,
            AccurateBackendPreference::Cpu => SettingsAccurateBackend::Cpu,
        },
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

fn accurate_model_download_labels() -> Vec<String> {
    [
        (AccurateModelVariant::TinyEnglish, "Tiny English"),
        (AccurateModelVariant::BaseEnglish, "Base English"),
        (AccurateModelVariant::TinyMultilingual, "Tiny Multilingual"),
        (AccurateModelVariant::BaseMultilingual, "Base Multilingual"),
    ]
    .into_iter()
    .map(|(variant, name)| {
        model_for_variant(variant).map_or_else(
            |_| format!("{name} download unavailable"),
            |model| {
                format!(
                    "Download {name} ({:.1} MiB)",
                    model.bytes as f64 / (1024.0 * 1024.0)
                )
            },
        )
    })
    .chain(std::iter::once(
        "Custom model: use Browse instead of download".to_owned(),
    ))
    .collect()
}

fn to_window_accurate_model(variant: AccurateModelVariant) -> SettingsAccurateModel {
    match variant {
        AccurateModelVariant::TinyEnglish => SettingsAccurateModel::TinyEnglish,
        AccurateModelVariant::BaseEnglish => SettingsAccurateModel::BaseEnglish,
        AccurateModelVariant::TinyMultilingual => SettingsAccurateModel::TinyMultilingual,
        AccurateModelVariant::BaseMultilingual => SettingsAccurateModel::BaseMultilingual,
        AccurateModelVariant::Custom => SettingsAccurateModel::Custom,
    }
}

fn from_window_accurate_model(variant: SettingsAccurateModel) -> AccurateModelVariant {
    match variant {
        SettingsAccurateModel::TinyEnglish => AccurateModelVariant::TinyEnglish,
        SettingsAccurateModel::BaseEnglish => AccurateModelVariant::BaseEnglish,
        SettingsAccurateModel::TinyMultilingual => AccurateModelVariant::TinyMultilingual,
        SettingsAccurateModel::BaseMultilingual => AccurateModelVariant::BaseMultilingual,
        SettingsAccurateModel::Custom => AccurateModelVariant::Custom,
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
    candidate.recognition.mode = match form.recognition_mode {
        SettingsRecognitionMode::Instant => RecognitionMode::Instant,
        SettingsRecognitionMode::Accurate => RecognitionMode::Accurate,
    };
    candidate.recognition.model_path = PathBuf::from(form.model_path.trim());
    candidate.recognition.instant_model_path = PathBuf::from(form.instant_model_path.trim());
    candidate.recognition.instant_runtime_path = PathBuf::from(form.instant_runtime_path.trim());
    candidate.recognition.accurate_model = from_window_accurate_model(form.accurate_model);
    candidate.recognition.accurate_backend = match form.accurate_backend {
        SettingsAccurateBackend::Auto => AccurateBackendPreference::Auto,
        SettingsAccurateBackend::Vulkan => AccurateBackendPreference::Vulkan,
        SettingsAccurateBackend::Cpu => AccurateBackendPreference::Cpu,
    };
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

#[derive(Clone, Copy)]
struct FinalAudioStats {
    total_samples: u64,
    peak_retained_samples: u64,
    rms: f32,
    auto_stopped: bool,
}

enum FinalAudioSource {
    Pending {
        audio: DeferredCapturedAudio,
        stats: FinalAudioStats,
    },
    Short {
        clip: AudioClip,
        stats: FinalAudioStats,
    },
    Extended {
        audio: Box<ExtendedCapturedAudio>,
        stats: FinalAudioStats,
    },
}

impl FinalAudioSource {
    fn resolve(self) -> Result<Self, String> {
        match self {
            Self::Pending { audio, stats } => {
                let captured = audio.resolve().map_err(|error| error.to_string())?;
                let stats = FinalAudioStats {
                    total_samples: captured.total_samples(),
                    ..stats
                };
                if captured.is_extended() {
                    Ok(Self::Extended {
                        audio: Box::new(captured),
                        stats,
                    })
                } else {
                    Ok(Self::Short {
                        clip: captured
                            .into_short_clip()
                            .map_err(|error| error.to_string())?,
                        stats,
                    })
                }
            }
            resolved => Ok(resolved),
        }
    }

    fn stats(&self) -> FinalAudioStats {
        match self {
            Self::Pending { stats, .. }
            | Self::Short { stats, .. }
            | Self::Extended { stats, .. } => *stats,
        }
    }

    fn cleanup(self) -> Result<(), String> {
        match self {
            Self::Pending { .. } => Err("final audio did not resolve before cleanup".to_owned()),
            Self::Short { .. } => Ok(()),
            Self::Extended { audio, .. } => audio.cleanup().map_err(|error| error.to_string()),
        }
    }
}

fn require_audio_cleanup<T>(
    recognition: Result<T, String>,
    cleanup: Result<(), String>,
) -> Result<T, String> {
    match cleanup {
        Ok(()) => recognition,
        Err(cleanup_error) => Err(match recognition {
            Ok(_) => format!("encrypted audio scratch cleanup failed: {cleanup_error}"),
            Err(recognition_error) => format!(
                "transcription failed ({recognition_error}); encrypted audio scratch cleanup also failed ({cleanup_error})"
            ),
        }),
    }
}

fn sample_range_for_duration(start: Duration, end: Duration) -> Result<SampleRange, String> {
    let samples = |duration: Duration| (duration.as_secs_f64() * 16_000.0).floor() as u64;
    SampleRange::new(samples(start), samples(end)).map_err(|error| error.to_string())
}

fn audio_span_to_clip(
    span: phorminx_session::AudioSpan,
) -> Result<AudioClip, phorminx_audio::CaptureError> {
    let mut guarded = span.into_samples();
    let samples = std::mem::take(&mut *guarded);
    AudioClip::new(samples, 16_000).map_err(phorminx_audio::CaptureError::Audio)
}

struct ProductionIo<'a> {
    overlay: &'a StatusOverlay,
    tray: &'a SystemTray,
    worker: &'a TranscriptionWorker,
    microphone: Option<&'a str>,
    context: &'a DictationContext,
    capture: &'a ExtendedCaptureFactory,
    released_at: Option<Instant>,
}

impl AppIo for ProductionIo<'_> {
    type Target = TargetSnapshot;
    type Recording = ExtendedRecording;
    type Audio = FinalAudioSource;
    type ClipboardReason = ClipboardOnlyReason;

    fn start_recording(&mut self) -> Result<Self::Recording, String> {
        match self.capture.start_input(self.microphone) {
            Ok(recording) => Ok(recording),
            Err(selected_error) if self.microphone.is_some() => {
                eprintln!(
                    "dictation_id=0 state=Listening event=selected_microphone_failed recovery=windows_default"
                );
                self.capture.start_input(None).map_err(|fallback_error| {
                    format!(
                        "selected microphone failed ({selected_error}); Windows default recovery failed ({fallback_error})"
                    )
                })
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn finish_recording(
        &mut self,
        recording: Self::Recording,
    ) -> Result<FinishedAudio<FinalAudioSource>, String> {
        let progress = recording.progress();
        let auto_stopped = recording.was_auto_stopped();
        if let Some(fault) = progress.sticky_fault {
            // Still drive the stopped pump through its bounded finalization
            // owner. This confirms or quarantines scratch cleanup instead of
            // abandoning the session at the first observed backend fault.
            if let Ok(captured) = recording.finalize() {
                captured.cleanup().map_err(|error| {
                    format!("{fault}; encrypted audio scratch cleanup also failed ({error})")
                })?;
            }
            return Err(fault.to_string());
        }
        let duration = progress.duration();
        if progress.storage == ExtendedStorageKind::Memory {
            let captured = recording.finalize().map_err(|error| error.to_string())?;
            let backend_warning_count = captured.backend_warning_count();
            let clip = captured
                .into_short_clip()
                .map_err(|error| error.to_string())?;
            let duration = clip.duration();
            let rms = clip.rms();
            return Ok(FinishedAudio {
                audio: FinalAudioSource::Short {
                    stats: FinalAudioStats {
                        total_samples: clip.samples.len() as u64,
                        peak_retained_samples: progress.peak_resident_samples,
                        rms,
                        auto_stopped,
                    },
                    clip,
                },
                duration,
                rms,
                backend_warning_count,
            });
        }
        let rms = if progress.canonical_samples == 0 {
            0.0
        } else {
            recording
                .recent_rms(Duration::from_secs(2))
                .map_err(|error| error.to_string())?
        };
        let backend_warning_count = progress.backend_warning_count;
        let audio = FinalAudioSource::Pending {
            audio: recording
                .defer_finalize()
                .map_err(|error| error.to_string())?,
            stats: FinalAudioStats {
                total_samples: progress.canonical_samples,
                peak_retained_samples: progress.peak_resident_samples,
                rms,
                auto_stopped,
            },
        };
        Ok(FinishedAudio {
            audio,
            duration,
            rms,
            backend_warning_count,
        })
    }

    fn submit_transcription(
        &mut self,
        id: DictationId,
        audio: FinalAudioSource,
        language: &str,
        audio_context: u32,
        timing: ReleaseTiming,
    ) -> Result<(), String> {
        self.worker
            .transcribe(TranscriptionRequest {
                id,
                released_at: self.released_at.unwrap_or_else(Instant::now),
                audio,
                language: language.to_owned(),
                audio_context,
                formatting: self.context.formatting.clone(),
                app_executable: self.context.app_executable.clone(),
                timing,
            })
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutoStopReason {
    CaptureOverflow,
    DeviceLoss,
    Resampling,
    ScratchQuota,
    ScratchIntegrity,
    ScratchIo,
    CaptureWorker,
    SampleAccounting,
}

impl AutoStopReason {
    const fn label(self) -> &'static str {
        match self {
            Self::CaptureOverflow => "capture_overflow",
            Self::DeviceLoss => "device_loss",
            Self::Resampling => "resampling",
            Self::ScratchQuota => "scratch_quota",
            Self::ScratchIntegrity => "scratch_integrity",
            Self::ScratchIo => "scratch_io",
            Self::CaptureWorker => "capture_worker",
            Self::SampleAccounting => "sample_accounting",
        }
    }
}

fn auto_stop_reason(fault: ExtendedCaptureFault) -> Option<AutoStopReason> {
    match fault {
        ExtendedCaptureFault::CallbackOverflow => Some(AutoStopReason::CaptureOverflow),
        ExtendedCaptureFault::StreamFailed => Some(AutoStopReason::DeviceLoss),
        ExtendedCaptureFault::Resampling => Some(AutoStopReason::Resampling),
        ExtendedCaptureFault::SpoolQuota => Some(AutoStopReason::ScratchQuota),
        ExtendedCaptureFault::SpoolIntegrity => Some(AutoStopReason::ScratchIntegrity),
        ExtendedCaptureFault::SpoolIo => Some(AutoStopReason::ScratchIo),
        ExtendedCaptureFault::WorkerUnavailable
        | ExtendedCaptureFault::WorkerPanicked
        | ExtendedCaptureFault::FinalizationTimeout => Some(AutoStopReason::CaptureWorker),
        ExtendedCaptureFault::SampleAccountingOverflow => Some(AutoStopReason::SampleAccounting),
        ExtendedCaptureFault::InvalidConfiguration
        | ExtendedCaptureFault::InvalidSnapshot
        | ExtendedCaptureFault::FinalizerBusy => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn auto_stop_failed_capture(
    runtime: &mut AppRuntime<TargetSnapshot, ExtendedRecording>,
    overlay: &StatusOverlay,
    tray: &SystemTray,
    worker: &TranscriptionWorker,
    microphone: Option<&str>,
    context: &DictationContext,
    capture: &ExtendedCaptureFactory,
) -> Result<()> {
    if runtime.state() != RuntimeState::Listening {
        return Ok(());
    }
    let Some((id, reason)) = runtime.active_id().zip(
        runtime
            .active_recording()
            .and_then(|recording| recording.progress().sticky_fault)
            .and_then(auto_stop_reason),
    ) else {
        return Ok(());
    };
    if let Some(recording) = runtime.active_recording_mut() {
        recording.mark_auto_stopped();
    }
    let stopped_at = Instant::now();
    eprintln!(
        "uptime_ms={} dictation_id={} state=FinalizingAudio event=automatic_safe_stop auto_stopped=true reason={}",
        monotonic_uptime_ms(),
        id.0,
        reason.label()
    );
    let notices = {
        let mut io = ProductionIo {
            overlay,
            tray,
            worker,
            microphone,
            context,
            capture,
            released_at: Some(stopped_at),
        };
        runtime.hold_ended_at(stopped_at, &mut io)?
    };
    report_notices(notices);
    Ok(())
}

fn poll_incremental_transcription(
    runtime: &AppRuntime<TargetSnapshot, ExtendedRecording>,
    planner: &mut IncrementalPlanner,
    worker: &TranscriptionWorker,
    language: &str,
    minimum_rms: f32,
) {
    if runtime.state() != RuntimeState::Listening {
        if let Some(id) = planner.active_id() {
            if matches!(
                runtime.state(),
                RuntimeState::Transcribing
                    | RuntimeState::Normalizing
                    | RuntimeState::Cleaning
                    | RuntimeState::ReadyToInsert
                    | RuntimeState::Inserting
            ) {
                planner.finish(id);
            } else {
                planner.cancel();
                let _ = worker.cancel_incremental(id);
                eprintln!(
                    "dictation_id={} state={:?} event=incremental_cancelled",
                    id.0,
                    runtime.state()
                );
            }
        }
        return;
    }

    let Some(id) = runtime.active_id() else {
        return;
    };
    if planner.active_id() != Some(id) {
        if let Some(stale) = planner.cancel() {
            let _ = worker.cancel_incremental(stale);
        }
        planner.start(id);
    }
    let Some(recording) = runtime.active_recording() else {
        planner.degrade(id);
        let _ = worker.cancel_incremental(id);
        return;
    };
    let captured = recording.captured_duration();
    if !planner.needs_probe(id, captured) {
        return;
    }

    // This is a boundary hint, not endpoint trimming. The overlap is as long as
    // the entire probe, and the untouched full recording remains the fallback.
    let silence_threshold = minimum_rms.clamp(0.000_8, 0.003);
    let silence_observed = recording
        .recent_rms(SILENCE_PROBE_DURATION)
        .is_ok_and(|rms| rms <= silence_threshold);
    let Some(plan) = planner.observe(id, captured, silence_observed) else {
        return;
    };
    let range = match sample_range_for_duration(plan.range.start, plan.range.end) {
        Ok(range) => range,
        Err(error) => {
            planner.degrade(id);
            let _ = worker.cancel_incremental(id);
            eprintln!(
                "dictation_id={} state=Listening event=incremental_fallback reason=invalid_range error={error}",
                id.0
            );
            return;
        }
    };
    let clip = match recording.snapshot(range).and_then(audio_span_to_clip) {
        Ok(clip) => clip,
        Err(error) => {
            planner.degrade(id);
            let _ = worker.cancel_incremental(id);
            eprintln!(
                "dictation_id={} state=Listening event=incremental_fallback reason=snapshot_failed error={error}",
                id.0
            );
            return;
        }
    };
    let audio_duration = clip.duration();
    match worker.transcribe_partial(plan, clip, language.to_owned()) {
        Ok(()) => eprintln!(
            "dictation_id={} state=Listening event=incremental_partial_submitted sequence={} boundary={:?} audio_ms={}",
            id.0,
            plan.sequence,
            plan.boundary,
            audio_duration.as_millis()
        ),
        Err(error) => {
            planner.degrade(id);
            let _ = worker.cancel_incremental(id);
            eprintln!(
                "dictation_id={} state=Listening event=incremental_fallback reason=submit_failed error={error}",
                id.0
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn recover_runtime_after_resume(
    runtime: &mut AppRuntime<TargetSnapshot, ExtendedRecording>,
    overlay: &StatusOverlay,
    tray: &SystemTray,
    worker: &TranscriptionWorker,
    incremental: &mut IncrementalPlanner,
    microphone: Option<&str>,
    context: &DictationContext,
    capture: &ExtendedCaptureFactory,
) -> Result<()> {
    let runtime_id = runtime.active_id();
    let planner_id = incremental.cancel();
    if let Some(id) = runtime_id {
        let _ = worker.cancel_incremental(id);
    }
    if let Some(id) = planner_id.filter(|id| Some(*id) != runtime_id) {
        let _ = worker.cancel_incremental(id);
    }
    let mut io = ProductionIo {
        overlay,
        tray,
        worker,
        microphone,
        context,
        capture,
        released_at: None,
    };
    let notices = runtime
        .recover_after_system_resume(&mut io)
        .context("failed to recover dictation after system resume")?;
    report_notices(notices);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_test_dictation(
    runtime: &mut AppRuntime<TargetSnapshot, ExtendedRecording>,
    overlay: &StatusOverlay,
    tray: &SystemTray,
    worker: &TranscriptionWorker,
    microphone: Option<&str>,
    context: &mut DictationContext,
    settings: &Settings,
    capture: &ExtendedCaptureFactory,
) -> Result<()> {
    if runtime.state() == RuntimeState::Idle {
        *context = DictationContext::global(settings, context.verified_variant)?;
        runtime.configure_next_dictation(context.language.clone(), context.runtime_formatting)?;
    }
    let released_at = (runtime.state() == RuntimeState::Listening).then(Instant::now);
    if let (Some(released_at), Some(id)) = (released_at, runtime.active_id()) {
        log_release_received(id, released_at);
    }
    let mut io = ProductionIo {
        overlay,
        tray,
        worker,
        microphone,
        context,
        capture,
        released_at: None,
    };
    let notices = match runtime.state() {
        RuntimeState::Idle => runtime.hold_started(None, &mut io)?,
        RuntimeState::Listening => runtime.hold_ended_at(
            released_at.expect("listening test dictation has a release timestamp"),
            &mut io,
        )?,
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
            RuntimeNotice::RecordingStopped {
                id,
                audio_finalization_time,
            } => {
                eprintln!(
                    "uptime_ms={} dictation_id={} state=FinalizingAudio event=audio_finalized release_to_audio_finalized_ms={}",
                    monotonic_uptime_ms(),
                    id.0,
                    audio_finalization_time.as_millis()
                );
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
    eprintln!(
        "uptime_ms={} dictation_id={id} state={state:?} event={event}",
        monotonic_uptime_ms()
    );
}

fn log_release_received(id: DictationId, released_at: Instant) {
    let _ = released_at;
    eprintln!(
        "uptime_ms={} dictation_id={} state=FinalizingAudio event=release_received release_to_event_ms=0",
        monotonic_uptime_ms(),
        id.0
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalOutcome {
    Inserted,
    ClipboardReady,
    EmptyTranscript,
    InsertionFailed,
    Stale,
    OtherFailure,
}

impl TerminalOutcome {
    fn is_success(self) -> bool {
        matches!(self, Self::Inserted | Self::ClipboardReady)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::ClipboardReady => "clipboard_ready",
            Self::EmptyTranscript => "empty_transcript",
            Self::InsertionFailed => "insertion_failed",
            Self::Stale => "stale",
            Self::OtherFailure => "other_failure",
        }
    }
}

fn terminal_outcome<R>(notices: &[RuntimeNotice<R>]) -> TerminalOutcome {
    if notices
        .iter()
        .any(|notice| matches!(notice, RuntimeNotice::Inserted { .. }))
    {
        return TerminalOutcome::Inserted;
    }
    if notices
        .iter()
        .any(|notice| matches!(notice, RuntimeNotice::ClipboardReady { .. }))
    {
        return TerminalOutcome::ClipboardReady;
    }
    if notices
        .iter()
        .any(|notice| matches!(notice, RuntimeNotice::NoSpeech { .. }))
    {
        return TerminalOutcome::EmptyTranscript;
    }
    if notices.iter().any(|notice| {
        matches!(
            notice,
            RuntimeNotice::Failure {
                event: "insertion_failed",
                ..
            }
        )
    }) {
        return TerminalOutcome::InsertionFailed;
    }
    if notices
        .iter()
        .any(|notice| matches!(notice, RuntimeNotice::StaleTranscription { .. }))
    {
        return TerminalOutcome::Stale;
    }
    TerminalOutcome::OtherFailure
}

fn record_terminal_failure(id: DictationId, outcome: TerminalOutcome) {
    static EMPTY_TRANSCRIPTS: AtomicU64 = AtomicU64::new(0);
    static INSERTION_FAILURES: AtomicU64 = AtomicU64::new(0);
    static STALE_RESULTS: AtomicU64 = AtomicU64::new(0);
    static OTHER_FAILURES: AtomicU64 = AtomicU64::new(0);
    let counter = match outcome {
        TerminalOutcome::EmptyTranscript => &EMPTY_TRANSCRIPTS,
        TerminalOutcome::InsertionFailed => &INSERTION_FAILURES,
        TerminalOutcome::Stale => &STALE_RESULTS,
        TerminalOutcome::Inserted
        | TerminalOutcome::ClipboardReady
        | TerminalOutcome::OtherFailure => &OTHER_FAILURES,
    };
    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
    eprintln!(
        "uptime_ms={} dictation_id={} state=Idle event=release_terminal_failed category={} failure_count={count}",
        monotonic_uptime_ms(),
        id.0,
        outcome.label()
    );
}

fn monotonic_uptime_ms() -> u128 {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    STARTED.get_or_init(Instant::now).elapsed().as_millis()
}

fn whisper_backend_preference(value: AccurateBackendPreference) -> WhisperBackendPreference {
    match value {
        AccurateBackendPreference::Auto => WhisperBackendPreference::Auto,
        AccurateBackendPreference::Vulkan => WhisperBackendPreference::Vulkan,
        AccurateBackendPreference::Cpu => WhisperBackendPreference::Cpu,
    }
}

struct TranscriptionWorker {
    commands: Sender<WorkerCommand>,
    instant_audio: mpsc::SyncSender<InstantAudioCommand>,
    results: Receiver<WorkerEvent>,
    cancellations: Arc<CancellationRegistry>,
    shutting_down: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    readiness: Option<WhisperReadiness>,
}

#[derive(Default)]
struct InstantPump {
    active_id: Option<DictationId>,
    degraded: bool,
}

fn poll_instant_transcription(
    runtime: &mut AppRuntime<TargetSnapshot, ExtendedRecording>,
    pump: &mut InstantPump,
    worker: &TranscriptionWorker,
) {
    if runtime.state() != RuntimeState::Listening {
        if let Some(id) = pump.active_id.take()
            && !matches!(
                runtime.state(),
                RuntimeState::Transcribing
                    | RuntimeState::Normalizing
                    | RuntimeState::Cleaning
                    | RuntimeState::ReadyToInsert
                    | RuntimeState::Inserting
            )
        {
            let _ = worker.cancel_incremental(id);
        }
        return;
    }
    let Some(id) = runtime.active_id() else {
        return;
    };
    let Some(recording) = runtime.active_recording_mut() else {
        return;
    };
    if pump.active_id != Some(id) {
        if let Some(stale) = pump.active_id.replace(id) {
            let _ = worker.cancel_incremental(stale);
        }
        pump.degraded = false;
        worker.instant_begin(id, recording.sample_rate());
    }
    match recording.drain_streaming(16_384) {
        Ok(batch) => {
            worker.instant_audio(id, batch.samples, batch.sample_rate, batch.dropped_samples)
        }
        Err(error) => {
            if !pump.degraded {
                pump.degraded = true;
                worker.degrade_instant(id);
                eprintln!(
                    "dictation_id={} state=Listening event=instant_stream_degraded error={error}",
                    id.0
                );
            }
        }
    }
}

enum RecognizerConfig {
    Accurate {
        model: PathBuf,
        backend: WhisperBackendPreference,
    },
    Instant {
        runtime_bundle: PathBuf,
        model: PathBuf,
        language: String,
    },
}

enum LoadedRecognizer {
    Accurate(WhisperRecognizer),
    Instant(VoskModel),
}

struct InstantSession {
    recognizer: VoskSession,
    sample_rate: u32,
    accepted_samples: u64,
    inference_time: Duration,
    degraded: bool,
    continuity_prefix: Vec<f32>,
    checkpoint_count: u64,
}

enum InstantAudioCommand {
    Begin {
        id: DictationId,
        sample_rate: u32,
    },
    Audio {
        id: DictationId,
        samples: Vec<f32>,
        sample_rate: u32,
        dropped_samples: u64,
    },
    Cancel {
        id: DictationId,
    },
}

impl TranscriptionWorker {
    fn start(
        recognizer: RecognizerConfig,
        formatting: WorkerFormatting,
        aliases: Vec<LexiconEntry>,
    ) -> Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let (instant_audio_tx, instant_audio_rx) = mpsc::sync_channel(8);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let cancellations = Arc::new(CancellationRegistry::default());
        let worker_cancellations = Arc::clone(&cancellations);
        let worker_shutting_down = Arc::new(AtomicBool::new(false));
        let thread_shutting_down = Arc::clone(&worker_shutting_down);
        let thread = thread::Builder::new()
            .name("phorminx-transcription".to_owned())
            .spawn(move || {
                let recognizer = match recognizer {
                    RecognizerConfig::Accurate { model, backend } => {
                        WhisperRecognizer::load_with_backend(&model, backend)
                            .map(LoadedRecognizer::Accurate)
                            .map_err(|error| error.to_string())
                    }
                    RecognizerConfig::Instant {
                        runtime_bundle,
                        model,
                        language,
                    } => load_and_probe_resident(
                        || VoskModel::load(&runtime_bundle, &model, &language),
                        |model| {
                            // Loading the model alone does not prove that the native runtime can
                            // construct a recognizer. Probe that boundary before advertising Ready.
                            let probe = model.session(16_000)?;
                            drop(probe);
                            Ok(())
                        },
                    )
                        .map(LoadedRecognizer::Instant)
                        .map_err(|error| error.to_string()),
                };
                let recognizer = match recognizer {
                    Ok(recognizer) => recognizer,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                let readiness = match &recognizer {
                    LoadedRecognizer::Accurate(recognizer) => {
                        let readiness = recognizer.readiness();
                        eprintln!(
                            "dictation_id=0 state=Starting event=stt_backend_ready requested_backend={} active_backend={} fallback={} device_present={} model_load_ms={}",
                            readiness.requested.as_str(),
                            readiness.backend.as_str(),
                            readiness.fallback_from.is_some(),
                            readiness.device_name.is_some(),
                            readiness.model_load_time.as_millis()
                        );
                        Some(readiness)
                    }
                    LoadedRecognizer::Instant(model) => {
                        eprintln!(
                            "dictation_id=0 state=Starting event=stt_backend_ready backend=vosk device_present=false model_load_ms={}",
                            model.load_time().as_millis()
                        );
                        None
                    }
                };
                if ready_tx.send(Ok(readiness)).is_err() {
                    return;
                }

                let ollama = formatting.model.as_ref().map(|_| production_ollama_client());
                let mut aliases = aliases;
                let mut incremental_sessions = HashMap::new();
                let mut instant_sessions = HashMap::new();
                if formatting.uses_ollama()
                    && let (Some(client), Some(model)) = (&ollama, &formatting.model)
                {
                    let client = client.clone();
                    let model = model.clone();
                    let keep_alive = formatting.keep_alive.clone();
                    let _ = thread::Builder::new()
                        .name("phorminx-ollama-warmup".to_owned())
                        .spawn(move || {
                            let cancel = CancellationToken::new();
                            let event = match client.warm_up(&model, keep_alive, &cancel) {
                                Ok(()) => "ollama_warmup_ready",
                                Err(error) => {
                                    eprintln!(
                                        "dictation_id=0 state=Starting event=ollama_warmup_degraded error={error}"
                                    );
                                    return;
                                }
                            };
                            eprintln!("dictation_id=0 state=Starting event={event}");
                        });
                }

                loop {
                    drain_instant_audio(
                        &recognizer,
                        &instant_audio_rx,
                        &mut instant_sessions,
                    );
                    let command = match command_rx.recv_timeout(Duration::from_millis(10)) {
                        Ok(command) => command,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    if thread_shutting_down.load(Ordering::Acquire) {
                        match command {
                            WorkerCommand::Shutdown => break,
                            WorkerCommand::CancelIncremental { id } => {
                                incremental_sessions.remove(&id);
                                worker_cancellations.acknowledge(id);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match command {
                        WorkerCommand::TranscribePartial {
                            plan,
                            clip,
                            language,
                        } => {
                            if worker_job_cancelled(
                                &worker_cancellations,
                                &thread_shutting_down,
                                plan.id,
                            ) {
                                incremental_sessions.remove(&plan.id);
                                if result_tx
                                    .send(WorkerEvent::PartialCompleted {
                                        id: plan.id,
                                        sequence: plan.sequence,
                                        succeeded: false,
                                        compute_time: Duration::ZERO,
                                        audio_duration: clip.duration(),
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                            let LoadedRecognizer::Accurate(recognizer) = &recognizer else {
                                continue;
                            };
                            let partial_id = plan.id;
                            let partial_abort = worker_cancellations.begin_partial(partial_id);
                            let event = process_partial_transcription(
                                recognizer,
                                &mut incremental_sessions,
                                plan,
                                clip,
                                &language,
                                &partial_abort,
                            );
                            worker_cancellations.end_partial(partial_id);
                            if result_tx.send(event).is_err() {
                                break;
                            }
                        }
                        WorkerCommand::Transcribe {
                            id,
                            released_at,
                            audio,
                            language,
                            audio_context,
                            formatting: dictation_formatting,
                            app_executable,
                            timing,
                        } => {
                            // Clear the release-priority tombstone only when
                            // the final command reaches the head of the FIFO.
                            // Every older queued partial therefore observes an
                            // already-aborted flag in `begin_partial`.
                            worker_cancellations.begin_final(id);
                            let worker_started_at = Instant::now();
                            if worker_job_cancelled(
                                &worker_cancellations,
                                &thread_shutting_down,
                                id,
                            ) {
                                incremental_sessions.remove(&id);
                                continue;
                            }
                            let pre_stt_time = elapsed_since_release(released_at, Instant::now());
                            drain_instant_audio(
                                &recognizer,
                                &instant_audio_rx,
                                &mut instant_sessions,
                            );
                            let stats = audio.stats();
                            let incremental = incremental_sessions.remove(&id);
                            let instant = instant_sessions.remove(&id);
                            let checkpoint_count = match &recognizer {
                                LoadedRecognizer::Accurate(_) => incremental
                                    .as_ref()
                                    .map_or(0, |session| u64::from(session.next_sequence)),
                                LoadedRecognizer::Instant(_) => instant
                                    .as_ref()
                                    .map_or(0, |session| session.checkpoint_count),
                            };
                            let repair_count = match (&recognizer, &instant) {
                                (LoadedRecognizer::Instant(_), Some(session)) if session.degraded => 1,
                                (LoadedRecognizer::Accurate(_), _)
                                    if incremental
                                        .as_ref()
                                        .is_none_or(|session| session.degraded.is_some()) =>
                                {
                                    1
                                }
                                _ => 0,
                            };
                            let recognition = (*audio).resolve().and_then(|mut audio| {
                                let recognition = match (&recognizer, &mut audio) {
                                    (LoadedRecognizer::Accurate(recognizer), FinalAudioSource::Short { clip, .. }) => transcribe_final(
                                        recognizer, incremental, clip, &language, audio_context, id,
                                    ),
                                    (LoadedRecognizer::Accurate(recognizer), FinalAudioSource::Extended { audio, stats }) => transcribe_extended_accurate(
                                        recognizer, incremental, audio, *stats, &language, id,
                                    ),
                                    (LoadedRecognizer::Instant(model), FinalAudioSource::Short { clip, .. }) => transcribe_instant_final(
                                        model, instant, clip, id,
                                    ).map_err(|error| error.to_string()),
                                    (LoadedRecognizer::Instant(model), FinalAudioSource::Extended { audio, stats }) => transcribe_extended_instant(
                                        model, instant, audio, *stats, id,
                                    ).map_err(|error| error.to_string()),
                                    (_, FinalAudioSource::Pending { .. }) => Err("final audio did not resolve".to_owned()),
                                };
                                let cleanup = audio.cleanup();
                                require_audio_cleanup(recognition, cleanup)
                            });
                            let result = match recognition {
                                Ok(transcript) => {
                                    if has_pathological_repetition(&transcript.text) {
                                        eprintln!(
                                            "dictation_id={} state=Transcribing event=transcription_rejected reason=repetition_loop",
                                            id.0
                                        );
                                        Err("Whisper produced a repeated output loop".to_owned())
                                    } else {
                                    // Whisper cannot be preempted safely today,
                                    // so cancellation is checked again before
                                    // any cleanup event or potentially long
                                    // Ollama pass begins.
                                    if worker_job_cancelled(
                                        &worker_cancellations,
                                        &thread_shutting_down,
                                        id,
                                    ) {
                                        incremental_sessions.remove(&id);
                                        continue;
                                    }
                                    if dictation_formatting.uses_ollama()
                                        && result_tx
                                            .send(WorkerEvent::CleanupStarted { id })
                                            .is_err()
                                    {
                                        break;
                                    }
                                    if worker_job_cancelled(
                                        &worker_cancellations,
                                        &thread_shutting_down,
                                        id,
                                    ) {
                                        incremental_sessions.remove(&id);
                                        continue;
                                    }
                                    let formatting_cancel =
                                        worker_cancellations.begin_formatting(id);
                                    if worker_job_cancelled(
                                        &worker_cancellations,
                                        &thread_shutting_down,
                                        id,
                                    ) {
                                        formatting_cancel.cancel();
                                        worker_cancellations.end_formatting(id);
                                        incremental_sessions.remove(&id);
                                        continue;
                                    }
                                    let mut processed = process_transcript(
                                        transcript,
                                        &language,
                                        &dictation_formatting,
                                        ollama.as_ref(),
                                        &aliases,
                                        app_executable.as_deref(),
                                        &formatting_cancel,
                                    );
                                    processed.terminal.checkpoint_count = Some(checkpoint_count);
                                    processed.terminal.checkpoint_repair_count = Some(repair_count);
                                    processed.terminal.peak_retained_audio_ms = Some(
                                        stats.peak_retained_samples.saturating_mul(1_000) / 16_000,
                                    );
                                    processed.terminal.auto_stopped = Some(stats.auto_stopped);
                                    processed.pre_stt_time = pre_stt_time;
                                    eprintln!(
                                        "dictation_id={} state=Cleaning event=release_pipeline_stages pre_stt_ms={} stt_compute_ms={} formatting_ms={}",
                                        id.0,
                                        processed.pre_stt_time.as_millis(),
                                        processed.transcript.inference_time.as_millis(),
                                        processed.formatting_time.as_millis()
                                    );
                                    processed.lifecycle = Some(LifecycleTiming {
                                        release: timing,
                                        worker_started_at,
                                        worker_completed_at: Instant::now(),
                                    });
                                    worker_cancellations.end_formatting(id);
                                    if worker_job_cancelled(
                                        &worker_cancellations,
                                        &thread_shutting_down,
                                        id,
                                    ) {
                                        incremental_sessions.remove(&id);
                                        continue;
                                    }
                                    Ok(processed)
                                    }
                                }
                                Err(error) => Err(error.to_string()),
                            };
                            if result_tx
                                .send(WorkerEvent::Completed(Box::new(WorkerResult {
                                    id,
                                    result,
                                })))
                                .is_err()
                            {
                                break;
                            }
                        }
                        WorkerCommand::CancelIncremental { id } => {
                            incremental_sessions.remove(&id);
                            instant_sessions.remove(&id);
                            worker_cancellations.acknowledge(id);
                        }
                        WorkerCommand::DegradeInstant { id } => {
                            if let Some(session) = instant_sessions.get_mut(&id) {
                                session.degraded = true;
                            } else if let LoadedRecognizer::Instant(model) = &recognizer
                                && let Ok(recognizer) = model.session(16_000)
                            {
                                instant_sessions.insert(
                                    id,
                                    InstantSession {
                                        recognizer,
                                        sample_rate: 16_000,
                                        accepted_samples: 0,
                                        inference_time: Duration::ZERO,
                                        degraded: true,
                                        continuity_prefix: Vec::new(),
                                        checkpoint_count: 0,
                                    },
                                );
                            }
                        }
                        WorkerCommand::ReloadAliases(updated) => aliases = updated,
                        WorkerCommand::Shutdown => break,
                    }
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(readiness)) => Ok(Self {
                commands: command_tx,
                instant_audio: instant_audio_tx,
                results: result_rx,
                cancellations,
                shutting_down: worker_shutting_down,
                thread: Some(thread),
                readiness,
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

    fn transcribe(&self, request: TranscriptionRequest) -> Result<()> {
        let TranscriptionRequest {
            id,
            released_at,
            audio,
            language,
            audio_context,
            formatting,
            app_executable,
            timing,
        } = request;
        // Publish release priority before the FIFO send. A running partial sees
        // this through whisper.cpp's abort callback and yields to the final.
        self.cancellations.prioritize_final(id);
        if self
            .commands
            .send(WorkerCommand::Transcribe {
                id,
                released_at,
                audio: Box::new(audio),
                language,
                audio_context,
                formatting,
                app_executable,
                timing,
            })
            .is_err()
        {
            self.cancellations.clear_final_priority(id);
            return Err(anyhow!("transcription worker is unavailable"));
        }
        Ok(())
    }

    fn readiness(&self) -> Option<&WhisperReadiness> {
        self.readiness.as_ref()
    }

    fn transcribe_partial(
        &self,
        plan: phorminx_app::incremental::ChunkPlan,
        clip: AudioClip,
        language: String,
    ) -> Result<()> {
        self.commands
            .send(WorkerCommand::TranscribePartial {
                plan,
                clip,
                language,
            })
            .map_err(|_| anyhow!("transcription worker is unavailable"))
    }

    fn instant_begin(&self, id: DictationId, sample_rate: u32) {
        if self
            .instant_audio
            .try_send(InstantAudioCommand::Begin { id, sample_rate })
            .is_err()
        {
            let _ = self.commands.send(WorkerCommand::DegradeInstant { id });
        }
    }

    fn instant_audio(
        &self,
        id: DictationId,
        samples: Vec<f32>,
        sample_rate: u32,
        dropped_samples: u64,
    ) {
        if samples.is_empty() {
            return;
        }
        if self
            .instant_audio
            .try_send(InstantAudioCommand::Audio {
                id,
                samples,
                sample_rate,
                dropped_samples,
            })
            .is_err()
        {
            let _ = self.commands.send(WorkerCommand::DegradeInstant { id });
        }
    }

    fn degrade_instant(&self, id: DictationId) {
        let _ = self.commands.send(WorkerCommand::DegradeInstant { id });
    }

    fn cancel_incremental(&self, id: DictationId) -> Result<()> {
        // Cancellation is visible before this FIFO command reaches the worker,
        // so queued stale audio is dropped without another Whisper invocation.
        self.cancellations.cancel(id);
        let _ = self
            .instant_audio
            .try_send(InstantAudioCommand::Cancel { id });
        self.commands
            .send(WorkerCommand::CancelIncremental { id })
            .map_err(|_| anyhow!("transcription worker is unavailable"))
    }

    fn shutdown(mut self) -> Result<()> {
        self.stop()
    }

    fn reload_aliases(&self, aliases: Vec<LexiconEntry>) -> Result<()> {
        self.commands
            .send(WorkerCommand::ReloadAliases(aliases))
            .map_err(|_| anyhow!("transcription worker is unavailable"))
    }

    fn stop(&mut self) -> Result<()> {
        if self.thread.is_none() {
            return Ok(());
        }
        self.shutting_down.store(true, Ordering::Release);
        self.cancellations.cancel_active_formatting();
        let _ = self.commands.send(WorkerCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow!("transcription worker panicked"))?;
        }
        Ok(())
    }
}

fn load_and_probe_resident<T, E>(
    loader: impl FnOnce() -> Result<T, E>,
    probe: impl FnOnce(&T) -> Result<(), E>,
) -> Result<T, E> {
    let resident = loader()?;
    probe(&resident)?;
    Ok(resident)
}

struct TranscriptionRequest {
    id: DictationId,
    released_at: Instant,
    audio: FinalAudioSource,
    language: String,
    audio_context: u32,
    formatting: WorkerFormatting,
    app_executable: Option<String>,
    timing: ReleaseTiming,
}

struct CancellationRegistry {
    state: Mutex<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    ids: HashSet<DictationId>,
    final_priorities: HashSet<DictationId>,
    formatting: HashMap<DictationId, CancellationToken>,
    partials: HashMap<DictationId, Arc<AtomicBool>>,
}

impl Default for CancellationRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::new(CancellationState::default()),
        }
    }
}

impl CancellationRegistry {
    fn cancel(&self, id: DictationId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.ids.insert(id);
        if let Some(token) = state.formatting.get(&id) {
            token.cancel();
        }
        if let Some(abort) = state.partials.get(&id) {
            abort.store(true, Ordering::Release);
        }
    }

    fn is_cancelled(&self, id: DictationId) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ids
            .contains(&id)
    }

    fn acknowledge(&self, id: DictationId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.ids.remove(&id);
        state.final_priorities.remove(&id);
        state.formatting.remove(&id);
        state.partials.remove(&id);
    }

    fn begin_partial(&self, id: DictationId) -> Arc<AtomicBool> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let abort = Arc::new(AtomicBool::new(
            state.ids.contains(&id) || state.final_priorities.contains(&id),
        ));
        state.partials.insert(id, Arc::clone(&abort));
        abort
    }

    fn end_partial(&self, id: DictationId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .partials
            .remove(&id);
    }

    fn prioritize_final(&self, id: DictationId) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.final_priorities.insert(id);
        if let Some(abort) = state.partials.get(&id) {
            abort.store(true, Ordering::Release);
        }
    }

    fn begin_final(&self, id: DictationId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .final_priorities
            .remove(&id);
    }

    fn clear_final_priority(&self, id: DictationId) {
        self.begin_final(id);
    }

    fn begin_formatting(&self, id: DictationId) -> CancellationToken {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let token = CancellationToken::new();
        if state.ids.contains(&id) {
            token.cancel();
        }
        state.formatting.insert(id, token.clone());
        token
    }

    fn end_formatting(&self, id: DictationId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .formatting
            .remove(&id);
    }

    fn cancel_active_formatting(&self) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for token in state.formatting.values() {
            token.cancel();
        }
        for abort in state.partials.values() {
            abort.store(true, Ordering::Release);
        }
    }
}

fn worker_job_cancelled(
    cancellations: &CancellationRegistry,
    shutting_down: &AtomicBool,
    id: DictationId,
) -> bool {
    shutting_down.load(Ordering::Acquire) || cancellations.is_cancelled(id)
}

impl Drop for TranscriptionWorker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

enum WorkerCommand {
    TranscribePartial {
        plan: phorminx_app::incremental::ChunkPlan,
        clip: AudioClip,
        language: String,
    },
    Transcribe {
        id: DictationId,
        released_at: Instant,
        audio: Box<FinalAudioSource>,
        language: String,
        audio_context: u32,
        formatting: WorkerFormatting,
        app_executable: Option<String>,
        timing: ReleaseTiming,
    },
    CancelIncremental {
        id: DictationId,
    },
    DegradeInstant {
        id: DictationId,
    },
    ReloadAliases(Vec<LexiconEntry>),
    Shutdown,
}

struct WorkerResult {
    id: DictationId,
    result: Result<ProcessedTranscript, String>,
}

enum WorkerEvent {
    PartialCompleted {
        id: DictationId,
        sequence: u32,
        succeeded: bool,
        compute_time: Duration,
        audio_duration: Duration,
    },
    CleanupStarted {
        id: DictationId,
    },
    Completed(Box<WorkerResult>),
}

struct PartialAccumulator {
    text: String,
    stable_end: Duration,
    accepted_through: Duration,
    /// Earliest audio that was observed but could not be accepted
    /// contiguously. Once present, later partials cannot advance past it and
    /// the final pass resumes here.
    unresolved_from: Option<Duration>,
    timestamp_stable: bool,
    last_boundary: Option<BoundaryKind>,
    next_sequence: u32,
    partial_compute_time: Duration,
    model_load_time: Duration,
    degraded: Option<&'static str>,
}

fn drain_instant_audio(
    recognizer: &LoadedRecognizer,
    receiver: &Receiver<InstantAudioCommand>,
    sessions: &mut HashMap<DictationId, InstantSession>,
) {
    let LoadedRecognizer::Instant(model) = recognizer else {
        while receiver.try_recv().is_ok() {}
        return;
    };
    while let Ok(command) = receiver.try_recv() {
        match command {
            InstantAudioCommand::Begin { id, sample_rate } => {
                if sessions.contains_key(&id) {
                    continue;
                }
                match model.session(sample_rate) {
                    Ok(recognizer) => {
                        sessions.insert(
                            id,
                            InstantSession {
                                recognizer,
                                sample_rate,
                                accepted_samples: 0,
                                inference_time: Duration::ZERO,
                                degraded: false,
                                continuity_prefix: Vec::new(),
                                checkpoint_count: 0,
                            },
                        );
                    }
                    Err(_) => {
                        eprintln!(
                            "dictation_id={} state=Listening event=instant_session_degraded reason=create_failed",
                            id.0
                        );
                    }
                }
            }
            InstantAudioCommand::Audio {
                id,
                samples,
                sample_rate,
                dropped_samples,
            } => {
                let Some(session) = sessions.get_mut(&id) else {
                    continue;
                };
                if sample_rate != session.sample_rate || dropped_samples != 0 {
                    session.degraded = true;
                    continue;
                }
                if session.degraded {
                    continue;
                }
                let started = Instant::now();
                let accepted = session.recognizer.accept_f32(&samples);
                session.inference_time += started.elapsed();
                match accepted {
                    Ok(_) => {
                        let remaining = 2_048usize.saturating_sub(session.continuity_prefix.len());
                        session
                            .continuity_prefix
                            .extend_from_slice(&samples[..samples.len().min(remaining)]);
                        session.accepted_samples += samples.len() as u64;
                        session.checkpoint_count = session.checkpoint_count.saturating_add(1);
                    }
                    Err(_) => session.degraded = true,
                }
            }
            InstantAudioCommand::Cancel { id } => {
                sessions.remove(&id);
            }
        }
    }
}

fn transcribe_instant_final(
    model: &VoskModel,
    session: Option<InstantSession>,
    clip: &AudioClip,
    id: DictationId,
) -> Result<Transcript, phorminx_vosk::VoskError> {
    let mut session = match session {
        Some(session)
            if !session.degraded
                && instant_stream_is_continuous(
                    session.accepted_samples,
                    session.sample_rate,
                    &session.continuity_prefix,
                    clip,
                ) =>
        {
            session
        }
        _ => {
            eprintln!(
                "dictation_id={} state=Transcribing event=instant_recovery path=full_clip",
                id.0
            );
            return transcribe_instant_full_clip(model, clip);
        }
    };

    let accepted_at_clip_rate = tail_start_at_clip_rate(
        session.accepted_samples,
        session.sample_rate,
        clip.sample_rate,
    );
    if accepted_at_clip_rate < clip.samples.len() {
        let native_tail = phorminx_audio::resample(
            &clip.samples[accepted_at_clip_rate..],
            clip.sample_rate,
            session.sample_rate,
        )
        .map_err(|_| phorminx_vosk::VoskError::DecoderFailed)?;
        let started = Instant::now();
        session.recognizer.accept_f32(&native_tail)?;
        session.inference_time += started.elapsed();
    }
    let started = Instant::now();
    let text = session.recognizer.finish()?;
    session.inference_time += started.elapsed();
    if text.trim().is_empty() && clip.rms() >= 0.01 {
        eprintln!(
            "dictation_id={} state=Transcribing event=instant_recovery path=full_clip reason=empty_high_energy",
            id.0
        );
        return transcribe_instant_full_clip(model, clip);
    }
    Ok(Transcript {
        text,
        backend: "vosk",
        model_load_time: model.load_time(),
        inference_time: session.inference_time,
        audio_duration: clip.duration(),
    })
}

fn transcribe_instant_full_clip(
    model: &VoskModel,
    clip: &AudioClip,
) -> Result<Transcript, phorminx_vosk::VoskError> {
    let mut recognizer = model.session(clip.sample_rate)?;
    let started = Instant::now();
    recognizer.accept_f32(&clip.samples)?;
    let text = recognizer.finish()?;
    Ok(Transcript {
        text,
        backend: "vosk",
        model_load_time: model.load_time(),
        inference_time: started.elapsed(),
        audio_duration: clip.duration(),
    })
}

fn instant_stream_is_continuous(
    accepted_samples: u64,
    accepted_sample_rate: u32,
    continuity_prefix: &[f32],
    clip: &AudioClip,
) -> bool {
    if accepted_sample_rate == 0 {
        return false;
    }
    let maximum_native_samples = (clip.samples.len() as u128)
        .saturating_mul(u128::from(accepted_sample_rate))
        .div_ceil(u128::from(clip.sample_rate));
    let edge_tolerance = 2;
    if u128::from(accepted_samples) > maximum_native_samples.saturating_add(edge_tolerance) {
        return false;
    }
    if accepted_samples == 0 {
        return continuity_prefix.is_empty();
    }
    if continuity_prefix.is_empty() {
        return false;
    }
    let Ok(expected) =
        phorminx_audio::resample(&clip.samples, clip.sample_rate, accepted_sample_rate)
    else {
        return false;
    };
    let length = continuity_prefix.len().min(expected.len());
    if length < 64 {
        return false;
    }
    let observed = &continuity_prefix[..length];
    let expected = &expected[..length];
    let observed_energy = observed.iter().map(|sample| sample * sample).sum::<f32>();
    let expected_energy = expected.iter().map(|sample| sample * sample).sum::<f32>();
    if observed_energy < 1e-6 && expected_energy < 1e-6 {
        return true;
    }
    if observed_energy < 1e-6 || expected_energy < 1e-6 {
        return false;
    }
    let correlation = observed
        .iter()
        .zip(expected)
        .map(|(left, right)| left * right)
        .sum::<f32>()
        / (observed_energy * expected_energy).sqrt();
    correlation >= 0.70
}

fn tail_start_at_clip_rate(
    accepted_samples: u64,
    accepted_sample_rate: u32,
    clip_sample_rate: u32,
) -> usize {
    let accepted_seconds = accepted_samples as f64 / f64::from(accepted_sample_rate);
    (accepted_seconds * f64::from(clip_sample_rate)).floor() as usize
}

fn transcribe_extended_instant(
    model: &VoskModel,
    session: Option<InstantSession>,
    audio: &mut ExtendedCapturedAudio,
    stats: FinalAudioStats,
    id: DictationId,
) -> Result<Transcript, String> {
    let continuous = session.as_ref().is_some_and(|session| {
        !session.degraded
            && session.sample_rate == 16_000
            && session.accepted_samples <= stats.total_samples
            && extended_stream_prefix_matches(session, audio).unwrap_or(false)
    });
    if !continuous {
        eprintln!(
            "dictation_id={} state=Transcribing event=instant_recovery path=bounded_spool",
            id.0
        );
        return transcribe_instant_sequential(model, audio, stats);
    }

    let mut session = session.expect("continuous extended session exists");
    if session.accepted_samples < stats.total_samples {
        let range = SampleRange::new(session.accepted_samples, stats.total_samples)
            .map_err(|error| error.to_string())?;
        let tail = audio.snapshot(range).map_err(|error| error.to_string())?;
        let started = Instant::now();
        session
            .recognizer
            .accept_f32(tail.samples())
            .map_err(|error| error.to_string())?;
        session.inference_time = session.inference_time.saturating_add(started.elapsed());
    }
    let started = Instant::now();
    let text = session
        .recognizer
        .finish()
        .map_err(|error| error.to_string())?;
    session.inference_time = session.inference_time.saturating_add(started.elapsed());
    if text.trim().is_empty() && stats.rms >= 0.01 {
        eprintln!(
            "dictation_id={} state=Transcribing event=instant_recovery path=bounded_spool reason=empty_high_energy",
            id.0
        );
        return transcribe_instant_sequential(model, audio, stats);
    }
    Ok(Transcript {
        text,
        backend: "vosk",
        model_load_time: model.load_time(),
        inference_time: session.inference_time,
        audio_duration: Duration::from_secs_f64(stats.total_samples as f64 / 16_000.0),
    })
}

fn extended_stream_prefix_matches(
    session: &InstantSession,
    audio: &mut ExtendedCapturedAudio,
) -> Result<bool, String> {
    if session.accepted_samples == 0 {
        return Ok(session.continuity_prefix.is_empty());
    }
    let length = session.continuity_prefix.len().min(2_048);
    if length < 64 {
        return Ok(false);
    }
    let span = audio
        .snapshot(SampleRange::new(0, length as u64).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let observed = &session.continuity_prefix[..length];
    let expected = span.samples();
    let observed_energy = observed.iter().map(|sample| sample * sample).sum::<f32>();
    let expected_energy = expected.iter().map(|sample| sample * sample).sum::<f32>();
    if observed_energy < 1e-6 || expected_energy < 1e-6 {
        return Ok(observed_energy < 1e-6 && expected_energy < 1e-6);
    }
    let correlation = observed
        .iter()
        .zip(expected)
        .map(|(left, right)| left * right)
        .sum::<f32>()
        / (observed_energy * expected_energy).sqrt();
    Ok(correlation >= 0.70)
}

fn transcribe_instant_sequential(
    model: &VoskModel,
    audio: &mut ExtendedCapturedAudio,
    stats: FinalAudioStats,
) -> Result<Transcript, String> {
    const RECOVERY_CHUNK_SAMPLES: u64 = 16_000 * 30;
    let mut recognizer = model.session(16_000).map_err(|error| error.to_string())?;
    let started = Instant::now();
    for_each_bounded_range(stats.total_samples, RECOVERY_CHUNK_SAMPLES, |range| {
        let span = audio.snapshot(range).map_err(|error| error.to_string())?;
        recognizer
            .accept_f32(span.samples())
            .map_err(|error| error.to_string())?;
        Ok(())
    })?;
    let text = recognizer.finish().map_err(|error| error.to_string())?;
    Ok(Transcript {
        text,
        backend: "vosk",
        model_load_time: model.load_time(),
        inference_time: started.elapsed(),
        audio_duration: Duration::from_secs_f64(stats.total_samples as f64 / 16_000.0),
    })
}

fn for_each_bounded_range(
    total_samples: u64,
    maximum_samples: u64,
    mut visit: impl FnMut(SampleRange) -> Result<(), String>,
) -> Result<(), String> {
    if maximum_samples == 0 {
        return Err("bounded audio traversal requires a non-zero chunk size".to_owned());
    }
    let mut cursor = 0;
    while cursor < total_samples {
        let end = total_samples.min(cursor.saturating_add(maximum_samples));
        visit(SampleRange::new(cursor, end).map_err(|error| error.to_string())?)?;
        cursor = end;
    }
    Ok(())
}

impl Default for PartialAccumulator {
    fn default() -> Self {
        Self {
            text: String::new(),
            stable_end: Duration::ZERO,
            accepted_through: Duration::ZERO,
            unresolved_from: None,
            timestamp_stable: false,
            last_boundary: None,
            next_sequence: 0,
            partial_compute_time: Duration::ZERO,
            model_load_time: Duration::ZERO,
            degraded: None,
        }
    }
}

fn process_partial_transcription(
    recognizer: &WhisperRecognizer,
    sessions: &mut HashMap<DictationId, PartialAccumulator>,
    plan: phorminx_app::incremental::ChunkPlan,
    clip: AudioClip,
    language: &str,
    abort: &Arc<AtomicBool>,
) -> WorkerEvent {
    let audio_duration = clip.duration();
    let accumulator = sessions.entry(plan.id).or_default();
    let mut compute_time = Duration::ZERO;
    let succeeded = if accumulator.degraded.is_some() {
        false
    } else if plan.sequence != accumulator.next_sequence {
        accumulator.degraded = Some("sequence_gap");
        false
    } else if (plan.sequence == 0) != plan.start_overlap.is_none() {
        accumulator.degraded = Some("boundary_mismatch");
        false
    } else {
        let options = TranscriptionOptions {
            language: Some(language),
            thread_count: None,
            audio_context: None,
        };
        let prompt = bounded_decoder_prompt(&accumulator.text);
        let (recognition, measured) = WallComputeClock.measure(|| {
            recognizer.transcribe_detailed(&clip, &options, prompt.as_deref(), Some(abort))
        });
        compute_time = measured;
        accumulator.partial_compute_time = accumulator
            .partial_compute_time
            .saturating_add(compute_time);
        match recognition {
            Ok(mut detailed) => {
                let transcript = &mut detailed.transcript;
                transcript.text = strip_known_non_speech_annotations(&transcript.text);
                append_timestamp_stable_segments(accumulator, plan, &detailed.segments);
                accumulator.timestamp_stable = true;
                accumulator.stable_end = plan.stable_end;
                // Text was admitted by absolute timestamp and ends no later
                // than the tail start, so final assembly concatenates rather
                // than requiring a duplicated lexical overlap.
                accumulator.last_boundary = Some(BoundaryKind::Silence);
                accumulator.next_sequence = accumulator.next_sequence.saturating_add(1);
                accumulator.model_load_time = transcript.model_load_time;
                true
            }
            Err(WhisperError::Aborted) => false,
            Err(_) => {
                accumulator.degraded = Some("partial_recognition_failed");
                false
            }
        }
    };

    WorkerEvent::PartialCompleted {
        id: plan.id,
        sequence: plan.sequence,
        succeeded,
        compute_time,
        audio_duration,
    }
}

fn append_timestamp_stable_segments(
    accumulator: &mut PartialAccumulator,
    plan: phorminx_app::incremental::ChunkPlan,
    segments: &[phorminx_whisper::TimedSegment],
) {
    // Once Whisper changes a boundary across the accepted frontier, only the
    // final pass may resolve it. Admitting later segments would create a
    // permanent hole while moving `accepted_through` beyond missing audio.
    if accumulator.unresolved_from.is_some() {
        return;
    }
    let guarded_end = match plan.boundary {
        BoundaryKind::Silence => plan.stable_end,
        BoundaryKind::Forced => plan.stable_end.saturating_sub(CHUNK_OVERLAP),
    };
    for segment in segments {
        let absolute_start = plan.range.start.saturating_add(segment.start);
        let absolute_end = plan.range.start.saturating_add(segment.end);
        if absolute_end <= accumulator.accepted_through {
            continue;
        }
        // A changed segmentation boundary can straddle already accepted audio.
        // Freeze the frontier at the first unresolved instant instead of
        // skipping forward and losing the unaccepted suffix.
        if absolute_start < accumulator.accepted_through {
            accumulator.unresolved_from = Some(accumulator.accepted_through);
            break;
        }
        // A timestamp jump is not proof that the skipped audio was silent.
        // Freeze at the last contiguous frontier so the final pass re-reads
        // every unproven sample from the canonical capture source.
        if absolute_start > accumulator.accepted_through || absolute_end <= absolute_start {
            accumulator.unresolved_from = Some(accumulator.accepted_through);
            break;
        }
        // Segments beyond the stability guard are deliberately deferred. The
        // final tail must resume at the accepted frontier before later
        // partials can be admitted; timestamps may contain silent gaps before
        // this segment, and Accurate mode never assumes those gaps are empty.
        if absolute_end > guarded_end {
            accumulator.unresolved_from = Some(accumulator.accepted_through);
            break;
        }
        let text = strip_known_non_speech_annotations(&segment.text);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        append_with_spacing(&mut accumulator.text, text);
        accumulator.accepted_through = accumulator.accepted_through.max(absolute_end);
    }
}

fn append_with_spacing(output: &mut String, value: &str) {
    if value.is_empty() {
        return;
    }
    if !output.is_empty()
        && !output.ends_with(char::is_whitespace)
        && !value.starts_with(|character: char| {
            character.is_whitespace() || ".,!?;:".contains(character)
        })
    {
        output.push(' ');
    }
    output.push_str(value);
}

fn bounded_decoder_prompt(text: &str) -> Option<String> {
    const MAX_PROMPT_CHARS: usize = 512;
    if text.trim().is_empty() {
        return None;
    }
    let mut start = text.len();
    for (count, (index, _)) in text.char_indices().rev().enumerate() {
        if count == MAX_PROMPT_CHARS {
            break;
        }
        start = index;
    }
    Some(text[start..].to_owned())
}

/// Detects high-confidence decoder loops without retaining or logging content.
/// Short intentional repetition is valid speech, so only a long phrase
/// repeated at least four consecutive times is classified as pathological.
fn has_pathological_repetition(text: &str) -> bool {
    const MIN_SPAN_WORDS: usize = 5;
    const REPEATS: usize = 4;
    const MAX_SPAN_WORDS: usize = 48;

    let words = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| !character.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let maximum_span = (words.len() / REPEATS).min(MAX_SPAN_WORDS);
    for span in MIN_SPAN_WORDS..=maximum_span {
        let repeated_width = span * REPEATS;
        for start in 0..=words.len().saturating_sub(repeated_width) {
            let candidate = &words[start..start + span];
            if (1..REPEATS).all(|repeat| {
                let offset = start + repeat * span;
                words[offset..offset + span] == *candidate
            }) {
                return true;
            }
        }
    }
    false
}

fn transcribe_extended_accurate<R>(
    recognizer: &R,
    incremental: Option<PartialAccumulator>,
    audio: &mut ExtendedCapturedAudio,
    stats: FinalAudioStats,
    language: &str,
    id: DictationId,
) -> Result<Transcript, String>
where
    R: SpeechRecognizer,
{
    transcribe_extended_accurate_with_source(
        recognizer,
        incremental,
        stats,
        language,
        id,
        |range| {
            let span = audio.snapshot(range).map_err(|error| error.to_string())?;
            audio_span_to_clip(span).map_err(|error| error.to_string())
        },
    )
}

fn transcribe_extended_accurate_with_source<R>(
    recognizer: &R,
    incremental: Option<PartialAccumulator>,
    stats: FinalAudioStats,
    language: &str,
    id: DictationId,
    mut snapshot: impl FnMut(SampleRange) -> Result<AudioClip, String>,
) -> Result<Transcript, String>
where
    R: SpeechRecognizer,
{
    let full_duration = Duration::from_secs_f64(stats.total_samples as f64 / 16_000.0);
    if let Some(accumulator) = incremental {
        let tail_attempt = (|| {
            let plan = final_tail_plan(&accumulator, full_duration).map_err(str::to_owned)?;
            let start_sample = (plan.start.as_secs_f64() * 16_000.0).floor() as u64;
            if start_sample >= stats.total_samples {
                return Err("extended Accurate capture has an empty final tail".to_owned());
            }
            let tail = snapshot(
                SampleRange::new(start_sample, stats.total_samples)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| format!("extended Accurate final tail is not recoverable: {error}"))?;
            let options = TranscriptionOptions {
                language: Some(language),
                thread_count: None,
                audio_context: None,
            };
            let (recognition, tail_compute_time) =
                WallComputeClock.measure(|| recognizer.transcribe(&tail, &options));
            let mut tail_transcript = recognition.map_err(|error| error.to_string())?;
            tail_transcript.inference_time = tail_compute_time;
            suppress_non_speech_annotation(&mut tail_transcript);
            let transcript = assemble_incremental_transcript(
                &accumulator,
                tail_transcript,
                tail.rms(),
                full_duration,
            )
            .map_err(|reason| format!("extended Accurate seam could not be proven: {reason}"))?;
            if has_pathological_repetition(&transcript.text) {
                return Err("Whisper produced a repeated output loop".to_owned());
            }
            eprintln!(
                "dictation_id={} state=Transcribing event=extended_final_tail tail_audio_ms={}",
                id.0,
                tail.duration().as_millis()
            );
            Ok(transcript)
        })();
        if let Ok(transcript) = tail_attempt {
            return Ok(transcript);
        }
    }

    eprintln!(
        "dictation_id={} state=Transcribing event=accurate_recovery path=bounded_spool",
        id.0
    );
    transcribe_accurate_sequential(recognizer, stats, language, full_duration, &mut snapshot)
}

fn transcribe_accurate_sequential<R>(
    recognizer: &R,
    stats: FinalAudioStats,
    language: &str,
    full_duration: Duration,
    snapshot: &mut impl FnMut(SampleRange) -> Result<AudioClip, String>,
) -> Result<Transcript, String>
where
    R: SpeechRecognizer,
{
    const WINDOW: u64 = 16_000 * 30;
    const OVERLAP: u64 = 16_000 * 2;
    let options = TranscriptionOptions {
        language: Some(language),
        thread_count: None,
        audio_context: None,
    };
    let mut output = String::new();
    let mut cursor = 0_u64;
    let mut inference_time = Duration::ZERO;
    let mut model_load_time = Duration::ZERO;
    let mut backend = None;
    let mut previous_was_silence = true;
    while cursor < stats.total_samples {
        let end = stats.total_samples.min(cursor.saturating_add(WINDOW));
        let clip = snapshot(SampleRange::new(cursor, end).map_err(|error| error.to_string())?)?;
        let (recognized, compute) =
            WallComputeClock.measure(|| recognizer.transcribe(&clip, &options));
        let mut recognized = recognized.map_err(|error| error.to_string())?;
        inference_time = inference_time.saturating_add(compute);
        model_load_time = model_load_time.max(recognized.model_load_time);
        backend.get_or_insert(recognized.backend);
        suppress_non_speech_annotation(&mut recognized);
        let text = recognized.text.trim();
        if text.is_empty() {
            if clip.rms() > 0.001 {
                return Err("extended Accurate recovery found uncertain empty speech".to_owned());
            }
            previous_was_silence = true;
        } else if output.is_empty() {
            output.push_str(text);
            previous_was_silence = false;
        } else {
            output = merge_overlapping(
                &output,
                text,
                if previous_was_silence {
                    MergeExpectation::Silence
                } else {
                    MergeExpectation::LexicalOverlap
                },
            )
            .map_err(|_| "extended Accurate recovery could not prove a window seam".to_owned())?;
            previous_was_silence = false;
        }
        if end == stats.total_samples {
            break;
        }
        cursor = end.saturating_sub(OVERLAP);
    }
    if has_pathological_repetition(&output) {
        return Err("Whisper produced a repeated output loop".to_owned());
    }
    Ok(Transcript {
        text: output,
        backend: backend.unwrap_or("whisper.cpp"),
        model_load_time,
        inference_time,
        audio_duration: full_duration,
    })
}

fn transcribe_final<R>(
    recognizer: &R,
    incremental: Option<PartialAccumulator>,
    full_clip: &AudioClip,
    language: &str,
    full_audio_context: u32,
    id: DictationId,
) -> Result<Transcript, String>
where
    R: SpeechRecognizer,
{
    transcribe_final_with_clock(
        recognizer,
        incremental,
        full_clip,
        language,
        full_audio_context,
        id,
        &WallComputeClock,
    )
}

fn transcribe_final_with_clock<R, C>(
    recognizer: &R,
    incremental: Option<PartialAccumulator>,
    full_clip: &AudioClip,
    language: &str,
    _full_audio_context: u32,
    id: DictationId,
    clock: &C,
) -> Result<Transcript, String>
where
    R: SpeechRecognizer,
    C: ComputeClock,
{
    let mut fallback = None;
    let mut partial_compute_time = Duration::ZERO;
    if let Some(accumulator) = incremental {
        partial_compute_time = accumulator.partial_compute_time;
        match transcribe_final_tail(recognizer, &accumulator, full_clip, language, clock) {
            Ok(transcript) => {
                let tail_compute_time = transcript
                    .inference_time
                    .saturating_sub(accumulator.partial_compute_time);
                eprintln!(
                    "dictation_id={} state=Transcribing event=incremental_final_tail stable_ms={} tail_audio_ms={} partial_compute_ms={} tail_compute_ms={}",
                    id.0,
                    accumulator.stable_end.as_millis(),
                    full_clip
                        .duration()
                        .saturating_sub(
                            final_tail_plan(&accumulator, full_clip.duration())
                                .map_or(accumulator.stable_end, |plan| plan.start),
                        )
                        .as_millis(),
                    accumulator.partial_compute_time.as_millis(),
                    tail_compute_time.as_millis()
                );
                if has_pathological_repetition(&transcript.text) {
                    fallback = Some(TailAttemptError {
                        reason: "incremental_repetition_loop",
                        compute_time: transcript
                            .inference_time
                            .saturating_sub(accumulator.partial_compute_time),
                    });
                } else {
                    return Ok(transcript);
                }
            }
            Err(error) => fallback = Some(error),
        }
    }

    let options = TranscriptionOptions {
        language: Some(language),
        thread_count: None,
        // Accurate production recognition uses the model's native encoder
        // context. Reduced audio_ctx remains benchmark-only.
        audio_context: None,
    };
    let (recognition, fallback_compute_time) =
        clock.measure(|| recognizer.transcribe(full_clip, &options));
    let mut transcript = recognition.map_err(|error| error.to_string())?;
    let doomed_tail_compute_time = fallback
        .as_ref()
        .map_or(Duration::ZERO, |error| error.compute_time);
    transcript.inference_time = partial_compute_time
        .saturating_add(doomed_tail_compute_time)
        .saturating_add(fallback_compute_time);
    suppress_non_speech_annotation(&mut transcript);
    if let Some(error) = fallback {
        eprintln!(
            "dictation_id={} state=Transcribing event=incremental_fallback reason={} partial_compute_ms={} doomed_tail_compute_ms={} fallback_compute_ms={}",
            id.0,
            error.reason,
            partial_compute_time.as_millis(),
            error.compute_time.as_millis(),
            fallback_compute_time.as_millis()
        );
    }
    if has_pathological_repetition(&transcript.text) {
        return Err("Whisper produced a repeated output loop".to_owned());
    }
    Ok(transcript)
}

fn transcribe_final_tail<R, C>(
    recognizer: &R,
    accumulator: &PartialAccumulator,
    full_clip: &AudioClip,
    language: &str,
    clock: &C,
) -> Result<Transcript, TailAttemptError>
where
    R: SpeechRecognizer,
    C: ComputeClock,
{
    // Eligibility must be established before copying audio or invoking
    // Whisper. An invalid partial session goes directly to the one full-clip
    // fallback in `transcribe_final`.
    let plan = final_tail_plan(accumulator, full_clip.duration())
        .map_err(TailAttemptError::before_recognition)?;
    let tail_start = plan.start;
    let start_sample =
        (tail_start.as_secs_f64() * f64::from(full_clip.sample_rate)).floor() as usize;
    if start_sample >= full_clip.samples.len() {
        return Err(TailAttemptError::before_recognition("empty_final_tail"));
    }
    let tail = AudioClip::new(
        full_clip.samples[start_sample..].to_vec(),
        full_clip.sample_rate,
    )
    .map_err(|_| TailAttemptError::before_recognition("invalid_final_tail"))?;
    let options = TranscriptionOptions {
        language: Some(language),
        thread_count: None,
        audio_context: None,
    };
    let (recognition, tail_compute_time) = clock.measure(|| recognizer.transcribe(&tail, &options));
    let mut tail_transcript = recognition.map_err(|_| TailAttemptError {
        reason: "tail_recognition_failed",
        compute_time: tail_compute_time,
    })?;
    tail_transcript.inference_time = tail_compute_time;
    suppress_non_speech_annotation(&mut tail_transcript);
    assemble_incremental_transcript(
        accumulator,
        tail_transcript,
        tail.rms(),
        full_clip.duration(),
    )
    .map_err(|reason| TailAttemptError {
        reason,
        compute_time: tail_compute_time,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TailAttemptError {
    reason: &'static str,
    compute_time: Duration,
}

impl TailAttemptError {
    fn before_recognition(reason: &'static str) -> Self {
        Self {
            reason,
            compute_time: Duration::ZERO,
        }
    }
}

trait ComputeClock {
    fn measure<T>(&self, operation: impl FnOnce() -> T) -> (T, Duration);
}

struct WallComputeClock;

impl ComputeClock for WallComputeClock {
    fn measure<T>(&self, operation: impl FnOnce() -> T) -> (T, Duration) {
        let started = Instant::now();
        let result = operation();
        (result, started.elapsed())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FinalTailPlan {
    start: Duration,
    expectation: MergeExpectation,
}

fn final_tail_plan(
    accumulator: &PartialAccumulator,
    full_audio_duration: Duration,
) -> Result<FinalTailPlan, &'static str> {
    if let Some(reason) = accumulator.degraded {
        return Err(reason);
    }
    if accumulator.next_sequence == 0 || accumulator.text.trim().is_empty() {
        return Err("no_stable_partial");
    }
    let boundary = accumulator.last_boundary.ok_or("missing_boundary")?;
    if accumulator.stable_end > full_audio_duration {
        return Err("stable_audio_exceeds_final");
    }
    Ok(FinalTailPlan {
        start: if accumulator.timestamp_stable {
            accumulator
                .unresolved_from
                .unwrap_or(accumulator.accepted_through)
        } else {
            accumulator.stable_end.saturating_sub(CHUNK_OVERLAP)
        },
        expectation: if accumulator.timestamp_stable {
            MergeExpectation::Silence
        } else {
            MergeExpectation::from(boundary)
        },
    })
}

fn assemble_incremental_transcript(
    accumulator: &PartialAccumulator,
    tail_transcript: Transcript,
    tail_rms: f32,
    full_audio_duration: Duration,
) -> Result<Transcript, &'static str> {
    let plan = final_tail_plan(accumulator, full_audio_duration)?;
    if tail_transcript.text.trim().is_empty() && tail_rms > 0.001 {
        return Err("uncertain_empty_tail");
    }
    if tail_rms <= 0.001
        && !tail_transcript.text.trim().is_empty()
        && merge_overlapping(
            &accumulator.text,
            &tail_transcript.text,
            MergeExpectation::LexicalOverlap,
        )
        .is_err()
    {
        return Err("low_energy_unmatched_tail");
    }
    let text = merge_overlapping(&accumulator.text, &tail_transcript.text, plan.expectation)
        .map_err(|_| "tail_overlap_unresolved")?;

    Ok(Transcript {
        text,
        backend: tail_transcript.backend,
        model_load_time: accumulator
            .model_load_time
            .max(tail_transcript.model_load_time),
        inference_time: accumulator
            .partial_compute_time
            .saturating_add(tail_transcript.inference_time),
        audio_duration: full_audio_duration,
    })
}

fn suppress_non_speech_annotation(transcript: &mut Transcript) {
    transcript.text = strip_known_non_speech_annotations(&transcript.text);
}

struct DictationContext {
    app_executable: Option<String>,
    language: String,
    formatting: WorkerFormatting,
    runtime_formatting: RuntimeFormatting,
    insertion_preference: InsertionPreference,
    verified_variant: Option<AccurateModelVariant>,
    activation_block: Option<ActivationBlockReason>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationBlockReason {
    ProfileDenied,
    InstantLanguageMismatch,
}

impl ActivationBlockReason {
    fn label(self) -> &'static str {
        match self {
            Self::ProfileDenied => "profile_denied",
            Self::InstantLanguageMismatch => "instant_language_mismatch",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::ProfileDenied => "Dictation is disabled by this application profile.",
            Self::InstantLanguageMismatch => {
                "This application profile requests a language that does not match the resident Instant model. Change the profile language or use Accurate mode."
            }
        }
    }
}

impl DictationContext {
    fn global(settings: &Settings, verified_variant: Option<AccurateModelVariant>) -> Result<Self> {
        if verified_variant
            .is_some_and(|variant| !variant.supports_language(&settings.recognition.language))
        {
            return Err(anyhow!(
                "the verified English-only Whisper model cannot transcribe the configured language"
            ));
        }
        Ok(Self {
            app_executable: None,
            language: settings.recognition.language.clone(),
            formatting: WorkerFormatting::from_settings(settings)?,
            runtime_formatting: RuntimeFormatting::try_from(settings.formatting.strength)?,
            insertion_preference: InsertionPreference::Automatic,
            verified_variant,
            activation_block: None,
        })
    }

    fn for_target(
        target: Option<TargetSnapshot>,
        persistence: &Persistence,
        settings: &Settings,
        verified_variant: Option<AccurateModelVariant>,
    ) -> Result<Self> {
        let context = Self::global(settings, verified_variant)?;
        let executable = target.and_then(TargetSnapshot::executable_name);
        Self::for_executable(context, executable, persistence, settings)
    }

    fn for_executable(
        mut context: Self,
        executable: Option<String>,
        persistence: &Persistence,
        settings: &Settings,
    ) -> Result<Self> {
        let Some(executable) = executable else {
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
        if let Some(profile_language) = profile.language {
            if !profile_language_matches_resident_model(
                settings.recognition.mode,
                &settings.recognition.language,
                &profile_language,
            ) {
                context.activation_block = Some(ActivationBlockReason::InstantLanguageMismatch);
            } else {
                context.language = profile_language;
            }
        }
        if context
            .verified_variant
            .is_some_and(|variant| !variant.supports_language(&context.language))
        {
            return Err(anyhow!(
                "the verified English-only Whisper model cannot transcribe the active application profile language"
            ));
        }
        context.insertion_preference = profile.insertion_preference;
        if profile.deny {
            context.activation_block = Some(ActivationBlockReason::ProfileDenied);
        }
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

fn profile_language_matches_resident_model(
    mode: RecognitionMode,
    resident_language: &str,
    profile_language: &str,
) -> bool {
    mode != RecognitionMode::Instant || profile_language.eq_ignore_ascii_case(resident_language)
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
    /// Release receipt through audio finalization and worker dequeue. This is
    /// kept content-free and persisted as part of the STT-stage latency.
    pre_stt_time: Duration,
    warnings: Vec<String>,
    language: String,
    app_executable: Option<String>,
    lifecycle: Option<LifecycleTiming>,
    terminal: TerminalMetadata,
}

#[derive(Clone, Copy, Debug)]
struct LifecycleTiming {
    release: ReleaseTiming,
    worker_started_at: Instant,
    worker_completed_at: Instant,
}

fn process_transcript(
    mut transcript: Transcript,
    language: &str,
    formatting: &WorkerFormatting,
    ollama: Option<&OllamaClient>,
    aliases: &[LexiconEntry],
    app_executable: Option<&str>,
    cancel: &CancellationToken,
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
    let mut terminal = TerminalMetadata::default();
    let (selected, cleaned_text) = match &formatting.profile {
        FormatProfile::Raw => (raw_text.clone(), None),
        FormatProfile::Light => (normalized_text.clone(), None),
        FormatProfile::Balanced | FormatProfile::Strong | FormatProfile::Custom(_) => {
            match (ollama, formatting.model.as_ref()) {
                (Some(client), Some(model)) => {
                    if normalized_text.len() > 64 * 1024 {
                        match client.format_document(
                            model,
                            &normalized_text,
                            &formatting.profile,
                            formatting.keep_alive.clone(),
                            cancel,
                        ) {
                            Ok(result) => {
                                terminal.formatting_chunk_count = Some(result.chunks.len() as u64);
                                if result.disposition != DocumentFormatDisposition::Completed {
                                    warnings.push(format!(
                                        "ollama_document:{}",
                                        document_disposition(result.disposition)
                                    ));
                                }
                                let cleaned = result.model.as_ref().map(|_| result.text.clone());
                                (result.text, cleaned)
                            }
                            Err(_) => {
                                warnings.push("ollama_fallback:document_rejected".to_owned());
                                (normalized_text.clone(), None)
                            }
                        }
                    } else {
                        match client.format(
                            model,
                            &normalized_text,
                            &formatting.profile,
                            formatting.keep_alive.clone(),
                            cancel,
                        ) {
                            FormatResult::Formatted { text, .. } => (text.clone(), Some(text)),
                            FormatResult::Fallback { reason, .. } => {
                                warnings
                                    .push(format!("ollama_fallback:{}", fallback_reason(&reason)));
                                (normalized_text.clone(), None)
                            }
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
        pre_stt_time: Duration::ZERO,
        warnings,
        language: language.to_owned(),
        app_executable: app_executable.map(str::to_owned),
        lifecycle: None,
        terminal,
    }
}

fn document_disposition(disposition: DocumentFormatDisposition) -> &'static str {
    match disposition {
        DocumentFormatDisposition::Completed => "completed",
        DocumentFormatDisposition::PartiallyFormatted => "partially_formatted",
        DocumentFormatDisposition::RawBypass => "raw_bypass",
        DocumentFormatDisposition::Cancelled => "cancelled",
        DocumentFormatDisposition::ServiceLost => "service_lost",
        DocumentFormatDisposition::SourceFallbackOutputLimit => "output_limit",
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
    insertion_started_at: Instant,
    insertion_completed_at: Instant,
) -> phorminx_persistence::Result<()> {
    let lifecycle = processed.lifecycle;
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
                    .pre_stt_time
                    .saturating_add(processed.transcript.inference_time)
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
            ),
            formatting_duration_ms: Some(
                processed
                    .formatting_time
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
            ),
            insertion_duration_ms: Some(duration_ms(
                insertion_completed_at.saturating_duration_since(insertion_started_at),
            )),
            audio_finalization_duration_ms: lifecycle
                .map(|timing| duration_ms(timing.release.audio_finalization_time())),
            worker_queue_duration_ms: lifecycle.map(|timing| {
                duration_ms(
                    timing
                        .worker_started_at
                        .saturating_duration_since(timing.release.audio_finalized_at),
                )
            }),
            release_to_insert_duration_ms: lifecycle.map(|timing| {
                duration_ms(
                    insertion_completed_at.saturating_duration_since(timing.release.released_at),
                )
            }),
        },
        warnings: processed.warnings.clone(),
    };
    persistence
        .history()
        .insert_with_terminal_metadata(&draft, &processed.terminal)
        .map(|_| ())
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
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

fn install_verified_vosk_assets(
    settings_store: &SettingsStore,
) -> Result<Option<(PathBuf, PathBuf)>> {
    use std::io::Write;
    use std::os::windows::process::CommandExt;

    let Some(runtime_archive) = choose_zip_archive("Choose official Vosk runtime ZIP") else {
        return Ok(None);
    };
    let Some(model_archive) = choose_zip_archive("Choose official English Vosk model ZIP") else {
        return Ok(None);
    };
    let destination = settings_store
        .path()
        .parent()
        .context("settings directory is unavailable")?
        .join(format!("vosk-assets-{}", now_ms()));
    let script_path = std::env::temp_dir().join(format!(
        "phorminx-verified-vosk-import-{}-{}.ps1",
        std::process::id(),
        now_ms()
    ));
    let mut script = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&script_path)
        .context("could not create the verified importer")?;
    script
        .write_all(include_bytes!(
            "../../../scripts/Install-PhorminxVoskAssets.ps1"
        ))
        .and_then(|_| script.flush())
        .and_then(|_| script.sync_all())
        .context("could not stage the verified importer")?;
    drop(script);
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script_path)
        .arg("-RuntimeArchive")
        .arg(&runtime_archive)
        .arg("-ModelArchive")
        .arg(&model_archive)
        .arg("-DestinationRoot")
        .arg(&destination)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    let _ = std::fs::remove_file(&script_path);
    if !status.is_ok_and(|status| status.success()) {
        return Err(anyhow!(
            "archive verification or safe extraction was rejected"
        ));
    }
    let runtime_path = destination.join("runtime/vosk");
    let model_path = destination.join("models/vosk-model-small-en-us-0.15");
    if !phorminx_vosk::inspect(&runtime_path, &model_path, "en").is_ready() {
        return Err(anyhow!(
            "installed assets failed the native readiness probe"
        ));
    }
    Ok(Some((runtime_path, model_path)))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn elapsed_since_release(released_at: Instant, stage_at: Instant) -> Duration {
    stage_at.saturating_duration_since(released_at)
}

#[cfg(test)]
mod composition_tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

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

    fn partial(text: &str, boundary: BoundaryKind) -> PartialAccumulator {
        PartialAccumulator {
            text: text.to_owned(),
            stable_end: Duration::from_secs(8),
            accepted_through: Duration::ZERO,
            unresolved_from: None,
            timestamp_stable: false,
            last_boundary: Some(boundary),
            next_sequence: 1,
            partial_compute_time: Duration::from_millis(400),
            model_load_time: Duration::from_millis(100),
            degraded: None,
        }
    }

    fn transcript(text: &str) -> Transcript {
        Transcript {
            text: text.to_owned(),
            backend: "fake",
            model_load_time: Duration::from_millis(100),
            inference_time: Duration::from_millis(200),
            audio_duration: Duration::from_secs(3),
        }
    }

    #[test]
    fn cleanup_failure_overrides_success_before_formatting_or_insertion() {
        let result = require_audio_cleanup::<u8>(Ok(7), Err("injected permission loss".to_owned()));
        assert_eq!(
            result.unwrap_err(),
            "encrypted audio scratch cleanup failed: injected permission loss"
        );
    }

    #[test]
    fn cleanup_is_still_reported_when_recognition_also_fails() {
        let result = require_audio_cleanup::<u8>(
            Err("decoder failed".to_owned()),
            Err("injected permission loss".to_owned()),
        );
        let error = result.unwrap_err();
        assert!(error.contains("transcription failed (decoder failed)"));
        assert!(error.contains("cleanup also failed (injected permission loss)"));
    }

    #[test]
    fn recognition_failure_survives_successful_cleanup() {
        assert_eq!(
            require_audio_cleanup::<u8>(Err("decoder failed".to_owned()), Ok(())).unwrap_err(),
            "decoder failed"
        );
    }

    #[test]
    fn every_active_capture_integrity_fault_has_a_content_free_safe_stop_reason() {
        for (fault, expected) in [
            (ExtendedCaptureFault::CallbackOverflow, "capture_overflow"),
            (ExtendedCaptureFault::StreamFailed, "device_loss"),
            (ExtendedCaptureFault::Resampling, "resampling"),
            (ExtendedCaptureFault::SpoolQuota, "scratch_quota"),
            (ExtendedCaptureFault::SpoolIntegrity, "scratch_integrity"),
            (ExtendedCaptureFault::SpoolIo, "scratch_io"),
            (ExtendedCaptureFault::WorkerUnavailable, "capture_worker"),
            (ExtendedCaptureFault::WorkerPanicked, "capture_worker"),
            (
                ExtendedCaptureFault::SampleAccountingOverflow,
                "sample_accounting",
            ),
        ] {
            assert_eq!(auto_stop_reason(fault).unwrap().label(), expected);
        }
        for non_active in [
            ExtendedCaptureFault::InvalidConfiguration,
            ExtendedCaptureFault::InvalidSnapshot,
            ExtendedCaptureFault::FinalizerBusy,
        ] {
            assert_eq!(auto_stop_reason(non_active), None);
        }
    }

    #[test]
    fn timestamp_stability_discards_overlap_without_lexical_guessing() {
        use phorminx_app::incremental::{ChunkPlan, TimeRange};
        use phorminx_whisper::TimedSegment;

        let mut accumulator = PartialAccumulator::default();
        let first = ChunkPlan {
            id: DictationId(1),
            sequence: 0,
            range: TimeRange {
                start: Duration::ZERO,
                end: Duration::from_secs(3),
            },
            stable_end: Duration::from_secs(3),
            start_overlap: None,
            boundary: BoundaryKind::Silence,
        };
        append_timestamp_stable_segments(
            &mut accumulator,
            first,
            &[TimedSegment {
                text: "Alpha beta.".to_owned(),
                start: Duration::ZERO,
                end: Duration::from_secs(3),
            }],
        );
        let second = ChunkPlan {
            id: DictationId(1),
            sequence: 1,
            range: TimeRange {
                start: Duration::from_millis(2_500),
                end: Duration::from_secs(5),
            },
            stable_end: Duration::from_secs(5),
            start_overlap: Some(MergeExpectation::Silence),
            boundary: BoundaryKind::Silence,
        };
        append_timestamp_stable_segments(
            &mut accumulator,
            second,
            &[
                TimedSegment {
                    text: "beta.".to_owned(),
                    start: Duration::ZERO,
                    end: Duration::from_millis(400),
                },
                TimedSegment {
                    text: "Gamma.".to_owned(),
                    start: Duration::from_millis(500),
                    end: Duration::from_secs(2),
                },
            ],
        );

        assert_eq!(accumulator.text, "Alpha beta. Gamma.");
        assert_eq!(accumulator.accepted_through, Duration::from_millis(4_500));
    }

    #[test]
    fn repetition_guard_rejects_decoder_loops_but_not_normal_emphasis() {
        assert!(has_pathological_repetition(
            "the same decoder phrase repeats forever the same decoder phrase repeats forever the same decoder phrase repeats forever the same decoder phrase repeats forever"
        ));
        assert!(!has_pathological_repetition("very very very important"));
        assert!(!has_pathological_repetition(
            "red green blue red green blue red green blue"
        ));
        assert!(!has_pathological_repetition(
            "the same phrase again, and then one ordinary conclusion"
        ));
    }

    #[test]
    fn release_priority_aborts_only_the_obsolete_partial() {
        let registry = CancellationRegistry::default();
        let abort = registry.begin_partial(DictationId(7));
        assert!(!abort.load(Ordering::Acquire));
        registry.prioritize_final(DictationId(7));
        assert!(abort.load(Ordering::Acquire));
        assert!(!registry.is_cancelled(DictationId(7)));
        registry.end_partial(DictationId(7));
    }

    #[test]
    fn release_priority_tombstone_aborts_a_partial_that_has_not_started_yet() {
        let registry = CancellationRegistry::default();
        let id = DictationId(8);

        // FIFO order: release is published while an older partial command is
        // queued, before that command calls begin_partial.
        registry.prioritize_final(id);
        let obsolete = registry.begin_partial(id);
        assert!(obsolete.load(Ordering::Acquire));
        registry.end_partial(id);

        // Only dequeuing the final command clears the tombstone. A new
        // dictation using this id would not inherit the release priority.
        registry.begin_final(id);
        let later = registry.begin_partial(id);
        assert!(!later.load(Ordering::Acquire));
    }

    #[test]
    fn release_latency_includes_work_before_worker_dequeue() {
        let released_at = Instant::now();
        let worker_dequeued_at = released_at + Duration::from_millis(37);
        let inserted_at = released_at + Duration::from_millis(91);

        assert_eq!(
            elapsed_since_release(released_at, worker_dequeued_at),
            Duration::from_millis(37)
        );
        assert_eq!(
            elapsed_since_release(released_at, inserted_at),
            Duration::from_millis(91)
        );
    }

    #[test]
    fn forced_guard_gap_freezes_frontier_and_final_resumes_before_missing_audio() {
        use phorminx_app::incremental::{ChunkPlan, TimeRange};
        use phorminx_whisper::TimedSegment;

        let mut accumulator = PartialAccumulator::default();
        let first = ChunkPlan {
            id: DictationId(9),
            sequence: 0,
            range: TimeRange {
                start: Duration::ZERO,
                end: Duration::from_secs(3),
            },
            stable_end: Duration::from_secs(3),
            start_overlap: None,
            boundary: BoundaryKind::Forced,
        };
        append_timestamp_stable_segments(
            &mut accumulator,
            first,
            &[
                TimedSegment {
                    text: "accepted".to_owned(),
                    start: Duration::ZERO,
                    end: Duration::from_secs(2),
                },
                TimedSegment {
                    text: "guarded".to_owned(),
                    start: Duration::from_secs(2),
                    end: Duration::from_secs(3),
                },
            ],
        );
        assert_eq!(accumulator.accepted_through, Duration::from_secs(2));
        assert_eq!(accumulator.unresolved_from, Some(Duration::from_secs(2)));

        let later = ChunkPlan {
            id: DictationId(9),
            sequence: 1,
            range: TimeRange {
                start: Duration::from_millis(2_500),
                end: Duration::from_secs(4),
            },
            stable_end: Duration::from_secs(4),
            start_overlap: Some(MergeExpectation::LexicalOverlap),
            boundary: BoundaryKind::Silence,
        };
        append_timestamp_stable_segments(
            &mut accumulator,
            later,
            &[TimedSegment {
                text: "must not jump the gap".to_owned(),
                start: Duration::ZERO,
                end: Duration::from_millis(1_500),
            }],
        );
        accumulator.timestamp_stable = true;
        accumulator.stable_end = Duration::from_secs(4);
        accumulator.last_boundary = Some(BoundaryKind::Silence);
        accumulator.next_sequence = 2;

        assert_eq!(accumulator.text, "accepted");
        assert_eq!(
            final_tail_plan(&accumulator, Duration::from_secs(5))
                .unwrap()
                .start,
            Duration::from_secs(2)
        );
    }

    #[test]
    fn first_segment_gap_never_advances_the_canonical_frontier() {
        use phorminx_app::incremental::{ChunkPlan, TimeRange};
        use phorminx_whisper::TimedSegment;

        let mut accumulator = PartialAccumulator::default();
        let plan = ChunkPlan {
            id: DictationId(12),
            sequence: 0,
            range: TimeRange {
                start: Duration::ZERO,
                end: Duration::from_secs(3),
            },
            stable_end: Duration::from_secs(3),
            start_overlap: None,
            boundary: BoundaryKind::Silence,
        };
        append_timestamp_stable_segments(
            &mut accumulator,
            plan,
            &[TimedSegment {
                text: "late speech".to_owned(),
                start: Duration::from_secs(1),
                end: Duration::from_secs(2),
            }],
        );
        assert!(accumulator.text.is_empty());
        assert_eq!(accumulator.accepted_through, Duration::ZERO);
        assert_eq!(accumulator.unresolved_from, Some(Duration::ZERO));
    }

    #[test]
    fn internal_timestamp_gap_preserves_only_the_contiguous_prefix() {
        use phorminx_app::incremental::{ChunkPlan, TimeRange};
        use phorminx_whisper::TimedSegment;

        let mut accumulator = PartialAccumulator::default();
        let plan = ChunkPlan {
            id: DictationId(13),
            sequence: 0,
            range: TimeRange {
                start: Duration::ZERO,
                end: Duration::from_secs(3),
            },
            stable_end: Duration::from_secs(3),
            start_overlap: None,
            boundary: BoundaryKind::Silence,
        };
        append_timestamp_stable_segments(
            &mut accumulator,
            plan,
            &[
                TimedSegment {
                    text: "proven".to_owned(),
                    start: Duration::ZERO,
                    end: Duration::from_secs(1),
                },
                TimedSegment {
                    text: "after gap".to_owned(),
                    start: Duration::from_millis(1_500),
                    end: Duration::from_secs(2),
                },
            ],
        );
        assert_eq!(accumulator.text, "proven");
        assert_eq!(accumulator.accepted_through, Duration::from_secs(1));
        assert_eq!(accumulator.unresolved_from, Some(Duration::from_secs(1)));
        accumulator.next_sequence = 1;
        accumulator.last_boundary = Some(BoundaryKind::Silence);
        accumulator.timestamp_stable = true;
        assert_eq!(
            final_tail_plan(&accumulator, Duration::from_secs(4))
                .unwrap()
                .start,
            Duration::from_secs(1)
        );
    }

    #[test]
    fn overlap_only_segments_and_timestamp_regression_cannot_move_frontier() {
        use phorminx_app::incremental::{ChunkPlan, TimeRange};
        use phorminx_whisper::TimedSegment;

        let mut accumulator = PartialAccumulator {
            text: "accepted prefix".to_owned(),
            accepted_through: Duration::from_secs(2),
            ..PartialAccumulator::default()
        };
        let plan = ChunkPlan {
            id: DictationId(14),
            sequence: 1,
            range: TimeRange {
                start: Duration::from_secs(1),
                end: Duration::from_secs(4),
            },
            stable_end: Duration::from_secs(4),
            start_overlap: Some(MergeExpectation::Silence),
            boundary: BoundaryKind::Silence,
        };
        append_timestamp_stable_segments(
            &mut accumulator,
            plan,
            &[
                TimedSegment {
                    text: "overlap only".to_owned(),
                    start: Duration::ZERO,
                    end: Duration::from_secs(1),
                },
                TimedSegment {
                    text: "straddles frontier".to_owned(),
                    start: Duration::from_millis(500),
                    end: Duration::from_millis(1_500),
                },
            ],
        );
        assert_eq!(accumulator.text, "accepted prefix");
        assert_eq!(accumulator.accepted_through, Duration::from_secs(2));
        assert_eq!(accumulator.unresolved_from, Some(Duration::from_secs(2)));
    }

    #[test]
    fn segment_straddling_accepted_frontier_defers_its_suffix_to_final() {
        use phorminx_app::incremental::{ChunkPlan, TimeRange};
        use phorminx_whisper::TimedSegment;

        let mut accumulator = PartialAccumulator {
            text: "accepted".to_owned(),
            accepted_through: Duration::from_secs(2),
            ..PartialAccumulator::default()
        };
        append_timestamp_stable_segments(
            &mut accumulator,
            ChunkPlan {
                id: DictationId(10),
                sequence: 1,
                range: TimeRange {
                    start: Duration::from_millis(1_800),
                    end: Duration::from_secs(4),
                },
                stable_end: Duration::from_secs(4),
                start_overlap: Some(MergeExpectation::Silence),
                boundary: BoundaryKind::Silence,
            },
            &[TimedSegment {
                text: "changed boundary".to_owned(),
                start: Duration::ZERO,
                end: Duration::from_millis(700),
            }],
        );
        assert_eq!(accumulator.accepted_through, Duration::from_secs(2));
        assert_eq!(accumulator.unresolved_from, Some(Duration::from_secs(2)));
    }

    struct CountingRecognizer {
        clip_lengths: RefCell<Vec<usize>>,
        output: String,
    }

    impl Default for CountingRecognizer {
        fn default() -> Self {
            Self {
                clip_lengths: RefCell::new(Vec::new()),
                output: "full fallback".to_owned(),
            }
        }
    }

    impl CountingRecognizer {
        fn returning(output: &str) -> Self {
            Self {
                output: output.to_owned(),
                ..Self::default()
            }
        }
    }

    impl SpeechRecognizer for CountingRecognizer {
        type Error = std::io::Error;

        fn load(_model_path: &Path) -> Result<Self, Self::Error> {
            Ok(Self::default())
        }

        fn transcribe(
            &self,
            clip: &AudioClip,
            _options: &TranscriptionOptions<'_>,
        ) -> Result<Transcript, Self::Error> {
            self.clip_lengths.borrow_mut().push(clip.samples.len());
            Ok(Transcript {
                text: self.output.clone(),
                backend: "fake",
                model_load_time: Duration::ZERO,
                inference_time: Duration::from_millis(1),
                audio_duration: clip.duration(),
            })
        }
    }

    enum SequencedOutcome {
        Text(&'static str),
        Failure,
    }

    struct SequencedRecognizer {
        outcomes: RefCell<VecDeque<SequencedOutcome>>,
        clip_lengths: RefCell<Vec<usize>>,
    }

    impl SequencedRecognizer {
        fn new(outcomes: impl IntoIterator<Item = SequencedOutcome>) -> Self {
            Self {
                outcomes: RefCell::new(outcomes.into_iter().collect()),
                clip_lengths: RefCell::new(Vec::new()),
            }
        }
    }

    impl SpeechRecognizer for SequencedRecognizer {
        type Error = std::io::Error;

        fn load(_model_path: &Path) -> Result<Self, Self::Error> {
            Ok(Self::new([]))
        }

        fn transcribe(
            &self,
            clip: &AudioClip,
            _options: &TranscriptionOptions<'_>,
        ) -> Result<Transcript, Self::Error> {
            self.clip_lengths.borrow_mut().push(clip.samples.len());
            match self.outcomes.borrow_mut().pop_front().unwrap() {
                SequencedOutcome::Text(text) => Ok(Transcript {
                    text: text.to_owned(),
                    backend: "fake",
                    model_load_time: Duration::ZERO,
                    inference_time: Duration::from_secs(99),
                    audio_duration: clip.duration(),
                }),
                SequencedOutcome::Failure => Err(std::io::Error::other("fake failure")),
            }
        }
    }

    struct ScriptedClock {
        durations: RefCell<VecDeque<Duration>>,
    }

    impl ScriptedClock {
        fn new(durations: impl IntoIterator<Item = Duration>) -> Self {
            Self {
                durations: RefCell::new(durations.into_iter().collect()),
            }
        }
    }

    impl ComputeClock for ScriptedClock {
        fn measure<T>(&self, operation: impl FnOnce() -> T) -> (T, Duration) {
            let result = operation();
            let duration = self.durations.borrow_mut().pop_front().unwrap();
            (result, duration)
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
    fn native_download_labels_match_every_manifest_variant_and_size() {
        let labels = accurate_model_download_labels();
        assert_eq!(labels.len(), 5);
        for (index, (variant, name)) in [
            (AccurateModelVariant::TinyEnglish, "Tiny English"),
            (AccurateModelVariant::BaseEnglish, "Base English"),
            (AccurateModelVariant::TinyMultilingual, "Tiny Multilingual"),
            (AccurateModelVariant::BaseMultilingual, "Base Multilingual"),
        ]
        .into_iter()
        .enumerate()
        {
            let spec = model_for_variant(variant).unwrap();
            assert!(labels[index].contains(name));
            assert!(
                labels[index]
                    .contains(&format!("{:.1} MiB", spec.bytes as f64 / (1024.0 * 1024.0)))
            );
        }
        assert!(labels[4].contains("Browse"));
    }

    #[test]
    fn final_tail_assembles_once_with_full_audio_timing() {
        let assembled = assemble_incremental_transcript(
            &partial("Please send the release notes", BoundaryKind::Forced),
            transcript("the release notes tomorrow morning."),
            0.1,
            Duration::from_secs(11),
        )
        .unwrap();

        assert_eq!(
            assembled.text,
            "Please send the release notes tomorrow morning."
        );
        assert_eq!(assembled.audio_duration, Duration::from_secs(11));
        assert_eq!(assembled.inference_time, Duration::from_millis(600));
    }

    #[test]
    fn final_tail_fails_closed_for_missing_overlap_and_degraded_partial() {
        assert_eq!(
            assemble_incremental_transcript(
                &partial("alpha beta", BoundaryKind::Forced),
                transcript("gamma delta"),
                0.1,
                Duration::from_secs(10),
            )
            .unwrap_err(),
            "tail_overlap_unresolved"
        );
        let mut degraded = partial("alpha beta", BoundaryKind::Silence);
        degraded.degraded = Some("partial_recognition_failed");
        assert_eq!(
            assemble_incremental_transcript(
                &degraded,
                transcript("gamma delta"),
                0.1,
                Duration::from_secs(10),
            )
            .unwrap_err(),
            "partial_recognition_failed"
        );
    }

    #[test]
    fn rejected_tail_compute_is_included_in_full_fallback_total() {
        let recognizer = SequencedRecognizer::new([
            SequencedOutcome::Text("gamma delta"),
            SequencedOutcome::Text("full recovered transcript"),
        ]);
        let clock = ScriptedClock::new([Duration::from_millis(200), Duration::from_millis(300)]);
        let full_clip = AudioClip::new(vec![0.1; 160_000], 16_000).unwrap();

        let result = transcribe_final_with_clock(
            &recognizer,
            Some(partial("alpha beta", BoundaryKind::Forced)),
            &full_clip,
            "en",
            recommended_audio_context(full_clip.duration()),
            DictationId(79),
            &clock,
        )
        .unwrap();

        assert_eq!(result.text, "full recovered transcript");
        assert_eq!(result.inference_time, Duration::from_millis(900));
        let lengths = recognizer.clip_lengths.borrow();
        assert_eq!(lengths.len(), 2);
        assert!(lengths[0] < full_clip.samples.len());
        assert_eq!(lengths[1], full_clip.samples.len());
    }

    #[test]
    fn incremental_decoder_loop_recovers_with_one_clean_full_clip_pass() {
        let recognizer = SequencedRecognizer::new([
            SequencedOutcome::Text(
                "the same decoder phrase repeats forever the same decoder phrase repeats forever the same decoder phrase repeats forever the same decoder phrase repeats forever",
            ),
            SequencedOutcome::Text("clean full transcript"),
        ]);
        let clock = ScriptedClock::new([Duration::from_millis(200), Duration::from_millis(300)]);
        let full_clip = AudioClip::new(vec![0.1; 160_000], 16_000).unwrap();

        let result = transcribe_final_with_clock(
            &recognizer,
            Some(partial("stable introduction", BoundaryKind::Silence)),
            &full_clip,
            "en",
            recommended_audio_context(full_clip.duration()),
            DictationId(81),
            &clock,
        )
        .unwrap();

        assert_eq!(result.text, "clean full transcript");
        assert_eq!(recognizer.clip_lengths.borrow().len(), 2);
        assert_eq!(recognizer.clip_lengths.borrow()[1], full_clip.samples.len());
    }

    #[test]
    fn failed_tail_recognition_compute_is_also_included_in_fallback_total() {
        let recognizer = SequencedRecognizer::new([
            SequencedOutcome::Failure,
            SequencedOutcome::Text("full recovered transcript"),
        ]);
        let clock = ScriptedClock::new([Duration::from_millis(200), Duration::from_millis(300)]);
        let full_clip = AudioClip::new(vec![0.1; 160_000], 16_000).unwrap();

        let result = transcribe_final_with_clock(
            &recognizer,
            Some(partial("alpha beta", BoundaryKind::Forced)),
            &full_clip,
            "en",
            recommended_audio_context(full_clip.duration()),
            DictationId(80),
            &clock,
        )
        .unwrap();

        assert_eq!(result.inference_time, Duration::from_millis(900));
        assert_eq!(recognizer.clip_lengths.borrow().len(), 2);
    }

    #[test]
    fn ineligible_partial_sessions_skip_tail_recognition_and_run_one_full_fallback() {
        let mut degraded = partial("stable words", BoundaryKind::Forced);
        degraded.degraded = Some("empty_partial");
        let mut empty = partial("", BoundaryKind::Forced);
        empty.next_sequence = 1;
        let mut missing_boundary = partial("stable words", BoundaryKind::Forced);
        missing_boundary.last_boundary = None;
        let full_clip = AudioClip::new(vec![0.1; 160_000], 16_000).unwrap();

        for accumulator in [degraded, empty, missing_boundary] {
            let recognizer = CountingRecognizer::default();
            let result = transcribe_final(
                &recognizer,
                Some(accumulator),
                &full_clip,
                "en",
                recommended_audio_context(full_clip.duration()),
                DictationId(77),
            )
            .unwrap();

            assert_eq!(result.text, "full fallback");
            assert_eq!(
                recognizer.clip_lengths.borrow().as_slice(),
                [full_clip.samples.len()]
            );
        }
    }

    #[test]
    fn final_tail_plan_rejects_invalid_state_before_audio_work() {
        let mut degraded = partial("stable words", BoundaryKind::Forced);
        degraded.degraded = Some("partial_recognition_failed");
        assert_eq!(
            final_tail_plan(&degraded, Duration::from_secs(10)),
            Err("partial_recognition_failed")
        );

        let mut empty = partial("", BoundaryKind::Forced);
        empty.next_sequence = 1;
        assert_eq!(
            final_tail_plan(&empty, Duration::from_secs(10)),
            Err("no_stable_partial")
        );

        let mut missing_boundary = partial("stable words", BoundaryKind::Forced);
        missing_boundary.last_boundary = None;
        assert_eq!(
            final_tail_plan(&missing_boundary, Duration::from_secs(10)),
            Err("missing_boundary")
        );
    }

    #[test]
    fn silence_tail_can_append_but_uncertain_empty_speech_falls_back() {
        let accumulator = partial("First sentence.", BoundaryKind::Silence);
        let appended = assemble_incremental_transcript(
            &accumulator,
            transcript("Second sentence."),
            0.1,
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(appended.text, "First sentence. Second sentence.");

        assert_eq!(
            assemble_incremental_transcript(
                &accumulator,
                transcript(""),
                0.1,
                Duration::from_secs(10),
            )
            .unwrap_err(),
            "uncertain_empty_tail"
        );
        assert_eq!(
            assemble_incremental_transcript(
                &accumulator,
                transcript(""),
                0.000_1,
                Duration::from_secs(10),
            )
            .unwrap()
            .text,
            "First sentence."
        );
    }

    #[test]
    fn known_non_speech_markers_never_reach_final_output() {
        let recognizer = CountingRecognizer::returning("hello [BLANK_AUDIO]");
        let full_clip = AudioClip::new(vec![0.1; 16_000], 16_000).unwrap();
        let result = transcribe_final(
            &recognizer,
            None,
            &full_clip,
            "en",
            recommended_audio_context(full_clip.duration()),
            DictationId(78),
        )
        .unwrap();
        assert_eq!(result.text, "hello");

        let accumulator = partial("Hello.", BoundaryKind::Silence);
        let mut marker_tail = transcript(" [ blank_audio ] ");
        suppress_non_speech_annotation(&mut marker_tail);
        assert_eq!(
            assemble_incremental_transcript(
                &accumulator,
                marker_tail,
                0.0,
                Duration::from_secs(10),
            )
            .unwrap()
            .text,
            "Hello."
        );
    }

    #[test]
    fn low_energy_unmatched_tail_falls_back_instead_of_appending_hallucination() {
        assert_eq!(
            assemble_incremental_transcript(
                &partial("Hello.", BoundaryKind::Silence),
                transcript("invented words"),
                0.000_1,
                Duration::from_secs(10),
            )
            .unwrap_err(),
            "low_energy_unmatched_tail"
        );
    }

    #[test]
    fn cancellation_is_visible_before_queued_final_or_partial_work_starts() {
        let cancellations = CancellationRegistry::default();
        let shutting_down = AtomicBool::new(false);
        let active = DictationId(80);
        let cancelled = DictationId(81);

        cancellations.cancel(cancelled);
        assert!(!worker_job_cancelled(
            &cancellations,
            &shutting_down,
            active
        ));
        // These checks model a partial and final already ahead of the FIFO
        // Cancel command: both see shared cancellation and skip inference.
        assert!(worker_job_cancelled(
            &cancellations,
            &shutting_down,
            cancelled
        ));
        assert!(worker_job_cancelled(
            &cancellations,
            &shutting_down,
            cancelled
        ));
        cancellations.acknowledge(cancelled);
        assert!(!worker_job_cancelled(
            &cancellations,
            &shutting_down,
            cancelled
        ));
    }

    #[test]
    fn shutdown_flag_skips_every_queued_job_before_fifo_shutdown_arrives() {
        let cancellations = CancellationRegistry::default();
        let shutting_down = AtomicBool::new(true);

        for id in [DictationId(90), DictationId(91), DictationId(92)] {
            assert!(worker_job_cancelled(&cancellations, &shutting_down, id));
        }
    }

    #[test]
    fn cancellation_observed_after_recognition_blocks_followup_formatting() {
        let cancellations = CancellationRegistry::default();
        let shutting_down = AtomicBool::new(false);
        let id = DictationId(93);

        assert!(!worker_job_cancelled(&cancellations, &shutting_down, id));
        // Models the cancellation becoming visible while the non-preemptible
        // recognizer is running and the post-recognition guard seeing it.
        cancellations.cancel(id);
        assert!(worker_job_cancelled(&cancellations, &shutting_down, id));
    }

    #[test]
    fn cancellation_registry_propagates_to_active_ollama_token() {
        let cancellations = CancellationRegistry::default();
        let id = DictationId(94);
        let token = cancellations.begin_formatting(id);
        assert!(!token.is_cancelled());

        cancellations.cancel(id);
        assert!(token.is_cancelled());
        cancellations.end_formatting(id);

        let already_cancelled = cancellations.begin_formatting(id);
        assert!(already_cancelled.is_cancelled());
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

    #[test]
    fn fatal_startup_message_is_content_free() {
        assert!(!FATAL_STARTUP_MESSAGE.contains('\\'));
        assert!(!FATAL_STARTUP_MESSAGE.contains('/'));
        assert!(!FATAL_STARTUP_MESSAGE.contains("error="));
        assert!(!FATAL_STARTUP_MESSAGE.contains("model"));
        assert!(!FATAL_STARTUP_MESSAGE.contains("transcript"));
    }

    #[test]
    fn callback_boundary_tail_maps_native_frames_to_the_archival_clip() {
        assert_eq!(tail_start_at_clip_rate(48_000, 48_000, 16_000), 16_000);
        assert_eq!(tail_start_at_clip_rate(47_999, 48_000, 16_000), 15_999);
        assert_eq!(tail_start_at_clip_rate(16_000, 16_000, 16_000), 16_000);
    }

    #[test]
    fn extended_accurate_without_checkpoints_fails_closed_when_spool_read_fails() {
        let recognizer = CountingRecognizer::returning("unused");
        let reads = Cell::new(0_u32);
        let result = transcribe_extended_accurate_with_source(
            &recognizer,
            None,
            FinalAudioStats {
                total_samples: 16_000 * 130,
                peak_retained_samples: 16_000 * 120,
                rms: 0.2,
                auto_stopped: false,
            },
            "en",
            DictationId(90),
            |_| {
                reads.set(reads.get() + 1);
                Err("must not read".to_owned())
            },
        );
        assert!(result.unwrap_err().contains("must not read"));
        assert_eq!(reads.get(), 1);
        assert!(recognizer.clip_lengths.borrow().is_empty());
    }

    #[test]
    fn extended_accurate_without_checkpoints_recovers_in_bounded_overlapping_windows() {
        let recognizer = SequencedRecognizer::new([
            SequencedOutcome::Text("zero one shared two"),
            SequencedOutcome::Text("shared two three four"),
            SequencedOutcome::Text("three four five six"),
            SequencedOutcome::Text("five six seven eight"),
            SequencedOutcome::Text("seven eight nine"),
        ]);
        let requested = RefCell::new(Vec::new());
        let transcript = transcribe_extended_accurate_with_source(
            &recognizer,
            None,
            FinalAudioStats {
                total_samples: 16_000 * 130,
                peak_retained_samples: 16_000 * 120,
                rms: 0.2,
                auto_stopped: false,
            },
            "en",
            DictationId(92),
            |range| {
                requested.borrow_mut().push(range);
                AudioClip::new(vec![0.2; range.len() as usize], 16_000)
                    .map_err(|error| error.to_string())
            },
        )
        .unwrap();
        let ranges = requested.borrow();
        assert_eq!(ranges.len(), 5);
        assert!(ranges.iter().all(|range| range.len() <= 16_000 * 30));
        assert!(
            ranges
                .windows(2)
                .all(|pair| pair[0].end() - pair[1].start() == 16_000 * 2)
        );
        assert_eq!(
            transcript.text,
            "zero one shared two three four five six seven eight nine"
        );
        assert_eq!(transcript.backend, "fake");
        assert_eq!(transcript.audio_duration, Duration::from_secs(130));
    }

    #[test]
    fn extended_accurate_reads_only_the_unresolved_tail_after_120_seconds() {
        let recognizer = CountingRecognizer::returning("tail text");
        let accumulator = PartialAccumulator {
            text: "stable text".to_owned(),
            stable_end: Duration::from_secs(120),
            accepted_through: Duration::from_secs(120),
            timestamp_stable: true,
            last_boundary: Some(BoundaryKind::Silence),
            next_sequence: 6,
            ..PartialAccumulator::default()
        };
        let requested = RefCell::new(Vec::new());
        let transcript = transcribe_extended_accurate_with_source(
            &recognizer,
            Some(accumulator),
            FinalAudioStats {
                total_samples: 16_000 * 130,
                peak_retained_samples: 16_000 * 120,
                rms: 0.2,
                auto_stopped: false,
            },
            "en",
            DictationId(91),
            |range| {
                requested.borrow_mut().push(range);
                AudioClip::new(vec![0.2; range.len() as usize], 16_000)
                    .map_err(|error| error.to_string())
            },
        )
        .unwrap();
        assert_eq!(
            requested.borrow().as_slice(),
            &[SampleRange::new(16_000 * 120, 16_000 * 130).unwrap()]
        );
        assert_eq!(recognizer.clip_lengths.borrow().as_slice(), &[16_000 * 10]);
        assert_eq!(transcript.audio_duration, Duration::from_secs(130));
    }

    #[test]
    fn degraded_instant_recovery_visits_long_audio_sequentially_with_bounded_reads() {
        let total = 16_000 * 185 + 7;
        let maximum = 16_000 * 30;
        let mut ranges = Vec::new();
        for_each_bounded_range(total, maximum, |range| {
            ranges.push(range);
            Ok(())
        })
        .unwrap();
        assert_eq!(ranges.first().unwrap().start(), 0);
        assert_eq!(ranges.last().unwrap().end(), total);
        assert!(ranges.iter().all(|range| range.len() <= maximum));
        assert!(
            ranges
                .windows(2)
                .all(|pair| pair[0].end() == pair[1].start())
        );
    }

    #[test]
    fn persisted_release_lifecycle_includes_audio_finalization_and_terminal_insert() {
        let path = std::env::temp_dir().join(format!(
            "phorminx-release-timing-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        let persistence = Persistence::open(&path).unwrap();
        persistence
            .history()
            .set_retention(RetentionPolicy::Indefinite, now_ms())
            .unwrap();
        let released_at = Instant::now();
        let mut processed = process_transcript(
            transcript("timed"),
            "en",
            &WorkerFormatting {
                profile: FormatProfile::Raw,
                model: None,
                keep_alive: KeepAlive::UnloadAfterRequest,
            },
            None,
            &[],
            None,
            &CancellationToken::new(),
        );
        processed.terminal = TerminalMetadata {
            checkpoint_count: Some(7),
            checkpoint_repair_count: Some(1),
            peak_retained_audio_ms: Some(120_000),
            formatting_chunk_count: Some(3),
            auto_stopped: Some(false),
        };
        processed.lifecycle = Some(LifecycleTiming {
            release: ReleaseTiming {
                released_at,
                audio_finalized_at: released_at + Duration::from_millis(11),
            },
            worker_started_at: released_at + Duration::from_millis(19),
            worker_completed_at: released_at + Duration::from_millis(73),
        });
        persist_transcript(
            &persistence,
            &processed,
            released_at + Duration::from_millis(80),
            released_at + Duration::from_millis(91),
        )
        .unwrap();
        let record = persistence.history().recent(1).unwrap().remove(0);
        assert_eq!(
            record.dictation.timings.audio_finalization_duration_ms,
            Some(11)
        );
        assert_eq!(record.dictation.timings.worker_queue_duration_ms, Some(8));
        assert_eq!(record.dictation.timings.insertion_duration_ms, Some(11));
        assert_eq!(
            record.dictation.timings.release_to_insert_duration_ms,
            Some(91)
        );
        assert_eq!(
            persistence.history().terminal_metadata(record.id).unwrap(),
            Some(processed.terminal)
        );
        drop(persistence);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn instant_continuity_accepts_44k1_and_48k_callback_boundaries() {
        for sample_rate in [44_100, 48_000] {
            let native = (0..sample_rate)
                .map(|index| {
                    ((index as f32 * 2.0 * std::f32::consts::PI * 233.0) / sample_rate as f32).sin()
                        * 0.2
                })
                .collect::<Vec<_>>();
            let archival = phorminx_audio::resample(&native, sample_rate, 16_000).unwrap();
            let clip = AudioClip::new(archival, 16_000).unwrap();
            assert!(instant_stream_is_continuous(
                u64::from(sample_rate - 256),
                sample_rate,
                &native[..2_048],
                &clip,
            ));
        }
    }

    #[test]
    fn instant_continuity_rejects_dropped_mismatched_and_late_batches() {
        let native = (0..48_000)
            .map(|index| ((index as f32) / 37.0).sin() * 0.2)
            .collect::<Vec<_>>();
        let clip = AudioClip::new(
            phorminx_audio::resample(&native, 48_000, 16_000).unwrap(),
            16_000,
        )
        .unwrap();
        let mismatched = native[..2_048].iter().rev().copied().collect::<Vec<_>>();
        assert!(!instant_stream_is_continuous(
            24_000,
            48_000,
            &mismatched,
            &clip,
        ));
        assert!(!instant_stream_is_continuous(
            48_100,
            48_000,
            &native[..2_048],
            &clip,
        ));
        assert!(!instant_stream_is_continuous(1_000, 48_000, &[], &clip));
    }

    #[test]
    fn instant_rejects_profile_language_that_does_not_match_resident_model() {
        assert!(profile_language_matches_resident_model(
            RecognitionMode::Instant,
            "en",
            "EN"
        ));
        assert!(!profile_language_matches_resident_model(
            RecognitionMode::Instant,
            "en",
            "pt-br"
        ));
        assert!(profile_language_matches_resident_model(
            RecognitionMode::Accurate,
            "en",
            "pt-br"
        ));
    }

    #[test]
    fn existing_incompatible_profile_blocks_before_any_runtime_work() {
        let path = std::env::temp_dir().join(format!(
            "phorminx-profile-language-gate-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        let persistence = Persistence::open(&path).unwrap();
        persistence
            .app_profiles()
            .upsert(&AppProfile {
                executable: ExecutableIdentity::new("code.exe").unwrap(),
                formatting_style: FormattingStyle::Light,
                custom_instructions: None,
                language: Some("pt-br".to_owned()),
                insertion_preference: InsertionPreference::Automatic,
                deny: false,
            })
            .unwrap();
        let mut settings = Settings::default();
        settings.recognition.mode = RecognitionMode::Instant;
        settings.recognition.language = "en".to_owned();

        let context = DictationContext::for_executable(
            DictationContext::global(&settings, None).unwrap(),
            Some("code.exe".to_owned()),
            &persistence,
            &settings,
        )
        .unwrap();
        assert_eq!(
            context.activation_block,
            Some(ActivationBlockReason::InstantLanguageMismatch)
        );
        assert_eq!(
            context.activation_block.unwrap().label(),
            "instant_language_mismatch"
        );
        assert!(!context.activation_block.unwrap().message().is_empty());

        // This is the same gate used by the event loop before it configures the
        // runtime or calls `hold_started`. None of those effects are eligible.
        let recording_starts = Cell::new(0);
        let stt_submissions = Cell::new(0);
        let insertions = Cell::new(0);
        if context.activation_block.is_none() {
            recording_starts.set(recording_starts.get() + 1);
            stt_submissions.set(stt_submissions.get() + 1);
            insertions.set(insertions.get() + 1);
        }
        assert_eq!(recording_starts.get(), 0);
        assert_eq!(stt_submissions.get(), 0);
        assert_eq!(insertions.get(), 0);

        drop(persistence);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn only_successful_terminal_outcomes_are_latency_and_history_eligible() {
        let id = DictationId(7);
        let cases: Vec<(RuntimeNotice<&str>, TerminalOutcome, bool)> = vec![
            (
                RuntimeNotice::Inserted {
                    id,
                    inference_time: Duration::from_millis(2),
                },
                TerminalOutcome::Inserted,
                true,
            ),
            (
                RuntimeNotice::ClipboardReady {
                    id,
                    reason: "clipboard",
                },
                TerminalOutcome::ClipboardReady,
                true,
            ),
            (
                RuntimeNotice::NoSpeech {
                    id,
                    event: "silence_rejected",
                },
                TerminalOutcome::EmptyTranscript,
                false,
            ),
            (
                RuntimeNotice::Failure {
                    id: Some(id),
                    event: "insertion_failed",
                    message: "content-free test failure".to_owned(),
                },
                TerminalOutcome::InsertionFailed,
                false,
            ),
            (
                RuntimeNotice::StaleTranscription {
                    id,
                    state: RuntimeState::Idle,
                },
                TerminalOutcome::Stale,
                false,
            ),
        ];

        for (notice, expected, eligible) in cases {
            let outcome = terminal_outcome(&[notice]);
            assert_eq!(outcome, expected);
            assert_eq!(outcome.is_success(), eligible);
        }
        let other = terminal_outcome::<&str>(&[]);
        assert_eq!(other, TerminalOutcome::OtherFailure);
        assert!(!other.is_success());
    }

    #[test]
    fn resident_loader_loads_and_probes_exactly_once_then_retains_value() {
        let loads = Cell::new(0);
        let probes = Cell::new(0);
        let resident = load_and_probe_resident(
            || {
                loads.set(loads.get() + 1);
                Ok::<_, ()>("resident")
            },
            |loaded| {
                assert_eq!(*loaded, "resident");
                probes.set(probes.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(resident, "resident");
        assert_eq!(loads.get(), 1);
        assert_eq!(probes.get(), 1);
    }

    #[test]
    fn active_shell_uses_resident_instant_readiness_without_another_load() {
        assert_eq!(
            active_shell_vosk_probe(RecognitionMode::Instant),
            UiVoskProbe::ResidentReady
        );
        assert_eq!(
            active_shell_vosk_probe(RecognitionMode::Accurate),
            UiVoskProbe::LayoutOnly
        );
        assert_eq!(setup_shell_vosk_probe(), UiVoskProbe::FullValidation);
    }
}

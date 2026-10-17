//! Opt-in Tauri shell adapter for the existing dictation runtime. This keeps
//! hotkeys, tray, and speech workers in their established host process while
//! the replacement presentation is being brought to parity.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use phorminx_audio::input_devices;
use phorminx_ollama::{ClientTimeouts, OllamaClient, OllamaEndpoint};
use phorminx_persistence::{CasePolicy, FormattingStyle, InsertionPreference};
use phorminx_ui::workspace::WorkspaceEvent;
use phorminx_whisper::WhisperReadiness;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::feedback::{FeedbackPanel, FeedbackView};
use crate::library_search::{LibrarySearchConfig, LibrarySearchWorker, embedding_identity};
use crate::product_shell::{ProductShellControl, ProductShellEvent};
use crate::settings::{
    AccurateBackendPreference, AccurateModelVariant, AppearancePreference, FormattingStrength,
    HistoryRetention, OllamaLifecycle, OllamaModelIdentity, RecognitionMode, RecordingMode,
    SettingsStore,
};
use crate::setup_center::{BenchmarkUnavailable, SetupCenter};
use crate::tauri_contract::{AppView, LibrarySearchView, ProfileDetailView, SetupView};
use crate::tauri_meetings::{MeetingTauriState, open_companion};
use crate::tauri_messaging::MessagingHost;
use crate::update_center::{UpdateCenter, UpdateView};
use crate::ui_bridge::{
    DEFAULT_HISTORY_LIMIT, UiBridge, UiCommand, UiCommandOutcome, UiEffect, UiHistoryVariant,
    UiLexiconDraft, UiProfileDraft, UiReadinessSnapshot, UiReadinessState, UiRoute,
    UiRuntimeStatus, UiVoskProbe,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostView {
    route: String,
    runtime_status: String,
    notice: String,
}

struct HostState {
    bridge: Mutex<UiBridge>,
    store: SettingsStore,
    readiness: Mutex<UiReadinessSnapshot>,
    setup: Mutex<Option<SetupCenter>>,
    updates: Mutex<UpdateCenter>,
    bootstrap: Mutex<BootstrapCenter>,
    bootstrap_allowed: bool,
    feedback: Mutex<FeedbackPanel>,
    library: Mutex<Option<LibrarySearchWorker>>,
    library_generation: AtomicU64,
    route: Mutex<UiRoute>,
    status: Mutex<UiRuntimeStatus>,
    notice: Mutex<String>,
    events: Sender<ProductShellEvent>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapView {
    available: bool,
    state: String,
    message: String,
    step: u8,
    total: u8,
}

#[derive(Deserialize)]
struct BootstrapStatus {
    state: String,
    message: String,
    step: u8,
    total: u8,
    run_id: String,
}

struct BootstrapCenter {
    child: Option<Child>,
    run_id: String,
    status_path: PathBuf,
    view: BootstrapView,
    completion_reported: bool,
}

impl BootstrapCenter {
    fn new(status_path: PathBuf, available: bool) -> Self {
        Self {
            child: None,
            run_id: String::new(),
            status_path,
            view: BootstrapView {
                available,
                state: "idle".into(),
                message: if available {
                    "Install the recommended local starter in one guided run.".into()
                } else {
                    "To run the complete starter setup, open Prepare Phorminx from the Start menu.".into()
                },
                step: 0,
                total: 6,
            },
            completion_reported: false,
        }
    }

    fn start(&mut self) -> Result<(), String> {
        if !self.view.available { return Err("Open Prepare Phorminx from the Start menu.".into()); }
        if self.child.is_some() { return Err("Starter setup is already running.".into()); }
        let executable = std::env::current_exe().map_err(|_| "Could not locate Phorminx.")?;
        let script = executable.parent().ok_or("Could not locate Phorminx.")?
            .join("setup").join("Initialize-Phorminx.ps1");
        if !script.is_file() { return Err("The installed setup component is missing. Repair or reinstall Phorminx.".into()); }
        let powershell = PathBuf::from(std::env::var_os("WINDIR").ok_or("Windows directory is unavailable.")?)
            .join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
        if !powershell.is_file() { return Err("Windows PowerShell is unavailable.".into()) }
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "System clock is unavailable.")?.as_nanos();
        let run_id = format!("{}-{nonce}", std::process::id());
        let mut command = Command::new(powershell);
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden",
            "-ExecutionPolicy", "Bypass", "-File"])
            .arg(script)
            .args(["-FromApp", "-HostProcessId"])
            .arg(std::process::id().to_string())
            .args(["-RunId", &run_id])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(windows)] {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = command.spawn().map_err(|_| "Could not start the local setup helper.")?;
        self.run_id = run_id;
        self.child = Some(child);
        self.completion_reported = false;
        self.view.state = "working".into();
        self.view.message = "Preparing the local starter bundle…".into();
        self.view.step = 0;
        Ok(())
    }

    fn cancel_after_step(&mut self) -> Result<(), String> {
        if self.child.is_none() { return Err("Starter setup is not running.".into()); }
        if self.view.state == "cancelling" { return Ok(()); }
        let directory = self.status_path.parent().ok_or("Setup storage is unavailable.")?;
        std::fs::create_dir_all(directory).map_err(|_| "Setup storage is unavailable.")?;
        let marker = directory.join(format!("setup-cancel-{}.flag", self.run_id));
        let file = std::fs::OpenOptions::new().create_new(true).write(true).open(marker)
            .map_err(|_| "Could not request a safe stop. Setup is still running.")?;
        file.sync_all().map_err(|_| "Could not request a safe stop. Setup is still running.")?;
        self.view.state = "cancelling".into();
        self.view.message = "Stopping after the current download or installation step…".into();
        Ok(())
    }

    fn poll(&mut self) -> Result<bool, String> {
        let Some(child) = self.child.as_mut() else { return Ok(false); };
        if let Ok(bytes) = std::fs::read(&self.status_path)
            && bytes.len() <= 4096
            && let Ok(status) = serde_json::from_slice::<BootstrapStatus>(&bytes)
            && status.run_id == self.run_id
        {
            if self.view.state != "cancelling" || status.state != "working" {
                self.view.state = status.state;
                self.view.message = status.message;
            }
            self.view.step = status.step.min(6);
            self.view.total = status.total;
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                self.child = None;
                if !exit.success() && self.view.state != "failed" && self.view.state != "cancelled" {
                    self.view.state = "failed".into();
                    self.view.message = "Starter setup stopped. Existing data was preserved; retry to reuse completed downloads.".into();
                }
                if exit.success() && self.view.state != "complete" {
                    self.view.state = "failed".into();
                    self.view.message = "Setup ended without a completion receipt. Review and retry before using the starter.".into();
                }
                if self.view.state == "complete" && !self.completion_reported {
                    self.completion_reported = true;
                    return Ok(true);
                }
            }
            Ok(None) => {}
            Err(_) => return Err("Could not inspect the setup helper.".into()),
        }
        Ok(false)
    }
}

#[tauri::command]
fn host_app_snapshot(state: State<'_, HostState>) -> Result<AppView, String> {
    let mut bridge = state
        .bridge
        .lock()
        .map_err(|_| "App data is unavailable.")?;
    bridge
        .reload_settings()
        .map_err(|error| error.to_string())?;
    let readiness = state
        .readiness
        .lock()
        .map_err(|_| "Readiness is unavailable.")?
        .clone();
    let status = *state
        .status
        .lock()
        .map_err(|_| "Runtime state is unavailable.")?;
    let snapshot = bridge
        .snapshot(status, readiness, DEFAULT_HISTORY_LIMIT)
        .map_err(|error| error.to_string())?;
    Ok(AppView::from(&snapshot))
}

#[tauri::command]
fn host_input_devices() -> Result<Vec<String>, String> {
    input_devices()
        .map(|devices| devices.into_iter().map(|device| device.name).collect())
        .map_err(|_| "Microphone devices could not be listed.".to_owned())
}

#[tauri::command]
fn host_history_text(
    id: i64,
    variant: Option<String>,
    state: State<'_, HostState>,
) -> Result<Option<String>, String> {
    let variant = match variant.as_deref().unwrap_or("output") {
        "output" => UiHistoryVariant::Output,
        "raw" => UiHistoryVariant::Raw,
        "normalized" => UiHistoryVariant::Normalized,
        "cleaned" => UiHistoryVariant::Cleaned,
        _ => return Err("Choose an available text version.".into()),
    };
    state
        .bridge
        .lock()
        .map_err(|_| "History is unavailable.")?
        .history_text(id, variant)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn host_library_search(
    query: String,
    semantic: bool,
    state: State<'_, HostState>,
) -> Result<u64, String> {
    if query.chars().count() > 1024 {
        return Err("Search is limited to 1,024 characters.".into());
    }
    let generation = state.library_generation.fetch_add(1, Ordering::AcqRel) + 1;
    let library = state.library.lock().map_err(|_| "Search is unavailable.")?;
    let worker = library
        .as_ref()
        .ok_or("Search is unavailable in this session.")?;
    worker.handle().search(generation, query, semantic);
    Ok(generation)
}

#[tauri::command]
fn host_library_update(state: State<'_, HostState>) -> Result<Option<LibrarySearchView>, String> {
    let library = state.library.lock().map_err(|_| "Search is unavailable.")?;
    Ok(library
        .as_ref()
        .and_then(LibrarySearchWorker::try_recv)
        .map(Into::into))
}

#[tauri::command]
fn host_select_route(route: String, state: State<'_, HostState>) -> Result<(), String> {
    let route = match route.as_str() {
        "home" => UiRoute::Home,
        "history" => UiRoute::History,
        "lexicon" => UiRoute::Lexicon,
        "profiles" => UiRoute::Profiles,
        "models" => UiRoute::Models,
        "settings" => UiRoute::Settings,
        "meetings" => UiRoute::Meetings,
        "actions" => UiRoute::Actions,
        "setup" => UiRoute::Setup,
        _ => return Err("Unknown route.".into()),
    };
    *state.route.lock().map_err(|_| "Route is unavailable.")? = route;
    Ok(())
}

#[tauri::command]
fn host_save_preferences(
    theme: String,
    recording_mode: String,
    launcher_shortcut: String,
    state: State<'_, HostState>,
) -> Result<(), String> {
    let theme = match theme.as_str() {
        "system" => AppearancePreference::System,
        "light" => AppearancePreference::Light,
        "dark" => AppearancePreference::Dark,
        _ => return Err("Choose a supported appearance.".into()),
    };
    let recording_mode = match recording_mode.as_str() {
        "hold" => RecordingMode::Hold,
        "toggle" => RecordingMode::Toggle,
        _ => return Err("Choose a supported recording mode.".into()),
    };
    let mut bridge = state
        .bridge
        .lock()
        .map_err(|_| "Settings are unavailable.")?;
    bridge
        .reload_settings()
        .map_err(|error| error.to_string())?;
    let mut candidate = bridge.settings().clone();
    candidate.appearance.theme = theme;
    candidate.interaction.recording_mode = recording_mode;
    candidate.interaction.launcher_shortcut = launcher_shortcut;
    let readiness = state
        .readiness
        .lock()
        .map_err(|_| "Readiness is unavailable.")?
        .clone();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let outcome = bridge
        .execute(UiCommand::SaveSettings(candidate), &readiness, now_ms)
        .map_err(|error| error.to_string())?;
    state
        .events
        .send(ProductShellEvent::RuntimeReloadRequested(outcome.mutation))
        .map_err(|_| "Settings were saved, but the runtime could not be notified.".to_owned())
}

#[tauri::command]
fn host_custom_instructions(state: State<'_, HostState>) -> Result<Option<String>, String> {
    Ok(state
        .bridge
        .lock()
        .map_err(|_| "Settings are unavailable.")?
        .settings()
        .formatting
        .custom_instructions
        .clone())
}

#[tauri::command]
fn host_save_dictation_settings(
    mode: String,
    backend: String,
    microphone: Option<String>,
    language: String,
    minimum_rms: String,
    strength: String,
    model: Option<String>,
    lifecycle: String,
    custom_instructions: Option<String>,
    state: State<'_, HostState>,
) -> Result<(), String> {
    let mode = match mode.as_str() {
        "accurate" => RecognitionMode::Accurate,
        "instant" => RecognitionMode::Instant,
        _ => return Err("Choose a supported recognition mode.".into()),
    };
    let backend = match backend.as_str() {
        "auto" => AccurateBackendPreference::Auto,
        "vulkan" => AccurateBackendPreference::Vulkan,
        "cpu" => AccurateBackendPreference::Cpu,
        _ => return Err("Choose a supported compute backend.".into()),
    };
    let strength = match strength.as_str() {
        "raw" => FormattingStrength::Raw,
        "light" => FormattingStrength::Light,
        "balanced" => FormattingStrength::Balanced,
        "strong" => FormattingStrength::Strong,
        "custom" => FormattingStrength::Custom,
        _ => return Err("Choose a supported formatting strength.".into()),
    };
    let lifecycle = match lifecycle.as_str() {
        "instant" => OllamaLifecycle::Instant,
        "balanced" => OllamaLifecycle::Balanced,
        "memorysaver" => OllamaLifecycle::MemorySaver,
        _ => return Err("Choose a supported model residency.".into()),
    };
    let minimum_rms = minimum_rms
        .parse::<f32>()
        .map_err(|_| "Enter a valid minimum speech level.".to_owned())?;
    let mut candidate = state
        .bridge
        .lock()
        .map_err(|_| "Settings are unavailable.")?
        .settings()
        .clone();
    candidate.recognition.mode = mode;
    candidate.recognition.accurate_backend = backend;
    candidate.recognition.microphone = match microphone.filter(|name| !name.trim().is_empty()) {
        Some(name) => {
            if !host_input_devices()?.contains(&name) {
                return Err("Choose an available microphone or Windows default.".into());
            }
            Some(name)
        }
        None => None,
    };
    candidate.recognition.language = language;
    candidate.recognition.minimum_rms = minimum_rms;
    candidate.formatting.strength = strength;
    candidate.formatting.ollama_lifecycle = lifecycle;
    candidate.formatting.custom_instructions = custom_instructions;
    let selected = model.filter(|model| !model.trim().is_empty());
    if selected != candidate.formatting.ollama_model {
        let readiness = state
            .readiness
            .lock()
            .map_err(|_| "Model catalog is unavailable.")?;
        candidate.formatting.ollama_model_identity = match selected.as_ref() {
            Some(name) => {
                let installed = readiness
                    .ollama
                    .models
                    .iter()
                    .find(|item| &item.name == name)
                    .ok_or("Choose an installed local model.")?;
                Some(
                    OllamaModelIdentity::new(
                        installed
                            .digest
                            .as_deref()
                            .ok_or("The selected model has no verified digest.")?,
                        installed
                            .size_bytes
                            .ok_or("The selected model has no verified size.")?,
                    )
                    .map_err(|error| error.to_string())?,
                )
            }
            None => None,
        };
        candidate.formatting.ollama_model = selected;
    }
    state.execute(UiCommand::SaveSettings(candidate))
}

#[tauri::command]
fn host_save_privacy_settings(
    retention: String,
    embedding_model: Option<String>,
    launch_at_login: bool,
    direct_shortcut: Option<String>,
    state: State<'_, HostState>,
) -> Result<(), String> {
    let retention = match retention.as_str() {
        "disabled" => HistoryRetention::Disabled,
        "one-day" => HistoryRetention::OneDay,
        "seven-days" => HistoryRetention::SevenDays,
        "thirty-days" => HistoryRetention::ThirtyDays,
        "indefinite" => HistoryRetention::Indefinite,
        _ => return Err("Choose a supported history retention.".into()),
    };
    let mut candidate = state
        .bridge
        .lock()
        .map_err(|_| "Settings are unavailable.")?
        .settings()
        .clone();
    candidate.privacy.history_retention = retention;
    candidate.search.embedding_model = embedding_model;
    candidate.startup.launch_at_login = launch_at_login;
    candidate.interaction.direct_dictation_shortcut = direct_shortcut;
    state.execute(UiCommand::SaveSettings(candidate))
}

impl HostState {
    fn notice(&self, message: &str) {
        if let Ok(mut notice) = self.notice.lock() {
            *notice = message.to_owned();
        }
    }

    fn execute(&self, command: UiCommand) -> Result<(), String> {
        let mut bridge = self.bridge.lock().map_err(|_| "App data is unavailable.")?;
        bridge
            .reload_settings()
            .map_err(|error| error.to_string())?;
        let readiness = self
            .readiness
            .lock()
            .map_err(|_| "Readiness is unavailable.")?
            .clone();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let outcome = bridge
            .execute(command, &readiness, now_ms)
            .map_err(|error| error.to_string())?;
        dispatch_settings_outcome(&self.events, outcome)?;
        self.configure_library();
        Ok(())
    }

    fn configure_library(&self) {
        let Ok(settings) = self.store.load() else {
            return;
        };
        let Ok(readiness) = self.readiness.lock() else {
            return;
        };
        let busy = self.status.lock().is_ok_and(|status| {
            matches!(
                *status,
                UiRuntimeStatus::Listening
                    | UiRuntimeStatus::Transcribing
                    | UiRuntimeStatus::Refining
            )
        });
        let Some(model) = settings.search.embedding_model.clone() else {
            if let Ok(library) = self.library.lock()
                && let Some(worker) = library.as_ref()
            {
                worker.handle().configure(LibrarySearchConfig {
                    model: None,
                    model_identity: None,
                    history_enabled: settings.privacy.history_retention
                        != HistoryRetention::Disabled,
                });
                worker.handle().set_paused(busy);
            }
            return;
        };
        let identity = readiness
            .ollama
            .models
            .iter()
            .find(|item| item.name == model)
            .and_then(|item| item.digest.as_deref())
            .map(|digest| embedding_identity(&model, digest));
        if let Ok(library) = self.library.lock()
            && let Some(worker) = library.as_ref()
        {
            worker.handle().configure(LibrarySearchConfig {
                model: Some(model),
                model_identity: identity,
                history_enabled: settings.privacy.history_retention != HistoryRetention::Disabled,
            });
            worker.handle().set_paused(busy);
        }
    }
}

fn dispatch_settings_outcome(
    events: &Sender<ProductShellEvent>,
    outcome: UiCommandOutcome,
) -> Result<(), String> {
    // A SettingsSaved reload exits the current runtime loop. Apply external
    // startup registration first, or that reload can strand the queued
    // launch-at-login effect until a later user action.
    for effect in outcome.effects {
        if let UiEffect::ApplyLaunchAtLogin(enabled) = effect {
            events
                .send(ProductShellEvent::ApplyLaunchAtLogin(enabled))
                .map_err(|_| "The preference was saved, but startup could not be updated.".to_owned())?;
        }
    }
    events
        .send(ProductShellEvent::RuntimeReloadRequested(outcome.mutation))
        .map_err(|_| "The change was saved, but the runtime could not be notified.".to_owned())
}

#[tauri::command]
fn host_save_lexicon(
    id: Option<i64>,
    canonical: String,
    alias: String,
    language: Option<String>,
    app_executable: Option<String>,
    case_policy: String,
    enabled: bool,
    state: State<'_, HostState>,
) -> Result<(), String> {
    let case_policy = match case_policy.as_str() {
        "preserve-input" => CasePolicy::PreserveInput,
        "canonical" => CasePolicy::UseCanonical,
        "lowercase" => CasePolicy::Lowercase,
        "uppercase" => CasePolicy::Uppercase,
        _ => return Err("Choose a supported case policy.".into()),
    };
    state.execute(UiCommand::SaveLexicon(UiLexiconDraft {
        id,
        canonical,
        alias,
        language,
        app_executable,
        case_policy,
        enabled,
    }))
}

#[tauri::command]
fn host_delete_lexicon(id: i64, state: State<'_, HostState>) -> Result<(), String> {
    state.execute(UiCommand::DeleteLexicon(id))
}

#[tauri::command]
fn host_profile_detail(
    executable: String,
    state: State<'_, HostState>,
) -> Result<Option<ProfileDetailView>, String> {
    state
        .bridge
        .lock()
        .map_err(|_| "Profiles are unavailable.")?
        .profile_detail(&executable)
        .map(|profile| profile.as_ref().map(ProfileDetailView::from))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn host_save_profile(
    original_executable: Option<String>,
    executable: String,
    formatting_style: String,
    custom_instructions: Option<String>,
    language: Option<String>,
    insertion_preference: String,
    deny: bool,
    state: State<'_, HostState>,
) -> Result<(), String> {
    let formatting_style = match formatting_style.as_str() {
        "raw" => FormattingStyle::Raw,
        "light" => FormattingStyle::Light,
        "balanced" => FormattingStyle::Balanced,
        "strong" => FormattingStyle::Strong,
        "custom" => FormattingStyle::Custom,
        _ => return Err("Choose a supported formatting style.".into()),
    };
    let insertion_preference = match insertion_preference.as_str() {
        "automatic" => InsertionPreference::Automatic,
        "direct" => InsertionPreference::Direct,
        "clipboard" => InsertionPreference::Clipboard,
        _ => return Err("Choose a supported insertion preference.".into()),
    };
    state.execute(UiCommand::SaveProfile(UiProfileDraft {
        original_executable,
        executable,
        formatting_style,
        custom_instructions,
        language,
        insertion_preference,
        deny,
    }))
}

#[tauri::command]
fn host_delete_profile(executable: String, state: State<'_, HostState>) -> Result<(), String> {
    state.execute(UiCommand::DeleteProfile(executable))
}

#[tauri::command]
fn host_setup_snapshot(
    app: tauri::AppHandle,
    state: State<'_, HostState>,
) -> Result<SetupView, String> {
    let readiness = state
        .readiness
        .lock()
        .map_err(|_| "Readiness is unavailable.")?
        .clone();
    let mut setup = state.setup.lock().map_err(|_| "Setup is unavailable.")?;
    let center = setup
        .as_mut()
        .ok_or("Setup is unavailable in this session.")?;
    let changed = center.poll().map_err(str::to_owned)?;
    let terminal = changed && center.terminal();
    let mut view = SetupView::from(&center.snapshot(&readiness));
    view.can_start = state.bootstrap_allowed
        && readiness.microphone.state == UiReadinessState::Ready
        && (readiness.whisper.state == UiReadinessState::Ready
            || readiness.vosk.state == UiReadinessState::Ready);
    drop(setup);
    if terminal {
        let mut bridge = state
            .bridge
            .lock()
            .map_err(|_| "Settings are unavailable.")?;
        let before = bridge.settings().recognition.clone();
        bridge
            .reload_settings()
            .map_err(|error| error.to_string())?;
        if before != bridge.settings().recognition {
            let _ = state.events.send(ProductShellEvent::RuntimeReloadRequested(
                crate::ui_bridge::UiMutation::SettingsSaved,
            ));
        }
        drop(bridge);
        let _ = spawn_readiness_probe(app, None, UiVoskProbe::FullValidation);
    }
    Ok(view)
}

#[tauri::command]
fn host_start_setup_action(
    id: String,
    consent_confirmed: bool,
    state: State<'_, HostState>,
) -> Result<(), String> {
    if state.bootstrap.lock().map_err(|_| "Starter setup is unavailable.")?.child.is_some() {
        return Err("Wait for the recommended starter to finish before running a repair step.".into());
    }
    let readiness = state
        .readiness
        .lock()
        .map_err(|_| "Readiness is unavailable.")?
        .clone();
    let mut setup = state.setup.lock().map_err(|_| "Setup is unavailable.")?;
    let center = setup
        .as_mut()
        .ok_or("Setup is unavailable in this session.")?;
    center.poll().map_err(str::to_owned)?;
    let snapshot = center.snapshot(&readiness);
    let action = snapshot
        .actions
        .iter()
        .find(|action| action.id == id && action.available)
        .ok_or("That setup action is not currently available.")?;
    if !action.consent.is_empty() && !consent_confirmed {
        return Err("Confirm the listed changes before starting setup.".into());
    }
    center.start(&id).map_err(str::to_owned)
}

#[tauri::command]
fn host_bootstrap_snapshot(app: tauri::AppHandle, state: State<'_, HostState>) -> Result<BootstrapView, String> {
    let mut bootstrap = state.bootstrap.lock().map_err(|_| "Starter setup is unavailable.")?;
    let completed = bootstrap.poll()?;
    let view = bootstrap.view.clone();
    drop(bootstrap);
    if completed {
        let _ = state.bridge.lock().map_err(|_| "Settings are unavailable.")?.reload_settings();
        let _ = spawn_readiness_probe(app, None, UiVoskProbe::FullValidation);
    }
    Ok(view)
}

#[tauri::command]
fn host_start_bootstrap(state: State<'_, HostState>) -> Result<BootstrapView, String> {
    if !state.bootstrap_allowed { return Err("Open Prepare Phorminx from the Start menu to run the full starter.".into()); }
    let readiness = state.readiness.lock().map_err(|_| "Readiness is unavailable.")?.clone();
    let busy = state.setup.lock().map_err(|_| "Setup is unavailable.")?.as_ref()
        .is_some_and(|center| center.snapshot(&readiness).actions.iter().any(|action| action.running));
    if busy { return Err("Wait for the current repair step to finish.".into()); }
    let mut bootstrap = state.bootstrap.lock().map_err(|_| "Starter setup is unavailable.")?;
    bootstrap.start()?;
    Ok(bootstrap.view.clone())
}

#[tauri::command]
fn host_cancel_bootstrap(state: State<'_, HostState>) -> Result<BootstrapView, String> {
    let mut bootstrap = state.bootstrap.lock().map_err(|_| "Starter setup is unavailable.")?;
    bootstrap.cancel_after_step()?;
    Ok(bootstrap.view.clone())
}

#[tauri::command]
fn host_cancel_setup_action(id: String, state: State<'_, HostState>) -> Result<(), String> {
    state
        .setup
        .lock()
        .map_err(|_| "Setup is unavailable.")?
        .as_mut()
        .ok_or("Setup is unavailable in this session.")?
        .cancel(&id)
        .map_err(str::to_owned)
}

fn update_idle(app: &tauri::AppHandle) -> bool {
    let host = app.state::<HostState>();
    // Let onboarding present its opt-out before background delivery begins.
    if host.bootstrap_allowed { return false; }
    let runtime_idle = host.status.lock().is_ok_and(|status| !matches!(
        *status,
        UiRuntimeStatus::Starting | UiRuntimeStatus::Listening
            | UiRuntimeStatus::Transcribing | UiRuntimeStatus::Refining
    ));
    let meeting_idle = app.state::<MeetingTauriState>()
        .snapshot(u64::MAX)
        .ok()
        .flatten()
        .is_some_and(|meeting| meeting.capture == "idle");
    runtime_idle && meeting_idle
}

#[tauri::command]
fn host_update_snapshot(app: tauri::AppHandle, state: State<'_, HostState>) -> Result<UpdateView, String> {
    let idle = update_idle(&app);
    let mut updates = state.updates.lock().map_err(|_| "Updates are unavailable.")?;
    updates.poll(idle);
    Ok(updates.snapshot())
}

#[tauri::command]
fn host_update_action(kind: String, enabled: Option<bool>, app: tauri::AppHandle, state: State<'_, HostState>) -> Result<UpdateView, String> {
    let idle = update_idle(&app);
    let mut updates = state.updates.lock().map_err(|_| "Updates are unavailable.")?;
    updates.poll(idle);
    match kind.as_str() {
        "check" => updates.check_now(),
        "download" => updates.download_now(),
        "install-on-exit" => updates.install_on_exit(idle)?,
        "restart-now" => {
            updates.restart_to_update(idle, false)?;
            state.events.send(ProductShellEvent::QuitRequested)
                .map_err(|_| "Phorminx could not restart. Close it from the tray to finish updating.")?;
        }
        "daily-checks" => updates.set_daily_checks(enabled.ok_or("Choose on or off.")?)?,
        "automatic-install" => updates.set_automatic_install(enabled.ok_or("Choose on or off.")?)?,
        _ => return Err("Unknown update action.".into()),
    }
    Ok(updates.snapshot())
}

#[tauri::command]
fn host_snapshot(state: State<'_, HostState>) -> Result<HostView, String> {
    Ok(HostView {
        route: format!(
            "{:?}",
            *state.route.lock().map_err(|_| "Route is unavailable.")?
        )
        .to_lowercase(),
        runtime_status: format!(
            "{:?}",
            *state
                .status
                .lock()
                .map_err(|_| "Runtime state is unavailable.")?
        )
        .to_lowercase(),
        notice: state
            .notice
            .lock()
            .map_err(|_| "Status is unavailable.")?
            .clone(),
    })
}

#[tauri::command]
fn host_action(kind: String, state: State<'_, HostState>) -> Result<(), String> {
    let event = match kind.as_str() {
        "test-dictation" => ProductShellEvent::TestDictation,
        "begin-shortcut-capture" => ProductShellEvent::ShortcutCapture(true),
        "end-shortcut-capture" => ProductShellEvent::ShortcutCapture(false),
        "start-after-setup" => {
            if !state.bootstrap_allowed { return Err("Phorminx is already running.".into()) }
            if state.bootstrap.lock().map_err(|_| "Setup is unavailable.")?.child.is_some() {
                return Err("Wait for starter setup to finish.".into());
            }
            let readiness = state.readiness.lock().map_err(|_| "Readiness is unavailable.")?;
            if readiness.microphone.state != UiReadinessState::Ready
                || (readiness.whisper.state != UiReadinessState::Ready
                    && readiness.vosk.state != UiReadinessState::Ready)
            {
                return Err("Finish installing a speech engine and check microphone access first.".into());
            }
            ProductShellEvent::RuntimeReloadRequested(crate::ui_bridge::UiMutation::SettingsSaved)
        }
        _ => return Err("Unknown host action.".into()),
    };
    state
        .events
        .send(event)
        .map_err(|_| "The dictation host is unavailable.".into())
}

#[tauri::command]
fn host_select_whisper_model(
    manifest_id: String,
    state: State<'_, HostState>,
) -> Result<(), String> {
    let variant = AccurateModelVariant::from_manifest_id(&manifest_id)
        .ok_or("Choose a supported Whisper model.")?;
    state
        .events
        .send(ProductShellEvent::ChangeWhisperModel(variant))
        .map_err(|_| "The model manager is unavailable.".into())
}

#[tauri::command]
fn host_install_vosk(state: State<'_, HostState>) -> Result<(), String> {
    state
        .events
        .send(ProductShellEvent::InstallVerifiedVoskAssets)
        .map_err(|_| "The model manager is unavailable.".into())
}

#[tauri::command]
fn feedback_snapshot(state: State<'_, HostState>) -> Result<FeedbackView, String> {
    let mut feedback = state
        .feedback
        .lock()
        .map_err(|_| "Feedback is unavailable.")?;
    feedback.poll();
    Ok(feedback.snapshot())
}

#[tauri::command]
fn feedback_update(
    text: String,
    diagnostics: bool,
    state: State<'_, HostState>,
) -> Result<(), String> {
    state
        .feedback
        .lock()
        .map_err(|_| "Feedback is unavailable.")?
        .update_draft(text, diagnostics)
}

#[tauri::command]
fn feedback_action(kind: String, state: State<'_, HostState>) -> Result<(), String> {
    let mut feedback = state
        .feedback
        .lock()
        .map_err(|_| "Feedback is unavailable.")?;
    match kind.as_str() {
        "record" => {
            let settings = state.store.load().map_err(|error| error.to_string())?;
            feedback.start_recording(&settings, &state.store);
            Ok(())
        }
        "stop" => {
            feedback.stop_recording();
            Ok(())
        }
        "send" => {
            let settings = state.store.load().map_err(|error| error.to_string())?;
            feedback.send_report(&settings)
        }
        "close" => {
            feedback.close_window();
            Ok(())
        }
        _ => Err("Unknown feedback action.".into()),
    }
}

#[tauri::command]
async fn host_open_feedback(app: tauri::AppHandle, state: State<'_, HostState>) -> Result<(), String> {
    present_feedback(&app, &state, false)
}

pub struct TauriShell {
    controls: Sender<ProductShellControl>,
    events: Receiver<ProductShellEvent>,
    thread: Option<JoinHandle<()>>,
}

impl TauriShell {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        store: SettingsStore,
        database_path: PathBuf,
        initial_route: UiRoute,
        initial_status: UiRuntimeStatus,
        initial_vosk_probe: UiVoskProbe,
        initially_visible: bool,
        loaded_whisper: Option<WhisperReadiness>,
    ) -> Result<Self, String> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-tauri-shell".into())
            .spawn(move || {
                let result = run_shell(
                    store,
                    database_path,
                    initial_route,
                    initial_status,
                    initial_vosk_probe,
                    loaded_whisper,
                    initially_visible,
                    control_rx,
                    event_tx.clone(),
                    ready_tx.clone(),
                );
                if let Err(message) = result {
                    let _ = ready_tx.send(Err(message.clone()));
                    let _ = event_tx.send(ProductShellEvent::Failed(message));
                }
            })
            .map_err(|error| error.to_string())?;
        ready_rx
            .recv()
            .map_err(|_| "The Tauri shell exited during startup.")??;
        Ok(Self {
            controls: control_tx,
            events: event_rx,
            thread: Some(thread),
        })
    }

    pub fn send(&self, control: ProductShellControl) -> Result<(), String> {
        self.controls
            .send(control)
            .map_err(|_| "The Tauri shell is unavailable.".into())
    }

    pub fn focus(&self, route: UiRoute) -> Result<(), String> {
        self.send(ProductShellControl::Focus(route))
    }

    pub fn events(&self) -> &Receiver<ProductShellEvent> {
        &self.events
    }

    pub fn shutdown(mut self) -> Result<(), String> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), String> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let _ = self.controls.send(ProductShellControl::Quit);
        thread
            .join()
            .map_err(|_| "The Tauri shell panicked.".into())
    }
}

impl Drop for TauriShell {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run_shell(
    store: SettingsStore,
    database_path: PathBuf,
    initial_route: UiRoute,
    initial_status: UiRuntimeStatus,
    initial_vosk_probe: UiVoskProbe,
    loaded_whisper: Option<WhisperReadiness>,
    initially_visible: bool,
    controls: Receiver<ProductShellControl>,
    events: Sender<ProductShellEvent>,
    ready: SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let bridge =
        UiBridge::open(store.clone(), &database_path).map_err(|error| error.to_string())?;
    // Starts the engine only; Tor stays off until the user enables messaging.
    let messaging = MessagingHost::start(&store, &database_path);
    let library = LibrarySearchWorker::spawn(database_path).ok();
    let initial_readiness = UiReadinessSnapshot::checking(bridge.settings(), &store);
    let setup = SetupCenter::open(store.clone(), Arc::new(BenchmarkUnavailable)).ok();
    let updates = UpdateCenter::new(store.resolve_asset_path(std::path::Path::new("updates")));
    let bootstrap_allowed = initial_route == UiRoute::Setup && initial_status == UiRuntimeStatus::NeedsAttention;
    let bootstrap = BootstrapCenter::new(
        store.resolve_asset_path(std::path::Path::new("setup-status.json")), bootstrap_allowed);
    let meeting = MeetingTauriState::start(store.clone())?;
    let host = HostState {
        bridge: Mutex::new(bridge),
        store,
        readiness: Mutex::new(initial_readiness),
        setup: Mutex::new(setup),
        updates: Mutex::new(updates),
        bootstrap: Mutex::new(bootstrap),
        bootstrap_allowed,
        feedback: Mutex::new(FeedbackPanel::default()),
        library: Mutex::new(library),
        library_generation: AtomicU64::new(0),
        route: Mutex::new(initial_route),
        status: Mutex::new(initial_status),
        notice: Mutex::new(String::new()),
        events: events.clone(),
    };
    let stopping = Arc::new(AtomicBool::new(false));
    let pump = Arc::new(Mutex::new(None::<JoinHandle<()>>));
    let pump_for_setup = pump.clone();
    let stopping_for_setup = stopping.clone();
    let events_for_close = events.clone();
    let result = tauri::Builder::default()
        .any_thread()
        .manage(meeting)
        .manage(host)
        .manage(messaging)
        .invoke_handler(tauri::generate_handler![
            crate::tauri_meetings::meeting_snapshot,
            crate::tauri_meetings::meeting_actions,
            crate::tauri_meetings::meeting_save_action,
            crate::tauri_meetings::meeting_delete_action,
            crate::tauri_meetings::meeting_provider,
            crate::tauri_meetings::meeting_save_provider,
            crate::tauri_meetings::meeting_action,
            crate::tauri_meetings::meeting_layout,
            crate::tauri_meetings::meeting_save_translation,
            crate::tauri_meetings::meeting_save_devices,
            crate::tauri_meetings::meeting_theme,
            host_snapshot,
            host_app_snapshot,
            host_input_devices,
            host_history_text,
            host_library_search,
            host_library_update,
            host_select_route,
            host_save_preferences,
            host_custom_instructions,
            host_save_dictation_settings,
            host_save_privacy_settings,
            host_save_lexicon,
            host_delete_lexicon,
            host_profile_detail,
            host_save_profile,
            host_delete_profile,
            host_setup_snapshot,
            host_start_setup_action,
            host_cancel_setup_action,
            host_bootstrap_snapshot,
            host_start_bootstrap,
            host_cancel_bootstrap,
            host_update_snapshot,
            host_update_action,
            host_action,
            host_select_whisper_model,
            host_install_vosk,
            feedback_snapshot,
            feedback_update,
            feedback_action,
            host_open_feedback,
            crate::tauri_messaging::host_messaging_snapshot,
            crate::tauri_messaging::host_messaging_enable,
            crate::tauri_messaging::host_messaging_disable,
            crate::tauri_messaging::host_messaging_set_name,
            crate::tauri_messaging::host_messaging_invite,
            crate::tauri_messaging::host_messaging_redeem,
            crate::tauri_messaging::host_messaging_conversation,
            crate::tauri_messaging::host_messaging_send,
            crate::tauri_messaging::host_messaging_focus,
            crate::tauri_messaging::host_messaging_contact,
            crate::tauri_messaging::host_messaging_launcher_key,
            crate::tauri_messaging::host_messaging_dictate,
            crate::tauri_messaging::host_messaging_draft,
            crate::tauri_messaging::host_messaging_draft_action,
        ])
        .setup(move |app| {
            if !initially_visible {
                if let Some(window) = app.get_webview_window("main") {
                    window.hide()?;
                }
            }
            let handle = app.handle().clone();
            app.state::<MessagingHost>()
                .spawn_draft_listener(handle.clone());
            spawn_readiness_probe(handle.clone(), loaded_whisper, initial_vosk_probe)
                .map_err(|error| tauri::Error::Anyhow(anyhow::anyhow!(error)))?;
            let stopping = stopping_for_setup.clone();
            let pump_thread = thread::Builder::new()
                .name("phorminx-tauri-controls".into())
                .spawn(move || control_loop(handle, controls, stopping))
                .map_err(|error| tauri::Error::Anyhow(error.into()))?;
            *pump_for_setup
                .lock()
                .map_err(|_| tauri::Error::Anyhow(anyhow::anyhow!("control slot unavailable")))? =
                Some(pump_thread);
            let _ = ready.send(Ok(()));
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "feedback" {
                    // The feedback X is an actual window close. Keeping a
                    // hidden WebView here can leave a failed/blank renderer
                    // around forever and make the next open reuse it.
                    if let Ok(mut feedback) =
                        window.app_handle().state::<HostState>().feedback.lock()
                    {
                        feedback.close_window();
                    }
                } else {
                    api.prevent_close();
                    let _ = window.hide();
                    if window.label() == "main" {
                        let _ = events_for_close.send(ProductShellEvent::Hidden);
                    }
                }
            }
        })
        .run(tauri::generate_context!());
    stopping.store(true, Ordering::Release);
    if let Some(pump) = pump.lock().map_err(|_| "Control slot unavailable.")?.take() {
        let _ = pump.join();
    }
    result.map_err(|error| error.to_string())
}

fn spawn_readiness_probe(
    app: tauri::AppHandle,
    loaded_whisper: Option<WhisperReadiness>,
    vosk_probe: UiVoskProbe,
) -> Result<(), String> {
    thread::Builder::new()
        .name("phorminx-tauri-readiness".into())
        .spawn(move || {
            let host = app.state::<HostState>();
            let settings = match host.store.load() {
                Ok(settings) => settings,
                Err(_) => return,
            };
            let Ok(ollama) = OllamaClient::new(
                OllamaEndpoint::default(),
                ClientTimeouts {
                    connect: Duration::from_millis(500),
                    response_headers: Duration::from_secs(2),
                    response_body: Duration::from_secs(2),
                    overall: Duration::from_secs(3),
                },
            ) else {
                return;
            };
            let snapshot = UiReadinessSnapshot::probe(
                &settings,
                &host.store,
                &ollama,
                loaded_whisper.as_ref(),
                vosk_probe,
            );
            if let Ok(mut readiness) = host.readiness.lock() {
                *readiness = snapshot.clone();
            }
            if let Ok(mut setup) = host.setup.lock()
                && let Some(center) = setup.as_mut()
            {
                let _ = center.replan(&settings, &snapshot);
            }
            host.configure_library();
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn control_loop(
    app: tauri::AppHandle,
    controls: Receiver<ProductShellControl>,
    stopping: Arc<AtomicBool>,
) {
    let mut next_update_tick = std::time::Instant::now() + Duration::from_secs(10);
    while !stopping.load(Ordering::Acquire) {
        if std::time::Instant::now() >= next_update_tick {
            let host = app.state::<HostState>();
            let idle = update_idle(&app);
            if let Ok(mut updates) = host.updates.lock() {
                updates.poll(idle);
                let window_visible = app.webview_windows().values().any(|window| window.is_visible().unwrap_or(false));
                if idle
                    && !window_visible
                    && phorminx_windows::user_idle_duration().is_some_and(|away| updates.idle_restart_due(away))
                    && updates.restart_to_update(idle, true).is_ok()
                {
                    let _ = host.events.send(ProductShellEvent::QuitRequested);
                }
            }
            next_update_tick = std::time::Instant::now() + Duration::from_secs(5);
        }
        let control = match controls.recv_timeout(Duration::from_millis(200)) {
            Ok(control) => control,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let host = app.state::<HostState>();
        match control {
            ProductShellControl::Feedback => {
                if let Err(error) = present_feedback(&app, &host, true) {
                    host.notice(&error);
                }
            }
            ProductShellControl::FocusActions => focus_route(&app, &host, UiRoute::Actions),
            ProductShellControl::StartMeeting => {
                let meeting = app.state::<MeetingTauriState>();
                match meeting.send(WorkspaceEvent::StartMeeting) {
                    Ok(()) => {
                        if let Err(error) = open_companion(&app) {
                            host.notice(&error);
                        }
                    }
                    Err(_) => host.notice("Meeting could not start."),
                }
            }
            ProductShellControl::TriggerAction(action) => {
                let meeting = app.state::<MeetingTauriState>();
                if meeting
                    .send(WorkspaceEvent::TriggerAction(action.id))
                    .is_err()
                {
                    host.notice("Action could not start.");
                }
            }
            ProductShellControl::Focus(route) => focus_route(&app, &host, route),
            ProductShellControl::SetRuntimeStatus(status) => {
                if let Ok(mut current) = host.status.lock() {
                    *current = status;
                }
                if let Ok(library) = host.library.lock()
                    && let Some(worker) = library.as_ref()
                {
                    worker.handle().set_paused(matches!(
                        status,
                        UiRuntimeStatus::Listening
                            | UiRuntimeStatus::Transcribing
                            | UiRuntimeStatus::Refining
                    ));
                }
            }
            ProductShellControl::Refresh => {
                let _ = app
                    .state::<MeetingTauriState>()
                    .send(WorkspaceEvent::Refresh);
            }
            ProductShellControl::ModelDownloadProgress(percent) => {
                host.notice(&format!("Model download: {percent}%"))
            }
            ProductShellControl::ModelDownloaded { path, variant } => {
                let candidate = host.bridge.lock().ok().map(|bridge| {
                    let mut candidate = bridge.settings().clone();
                    candidate.recognition.model_path = path;
                    candidate.recognition.accurate_model = variant;
                    candidate.startup.onboarding_complete = true;
                    candidate
                });
                if let Some(candidate) = candidate {
                    match host.execute(UiCommand::SaveSettings(candidate)) {
                        Ok(()) => host.notice("Model downloaded, verified and selected."),
                        Err(error) => host.notice(&error),
                    }
                } else {
                    host.notice("Model downloaded, but settings are unavailable.");
                }
            }
            ProductShellControl::ModelDownloadFailed => host.notice("Model download failed."),
            ProductShellControl::VoskInstallFailed(_) => host.notice("Vosk installation failed."),
            ProductShellControl::ActivationBlocked(_) => {
                host.notice("That action is unavailable right now.")
            }
            ProductShellControl::OpenMessages => {
                app.state::<MessagingHost>().request_page();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            ProductShellControl::MessageContact { contact_id } => {
                if let Err(error) = app.state::<MessagingHost>().dictate(&app, contact_id) {
                    host.notice(&error);
                }
            }
            ProductShellControl::Quit => {
                app.exit(0);
                break;
            }
        }
    }
}

fn focus_route(app: &tauri::AppHandle, host: &HostState, route: UiRoute) {
    if let Ok(mut current) = host.route.lock() {
        *current = route;
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn open_feedback(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("feedback") {
        window.show().map_err(|_| "Feedback could not be shown.")?;
        window
            .set_focus()
            .map_err(|_| "Feedback could not be focused.")?;
    } else {
        WebviewWindowBuilder::new(
            app,
            "feedback",
            WebviewUrl::App("tauri-preview/feedback.html".into()),
        )
        .title("Phorminx · Feedback")
        .inner_size(600.0, 570.0)
        .min_inner_size(420.0, 400.0)
        .resizable(true)
        .build()
        .map_err(|_| "Feedback could not be opened.")?;
    }
    Ok(())
}

fn present_feedback(
    app: &tauri::AppHandle,
    host: &HostState,
    auto_record: bool,
) -> Result<(), String> {
    let settings = host.store.load().map_err(|error| error.to_string())?;
    host.feedback
        .lock()
        .map_err(|_| "Feedback is unavailable.")?
        .open(&settings, &host.store, auto_record);
    open_feedback(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_bridge::UiMutation;

    #[test]
    fn startup_registration_is_dispatched_before_runtime_restart() {
        let (send, receive) = mpsc::channel();
        dispatch_settings_outcome(
            &send,
            UiCommandOutcome {
                mutation: UiMutation::SettingsSaved,
                effects: vec![UiEffect::ReloadRuntime, UiEffect::ApplyLaunchAtLogin(true)],
            },
        )
        .unwrap();
        assert_eq!(receive.try_recv().unwrap(), ProductShellEvent::ApplyLaunchAtLogin(true));
        assert_eq!(
            receive.try_recv().unwrap(),
            ProductShellEvent::RuntimeReloadRequested(UiMutation::SettingsSaved)
        );
    }
}

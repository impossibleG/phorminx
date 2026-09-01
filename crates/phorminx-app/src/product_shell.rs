//! Durable unified product shell hosted beside the latency-sensitive runtime loop.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use eframe::egui::{self, Vec2, ViewportCommand};
use phorminx_ollama::OllamaClient;
use phorminx_persistence::{CasePolicy, FormattingStyle, InsertionPreference};
use phorminx_ui::theme::ThemeMode;
use phorminx_ui::{
    AppearancePreference as ShellAppearance, ApplicationProfile,
    FormattingStrength as ShellFormatting, HistoryItem, InlineNotice, LexiconCasePolicy,
    LexiconEntry, ModelSystem, NoticeKind, OllamaLifecycle as ShellLifecycle, PhorminxUi,
    ProfileInsertion, Readiness, RecordingMode as ShellRecording, Route, RuntimeStatus,
    SettingsSnapshot, ShellEvent, ShellSnapshot, SystemReadiness,
};
use phorminx_windows::{SystemAppearance, system_appearance};

use crate::settings::{
    AppearancePreference as StoredAppearance, FormattingStrength, HistoryRetention,
    OllamaLifecycle, RecordingMode, Settings, SettingsStore,
};
use crate::ui_bridge::{
    DEFAULT_HISTORY_LIMIT, UiBridge, UiCommand, UiEffect, UiLexiconDraft, UiMutation,
    UiProfileDraft, UiReadinessSnapshot, UiReadinessState, UiRoute, UiRuntimeStatus, UiSnapshot,
};

#[cfg(windows)]
use winit::platform::windows::EventLoopBuilderExtWindows;

#[derive(Clone, Debug)]
pub enum ProductShellControl {
    Focus(UiRoute),
    SetRuntimeStatus(UiRuntimeStatus),
    Refresh,
    ModelDownloadProgress(u64),
    ModelDownloaded(PathBuf),
    ModelDownloadFailed,
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductShellEvent {
    TestDictation,
    ChangeWhisperModel,
    CancelWhisperModelDownload,
    RuntimeReloadRequested(UiMutation),
    ApplyLaunchAtLogin(bool),
    Hidden,
    Failed(String),
}

pub struct ProductShell {
    controls: Sender<ProductShellControl>,
    events: Receiver<ProductShellEvent>,
    context: egui::Context,
    thread: Option<JoinHandle<()>>,
}

impl ProductShell {
    pub fn start(
        store: SettingsStore,
        database_path: PathBuf,
        initial_route: UiRoute,
        initial_status: UiRuntimeStatus,
        initially_visible: bool,
    ) -> Result<Self, String> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-product-shell".to_owned())
            .spawn(move || {
                let result = run_shell(
                    store,
                    database_path,
                    initial_route,
                    initial_status,
                    initially_visible,
                    ShellChannels {
                        controls: control_rx,
                        events: event_tx.clone(),
                        ready: ready_tx,
                    },
                );
                if let Err(message) = result {
                    let _ = event_tx.send(ProductShellEvent::Failed(message));
                }
            })
            .map_err(|error| error.to_string())?;
        let context = ready_rx
            .recv()
            .map_err(|_| "the product shell exited during startup".to_owned())??;
        Ok(Self {
            controls: control_tx,
            events: event_rx,
            context,
            thread: Some(thread),
        })
    }

    pub fn send(&self, control: ProductShellControl) -> Result<(), String> {
        self.controls
            .send(control)
            .map_err(|_| "the product shell is unavailable".to_owned())?;
        self.context.request_repaint();
        Ok(())
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
        self.context.request_repaint();
        thread
            .join()
            .map_err(|_| "the product shell panicked".to_owned())
    }
}

impl Drop for ProductShell {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run_shell(
    store: SettingsStore,
    database_path: PathBuf,
    initial_route: UiRoute,
    initial_status: UiRuntimeStatus,
    initially_visible: bool,
    channels: ShellChannels,
) -> Result<(), String> {
    let ShellChannels {
        controls,
        events,
        ready,
    } = channels;
    let bridge = UiBridge::open(store.clone(), database_path).map_err(|error| error.to_string())?;
    let readiness = UiReadinessSnapshot::checking(bridge.settings(), &store);
    let snapshot = bridge
        .snapshot(initial_status, readiness.clone(), DEFAULT_HISTORY_LIMIT)
        .map_err(|error| error.to_string())?;
    let (readiness_tx, readiness_rx) = mpsc::channel();
    probe_readiness(bridge.settings().clone(), store.clone(), readiness_tx);

    let mut native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Phorminx")
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!(
                    "../../../design/brand/png/app/phorminx-app-256.png"
                ))
                .map_err(|error| format!("failed to decode the embedded window icon: {error}"))?,
            )
            .with_visible(initially_visible)
            .with_inner_size(Vec2::new(1120.0, 760.0))
            .with_min_inner_size(Vec2::new(900.0, 620.0))
            .with_resizable(true),
        centered: true,
        ..Default::default()
    };
    #[cfg(windows)]
    {
        native_options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_any_thread(true);
        }));
    }
    let app = ProductShellApp::new(
        bridge,
        readiness,
        snapshot,
        initial_route,
        initial_status,
        controls,
        events,
        readiness_rx,
        store,
    );
    eframe::run_native(
        "Phorminx",
        native_options,
        Box::new(move |creation| {
            let _ = ready.send(Ok(creation.egui_ctx.clone()));
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| error.to_string())
}

struct ShellChannels {
    controls: Receiver<ProductShellControl>,
    events: Sender<ProductShellEvent>,
    ready: mpsc::SyncSender<Result<egui::Context, String>>,
}

fn probe_readiness(settings: Settings, store: SettingsStore, sender: Sender<UiReadinessSnapshot>) {
    let _ = thread::Builder::new()
        .name("phorminx-readiness".to_owned())
        .spawn(move || {
            let readiness = UiReadinessSnapshot::probe(&settings, &store, &OllamaClient::default());
            let _ = sender.send(readiness);
        });
}

struct ProductShellApp {
    shell: PhorminxUi,
    bridge: UiBridge,
    readiness: UiReadinessSnapshot,
    route: UiRoute,
    runtime_status: UiRuntimeStatus,
    controls: Receiver<ProductShellControl>,
    events: Sender<ProductShellEvent>,
    readiness_rx: Receiver<UiReadinessSnapshot>,
    store: SettingsStore,
    notice: Option<InlineNotice>,
    download_active: bool,
    quitting: bool,
}

impl ProductShellApp {
    #[allow(clippy::too_many_arguments)]
    fn new(
        bridge: UiBridge,
        readiness: UiReadinessSnapshot,
        snapshot: UiSnapshot,
        route: UiRoute,
        runtime_status: UiRuntimeStatus,
        controls: Receiver<ProductShellControl>,
        events: Sender<ProductShellEvent>,
        readiness_rx: Receiver<UiReadinessSnapshot>,
        store: SettingsStore,
    ) -> Self {
        Self {
            shell: PhorminxUi::new(map_snapshot(snapshot, route, None)),
            bridge,
            readiness,
            route,
            runtime_status,
            controls,
            events,
            readiness_rx,
            store,
            notice: None,
            download_active: false,
            quitting: false,
        }
    }

    fn refresh(&mut self) {
        match self.bridge.snapshot(
            self.runtime_status,
            self.readiness.clone(),
            DEFAULT_HISTORY_LIMIT,
        ) {
            Ok(snapshot) => {
                self.shell
                    .apply_snapshot(map_snapshot(snapshot, self.route, self.notice.clone()))
            }
            Err(error) => self.set_error(error.to_string()),
        }
    }

    fn set_error(&mut self, message: String) {
        self.notice = Some(InlineNotice {
            kind: NoticeKind::Error,
            title: "The instrument needs attention".to_owned(),
            detail: message,
            action: None,
        });
    }

    fn execute(&mut self, command: UiCommand) -> bool {
        match self.bridge.execute(command, &self.readiness, now_ms()) {
            Ok(outcome) => {
                self.notice = None;
                for effect in outcome.effects.into_iter().rev() {
                    let event = match effect {
                        UiEffect::ReloadRuntime => {
                            ProductShellEvent::RuntimeReloadRequested(outcome.mutation.clone())
                        }
                        UiEffect::ApplyLaunchAtLogin(enabled) => {
                            ProductShellEvent::ApplyLaunchAtLogin(enabled)
                        }
                    };
                    let _ = self.events.send(event);
                }
                if matches!(outcome.mutation, UiMutation::SettingsSaved) {
                    self.readiness =
                        UiReadinessSnapshot::checking(self.bridge.settings(), &self.store);
                    let (sender, receiver) = mpsc::channel();
                    self.readiness_rx = receiver;
                    probe_readiness(self.bridge.settings().clone(), self.store.clone(), sender);
                }
                self.refresh();
                true
            }
            Err(error) => {
                self.set_error(error.to_string());
                false
            }
        }
    }

    fn handle_shell_event(&mut self, event: ShellEvent, ctx: &egui::Context) {
        match event {
            ShellEvent::Navigate(route) => self.route = unmap_route(route),
            ShellEvent::TestDictation => {
                let _ = self.events.send(ProductShellEvent::TestDictation);
            }
            ShellEvent::CopyHistory { id, variant } => {
                let text = self
                    .shell
                    .snapshot()
                    .history
                    .iter()
                    .find(|item| item.id == id)
                    .and_then(|item| item.text_for(variant))
                    .map(str::to_owned);
                if let Some(text) = text {
                    ctx.copy_text(text);
                    self.notice = Some(InlineNotice {
                        kind: NoticeKind::Information,
                        title: "Copied.".to_owned(),
                        detail: "The selected transcript text is on the clipboard.".to_owned(),
                        action: None,
                    });
                    self.refresh();
                }
            }
            ShellEvent::ClearHistory => {
                self.execute(UiCommand::ClearHistory);
            }
            ShellEvent::SaveLexicon(draft) => {
                if self.execute(UiCommand::SaveLexicon(UiLexiconDraft {
                    id: draft.id,
                    canonical: draft.written,
                    alias: draft.spoken,
                    language: optional(draft.language),
                    app_executable: optional(draft.scope),
                    case_policy: unmap_case_policy(draft.case_policy),
                    enabled: draft.enabled,
                })) {
                    self.shell.close_lexicon_editor();
                }
            }
            ShellEvent::DeleteLexicon(id) => {
                self.execute(UiCommand::DeleteLexicon(id));
            }
            ShellEvent::SaveProfile(draft) => {
                let original = draft.original_executable.clone();
                if self.execute(UiCommand::SaveProfile(UiProfileDraft {
                    original_executable: original,
                    executable: draft.executable,
                    formatting_style: map_profile_formatting(draft.formatting),
                    custom_instructions: optional(draft.custom_instruction),
                    language: optional(draft.language),
                    insertion_preference: map_profile_insertion(draft.insertion),
                    deny: draft.blocked,
                })) {
                    self.shell.close_profile_editor();
                }
            }
            ShellEvent::RemoveProfile(executable) => {
                self.execute(UiCommand::DeleteProfile(executable));
            }
            ShellEvent::VerifyModels => {
                self.readiness = UiReadinessSnapshot::checking(self.bridge.settings(), &self.store);
                let (sender, receiver) = mpsc::channel();
                self.readiness_rx = receiver;
                probe_readiness(self.bridge.settings().clone(), self.store.clone(), sender);
                self.refresh();
            }
            ShellEvent::NoticeAction if self.download_active => {
                let _ = self
                    .events
                    .send(ProductShellEvent::CancelWhisperModelDownload);
            }
            ShellEvent::NoticeAction => {
                self.readiness = UiReadinessSnapshot::checking(self.bridge.settings(), &self.store);
                let (sender, receiver) = mpsc::channel();
                self.readiness_rx = receiver;
                probe_readiness(self.bridge.settings().clone(), self.store.clone(), sender);
                self.refresh();
            }
            ShellEvent::ChangeWhisperModel => {
                let _ = self.events.send(ProductShellEvent::ChangeWhisperModel);
            }
            ShellEvent::SelectOllamaModel(model) => {
                let mut settings = self.bridge.settings().clone();
                settings.formatting.ollama_model = Some(model);
                self.execute(UiCommand::SaveSettings(settings));
            }
            ShellEvent::SaveSettings(form) => {
                let settings = apply_settings_snapshot(self.bridge.settings(), &form);
                self.execute(UiCommand::SaveSettings(settings));
            }
            ShellEvent::DismissNotice => {
                self.notice = None;
                self.refresh();
            }
            ShellEvent::SelectHistory(id) => self.shell.select_history(id),
            ShellEvent::SelectHistoryVariant(_)
            | ShellEvent::NewLexiconEntry
            | ShellEvent::EditLexicon(_)
            | ShellEvent::NewProfile
            | ShellEvent::EditProfile(_) => {}
            ShellEvent::CancelLexiconEdit => self.shell.close_lexicon_editor(),
            ShellEvent::CancelProfileEdit => self.shell.close_profile_editor(),
        }
    }
}

fn resolve_theme(
    preference: StoredAppearance,
    system_theme: Option<egui::Theme>,
    appearance: SystemAppearance,
) -> ThemeMode {
    let prefers_dark = if appearance.high_contrast {
        appearance.contrast_theme_is_dark
    } else {
        match preference {
            StoredAppearance::System => !matches!(system_theme, Some(egui::Theme::Light)),
            StoredAppearance::Light => false,
            StoredAppearance::Dark => true,
        }
    };
    ThemeMode::from_system(prefers_dark, appearance.high_contrast)
}

impl eframe::App for ProductShellApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.shell.set_theme(resolve_theme(
            self.bridge.settings().appearance.theme,
            ctx.system_theme(),
            system_appearance(),
        ));
        while let Ok(control) = self.controls.try_recv() {
            match control {
                ProductShellControl::Focus(route) => {
                    self.route = route;
                    self.shell.request_route_focus();
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.refresh();
                }
                ProductShellControl::SetRuntimeStatus(status) => {
                    self.runtime_status = status;
                    self.refresh();
                }
                ProductShellControl::Refresh => self.refresh(),
                ProductShellControl::ModelDownloadProgress(percent) => {
                    self.download_active = true;
                    self.notice = Some(InlineNotice {
                        kind: NoticeKind::Information,
                        title: "Acquiring the local instrument".to_owned(),
                        detail: format!("Downloading and verifying the pinned model · {percent}%"),
                        action: Some("Cancel download".to_owned()),
                    });
                    self.refresh();
                }
                ProductShellControl::ModelDownloaded(path) => {
                    self.download_active = false;
                    let mut settings = self.bridge.settings().clone();
                    settings.recognition.model_path = path;
                    settings.startup.onboarding_complete = true;
                    self.execute(UiCommand::SaveSettings(settings));
                }
                ProductShellControl::ModelDownloadFailed => {
                    self.download_active = false;
                    self.set_error("The verified model could not be downloaded. Your previous model was not changed.".to_owned());
                    self.refresh();
                }
                ProductShellControl::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        match self.readiness_rx.try_recv() {
            Ok(readiness) => {
                self.readiness = readiness;
                self.refresh();
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        if ctx.input(|input| input.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            let _ = self.events.send(ProductShellEvent::Hidden);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.show(ui);
        for event in self.shell.take_events() {
            self.handle_shell_event(event, ui.ctx());
        }
    }
}

fn map_snapshot(
    snapshot: UiSnapshot,
    route: UiRoute,
    notice: Option<InlineNotice>,
) -> ShellSnapshot {
    let settings = &snapshot.settings.values;
    let history = snapshot
        .history
        .into_iter()
        .map(|item| HistoryItem {
            id: item.id,
            time: format_timestamp(item.created_at_ms),
            application: item
                .target_executable
                .unwrap_or_else(|| "Unknown application".to_owned()),
            language: item.language.unwrap_or_else(|| "Automatic".to_owned()),
            output: item.selected_output,
            raw: Some(item.raw_text),
            normalized: item.normalized_text,
            cleaned: item.cleaned_text,
            latency: format_duration(
                item.stt_duration_ms,
                item.formatting_duration_ms,
                item.insertion_duration_ms,
            ),
            warning: item.warnings.first().cloned(),
        })
        .collect();
    let lexicon = snapshot
        .lexicon
        .into_iter()
        .map(|item| LexiconEntry {
            id: item.id,
            spoken: item.alias,
            written: item.canonical,
            language: item.language.unwrap_or_else(|| "Every language".to_owned()),
            scope: item
                .app_executable
                .unwrap_or_else(|| "Everywhere".to_owned()),
            case_policy: map_case_policy(item.case_policy),
            enabled: item.enabled,
        })
        .collect();
    let profiles = snapshot
        .profiles
        .into_iter()
        .map(|item| ApplicationProfile {
            executable: item.executable,
            formatting: formatting_style_label(item.formatting_style).to_owned(),
            language: item
                .language
                .unwrap_or_else(|| "Default language".to_owned()),
            insertion: insertion_label(item.insertion_preference).to_owned(),
            blocked: item.deny,
            custom_instruction: item.custom_instructions.unwrap_or_default(),
        })
        .collect();
    let microphone = &snapshot.readiness.microphone;
    let whisper = &snapshot.readiness.whisper;
    let ollama = &snapshot.readiness.ollama;
    let whisper_name = snapshot
        .settings
        .resolved_model_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Not selected")
        .to_owned();
    ShellSnapshot {
        route: map_route(route),
        status: map_runtime_status(snapshot.runtime_status),
        shortcut: "Ctrl  Alt  Space".to_owned(),
        systems: vec![
            SystemReadiness::new(
                "Microphone",
                microphone.message.clone(),
                map_readiness(microphone.state),
            ),
            SystemReadiness::new(
                "Whisper",
                whisper.message.clone(),
                map_readiness(whisper.state),
            ),
            SystemReadiness::new(
                "Ollama",
                ollama.message.clone(),
                map_readiness(ollama.state),
            ),
        ],
        history_enabled: snapshot.history_enabled,
        history,
        lexicon,
        profiles,
        whisper: ModelSystem {
            name: "Whisper".to_owned(),
            selected: Some(whisper_name),
            detail: whisper.message.clone(),
            state: map_readiness(whisper.state),
            installed: Vec::new(),
        },
        ollama: ModelSystem {
            name: "Ollama".to_owned(),
            selected: ollama.selected.clone(),
            detail: ollama.message.clone(),
            state: map_readiness(ollama.state),
            installed: ollama
                .models
                .iter()
                .map(|model| model.name.clone())
                .collect(),
        },
        settings: map_settings(settings, microphone),
        notice,
    }
}

fn map_settings(
    settings: &Settings,
    readiness: &crate::ui_bridge::UiMicrophoneReadiness,
) -> SettingsSnapshot {
    let mut microphones = vec!["Windows default".to_owned()];
    microphones.extend(readiness.devices.iter().map(|device| device.name.clone()));
    SettingsSnapshot {
        appearance: match settings.appearance.theme {
            StoredAppearance::System => ShellAppearance::System,
            StoredAppearance::Light => ShellAppearance::Light,
            StoredAppearance::Dark => ShellAppearance::Dark,
        },
        microphone: settings
            .recognition
            .microphone
            .clone()
            .unwrap_or_else(|| "Windows default".to_owned()),
        microphones,
        recording_mode: match settings.interaction.recording_mode {
            RecordingMode::Hold => ShellRecording::Hold,
            RecordingMode::Toggle => ShellRecording::Toggle,
        },
        language: match settings.recognition.language.as_str() {
            "en" => "English".to_owned(),
            "pt-br" => "Português (Brasil)".to_owned(),
            other => other.to_owned(),
        },
        formatting: map_formatting(settings.formatting.strength),
        custom_instruction: settings
            .formatting
            .custom_instructions
            .clone()
            .unwrap_or_default(),
        minimum_rms: settings.recognition.minimum_rms.to_string(),
        ollama_lifecycle: match settings.formatting.ollama_lifecycle {
            OllamaLifecycle::Instant => ShellLifecycle::Instant,
            OllamaLifecycle::Balanced => ShellLifecycle::Balanced,
            OllamaLifecycle::MemorySaver => ShellLifecycle::MemorySaver,
        },
        model_path: settings.recognition.model_path.display().to_string(),
        history_retention: history_label(settings.privacy.history_retention).to_owned(),
        launch_at_login: settings.startup.launch_at_login,
    }
}

fn apply_settings_snapshot(current: &Settings, form: &SettingsSnapshot) -> Settings {
    let mut settings = current.clone();
    settings.appearance.theme = match form.appearance {
        ShellAppearance::System => StoredAppearance::System,
        ShellAppearance::Light => StoredAppearance::Light,
        ShellAppearance::Dark => StoredAppearance::Dark,
    };
    settings.recognition.microphone =
        (form.microphone != "Windows default").then(|| form.microphone.clone());
    settings.recognition.language = match form.language.as_str() {
        "English" => "en",
        "Português (Brasil)" => "pt-br",
        other => other,
    }
    .to_owned();
    settings.interaction.recording_mode = match form.recording_mode {
        ShellRecording::Hold => RecordingMode::Hold,
        ShellRecording::Toggle => RecordingMode::Toggle,
    };
    settings.formatting.strength = unmap_formatting(form.formatting);
    settings.formatting.custom_instructions =
        (form.formatting == ShellFormatting::Custom).then(|| form.custom_instruction.clone());
    settings.recognition.minimum_rms = form.minimum_rms.trim().parse().unwrap_or(f32::NAN);
    settings.formatting.ollama_lifecycle = match form.ollama_lifecycle {
        ShellLifecycle::Instant => OllamaLifecycle::Instant,
        ShellLifecycle::Balanced => OllamaLifecycle::Balanced,
        ShellLifecycle::MemorySaver => OllamaLifecycle::MemorySaver,
    };
    settings.recognition.model_path = PathBuf::from(form.model_path.trim());
    settings.privacy.history_retention = match form.history_retention.as_str() {
        "Off" => HistoryRetention::Disabled,
        "1 day" => HistoryRetention::OneDay,
        "30 days" => HistoryRetention::ThirtyDays,
        "Indefinitely" => HistoryRetention::Indefinite,
        _ => HistoryRetention::SevenDays,
    };
    settings.startup.launch_at_login = form.launch_at_login;
    settings.startup.onboarding_complete = true;
    settings
}

fn optional(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}
fn map_route(route: UiRoute) -> Route {
    match route {
        UiRoute::Home => Route::Home,
        UiRoute::History => Route::History,
        UiRoute::Lexicon => Route::Lexicon,
        UiRoute::Profiles => Route::Profiles,
        UiRoute::Models => Route::Models,
        UiRoute::Settings => Route::Settings,
    }
}
fn unmap_route(route: Route) -> UiRoute {
    match route {
        Route::Home => UiRoute::Home,
        Route::History => UiRoute::History,
        Route::Lexicon => UiRoute::Lexicon,
        Route::Profiles => UiRoute::Profiles,
        Route::Models => UiRoute::Models,
        Route::Settings => UiRoute::Settings,
    }
}
fn map_runtime_status(status: UiRuntimeStatus) -> RuntimeStatus {
    match status {
        UiRuntimeStatus::Starting => RuntimeStatus::Transcribing,
        UiRuntimeStatus::Ready => RuntimeStatus::Ready,
        UiRuntimeStatus::Listening => RuntimeStatus::Listening,
        UiRuntimeStatus::Transcribing => RuntimeStatus::Transcribing,
        UiRuntimeStatus::Refining => RuntimeStatus::Refining,
        UiRuntimeStatus::Inserted => RuntimeStatus::Inserted,
        UiRuntimeStatus::Copied => RuntimeStatus::Copied,
        UiRuntimeStatus::NoSpeech | UiRuntimeStatus::NeedsAttention => {
            RuntimeStatus::NeedsAttention
        }
    }
}
fn map_readiness(state: UiReadinessState) -> Readiness {
    match state {
        UiReadinessState::Checking => Readiness::Working,
        UiReadinessState::Ready => Readiness::Ready,
        UiReadinessState::NeedsAttention => Readiness::Error,
    }
}
fn map_formatting(value: FormattingStrength) -> ShellFormatting {
    match value {
        FormattingStrength::Raw => ShellFormatting::Raw,
        FormattingStrength::Light => ShellFormatting::Light,
        FormattingStrength::Balanced => ShellFormatting::Balanced,
        FormattingStrength::Strong => ShellFormatting::Strong,
        FormattingStrength::Custom => ShellFormatting::Custom,
    }
}
fn unmap_formatting(value: ShellFormatting) -> FormattingStrength {
    match value {
        ShellFormatting::Raw => FormattingStrength::Raw,
        ShellFormatting::Light => FormattingStrength::Light,
        ShellFormatting::Balanced => FormattingStrength::Balanced,
        ShellFormatting::Strong => FormattingStrength::Strong,
        ShellFormatting::Custom => FormattingStrength::Custom,
    }
}
fn map_profile_formatting(value: ShellFormatting) -> FormattingStyle {
    match value {
        ShellFormatting::Raw => FormattingStyle::Raw,
        ShellFormatting::Light => FormattingStyle::Light,
        ShellFormatting::Balanced => FormattingStyle::Balanced,
        ShellFormatting::Strong => FormattingStyle::Strong,
        ShellFormatting::Custom => FormattingStyle::Custom,
    }
}
fn map_case_policy(value: CasePolicy) -> LexiconCasePolicy {
    match value {
        CasePolicy::PreserveInput => LexiconCasePolicy::PreserveInput,
        CasePolicy::UseCanonical => LexiconCasePolicy::UseCanonical,
        CasePolicy::Lowercase => LexiconCasePolicy::Lowercase,
        CasePolicy::Uppercase => LexiconCasePolicy::Uppercase,
    }
}
fn unmap_case_policy(value: LexiconCasePolicy) -> CasePolicy {
    match value {
        LexiconCasePolicy::PreserveInput => CasePolicy::PreserveInput,
        LexiconCasePolicy::UseCanonical => CasePolicy::UseCanonical,
        LexiconCasePolicy::Lowercase => CasePolicy::Lowercase,
        LexiconCasePolicy::Uppercase => CasePolicy::Uppercase,
    }
}
fn map_profile_insertion(value: ProfileInsertion) -> InsertionPreference {
    match value {
        ProfileInsertion::Automatic => InsertionPreference::Automatic,
        ProfileInsertion::Direct => InsertionPreference::Direct,
        ProfileInsertion::Clipboard => InsertionPreference::Clipboard,
    }
}
fn formatting_style_label(value: FormattingStyle) -> &'static str {
    match value {
        FormattingStyle::Raw => "Raw",
        FormattingStyle::Light => "Light",
        FormattingStyle::Balanced => "Balanced",
        FormattingStyle::Strong => "Strong",
        FormattingStyle::Custom => "Custom",
    }
}
fn insertion_label(value: InsertionPreference) -> &'static str {
    match value {
        InsertionPreference::Automatic => "Automatic insertion",
        InsertionPreference::Direct => "Direct insertion",
        InsertionPreference::Clipboard => "Clipboard only",
    }
}
fn history_label(value: HistoryRetention) -> &'static str {
    match value {
        HistoryRetention::Disabled => "Off",
        HistoryRetention::OneDay => "1 day",
        HistoryRetention::SevenDays => "7 days",
        HistoryRetention::ThirtyDays => "30 days",
        HistoryRetention::Indefinite => "Indefinitely",
    }
}
fn format_duration(stt: Option<u64>, formatting: Option<u64>, insertion: Option<u64>) -> String {
    format!(
        "{} ms",
        stt.unwrap_or(0)
            .saturating_add(formatting.unwrap_or(0))
            .saturating_add(insertion.unwrap_or(0))
    )
}
fn format_timestamp(created_at_ms: i64) -> String {
    let seconds = created_at_ms.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    format!("{year:04}-{month:02}-{day:02}  {hour:02}:{minute:02} UTC")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_mapping_is_bidirectional() {
        for route in [
            UiRoute::Home,
            UiRoute::History,
            UiRoute::Lexicon,
            UiRoute::Profiles,
            UiRoute::Models,
            UiRoute::Settings,
        ] {
            assert_eq!(unmap_route(map_route(route)), route);
        }
    }

    #[test]
    fn shell_settings_preserve_unedited_runtime_policy() {
        let mut settings = Settings::default();
        settings.recognition.minimum_rms = 0.123;
        settings.formatting.ollama_model = Some("local:test".to_owned());
        let form = SettingsSnapshot {
            language: "Português (Brasil)".to_owned(),
            minimum_rms: "0.123".to_owned(),
            ..SettingsSnapshot::default()
        };
        let mapped = apply_settings_snapshot(&settings, &form);
        assert_eq!(mapped.recognition.minimum_rms, 0.123);
        assert_eq!(
            mapped.formatting.ollama_model.as_deref(),
            Some("local:test")
        );
        assert_eq!(mapped.recognition.language, "pt-br");
    }

    #[test]
    fn explicit_appearance_overrides_system_but_not_high_contrast() {
        let normal = SystemAppearance {
            high_contrast: false,
            contrast_theme_is_dark: false,
        };
        assert_eq!(
            resolve_theme(StoredAppearance::Dark, Some(egui::Theme::Light), normal),
            ThemeMode::AuthoredDark
        );
        assert_eq!(
            resolve_theme(StoredAppearance::Light, Some(egui::Theme::Dark), normal),
            ThemeMode::AuthoredLight
        );
        let contrast = SystemAppearance {
            high_contrast: true,
            contrast_theme_is_dark: true,
        };
        assert_eq!(
            resolve_theme(StoredAppearance::Light, Some(egui::Theme::Light), contrast),
            ThemeMode::HighContrast
        );
    }

    #[test]
    fn appearance_round_trips_through_the_shell_form() {
        let mut settings = Settings::default();
        settings.appearance.theme = StoredAppearance::Dark;
        let form = map_settings(
            &settings,
            &crate::ui_bridge::UiMicrophoneReadiness {
                state: crate::ui_bridge::UiReadinessState::Ready,
                devices: Vec::new(),
                selected: None,
                message: String::new(),
            },
        );
        assert_eq!(form.appearance, ShellAppearance::Dark);
        assert_eq!(
            apply_settings_snapshot(&settings, &form).appearance.theme,
            StoredAppearance::Dark
        );
    }

    #[test]
    fn history_duration_is_saturating_and_content_free() {
        assert_eq!(format_duration(Some(100), Some(20), None), "120 ms");
    }

    #[test]
    fn history_timestamp_uses_stable_utc_civil_time() {
        assert_eq!(format_timestamp(0), "1970-01-01  00:00 UTC");
        assert_eq!(format_timestamp(1_704_164_645_000), "2024-01-02  03:04 UTC");
    }
}

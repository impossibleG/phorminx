//! Durable unified product shell hosted beside the latency-sensitive runtime loop.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use eframe::egui::{self, Vec2, ViewportCommand};
use phorminx_ollama::{ClientTimeouts, OllamaClient, OllamaEndpoint};
use phorminx_persistence::{CasePolicy, FormattingStyle, InsertionPreference};
use phorminx_ui::theme::ThemeMode;
use phorminx_ui::{
    AccurateBackend as ShellAccurateBackend, AccurateModel as ShellAccurateModel,
    AppearancePreference as ShellAppearance, ApplicationProfile,
    FormattingStrength as ShellFormatting, HistoryItem, HistoryVariant, HistoryVariantAvailability,
    InlineNotice, LexiconCasePolicy, LexiconEntry, ModelSystem, NoticeKind,
    OllamaLifecycle as ShellLifecycle, PhorminxUi, ProfileInsertion, Readiness,
    RecognitionMode as ShellRecognitionMode, RecordingMode as ShellRecording, Route, RuntimeStatus,
    SettingsSnapshot, SetupCapability, SetupSnapshot, SetupStage, ShellEvent, ShellSnapshot,
    SystemReadiness,
};
use phorminx_whisper::WhisperReadiness;
use phorminx_windows::{SystemAppearance, system_appearance};

use crate::history_loader::{HistoryLoadIntent, HistoryLoadKey, HistoryLoadResult, HistoryLoader};
use crate::settings::{
    AccurateBackendPreference, AccurateModelVariant, AppearancePreference as StoredAppearance,
    FormattingStrength, HistoryRetention, OllamaLifecycle, RecognitionMode, RecordingMode,
    Settings, SettingsStore,
};
use crate::setup_center::{BenchmarkUnavailable, SetupCenter};
use crate::setup_features::SetupFeatures;
use crate::ui_bridge::{
    DEFAULT_HISTORY_LIMIT, UiBridge, UiCommand, UiEffect, UiLexiconDraft, UiMutation,
    UiProfileDraft, UiReadinessSnapshot, UiReadinessState, UiRoute, UiRuntimeStatus, UiSnapshot,
    UiVoskProbe,
};

#[cfg(windows)]
use winit::platform::windows::EventLoopBuilderExtWindows;

#[derive(Clone, Debug)]
pub enum ProductShellControl {
    Focus(UiRoute),
    SetRuntimeStatus(UiRuntimeStatus),
    Refresh,
    ModelDownloadProgress(u64),
    ModelDownloaded {
        path: PathBuf,
        variant: AccurateModelVariant,
    },
    ModelDownloadFailed,
    VoskInstallFailed(String),
    ActivationBlocked(String),
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductShellEvent {
    TestDictation,
    ChangeWhisperModel(AccurateModelVariant),
    InstallVerifiedVoskAssets,
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
        initial_vosk_probe: UiVoskProbe,
        initially_visible: bool,
        loaded_whisper: Option<WhisperReadiness>,
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
                    initial_vosk_probe,
                    initially_visible,
                    loaded_whisper,
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

#[allow(clippy::too_many_arguments)]
fn run_shell(
    store: SettingsStore,
    database_path: PathBuf,
    initial_route: UiRoute,
    initial_status: UiRuntimeStatus,
    initial_vosk_probe: UiVoskProbe,
    initially_visible: bool,
    loaded_whisper: Option<WhisperReadiness>,
    channels: ShellChannels,
) -> Result<(), String> {
    let ShellChannels {
        controls,
        events,
        ready,
    } = channels;
    let bridge =
        UiBridge::open(store.clone(), database_path.clone()).map_err(|error| error.to_string())?;
    let history_loader = HistoryLoader::new(database_path.clone());
    // Setup is an optional child subsystem. Failure to recover it must never
    // make dictation or the unified shell fail to start.
    let setup_center =
        SetupCenter::open(store.clone(), std::sync::Arc::new(BenchmarkUnavailable)).ok();
    let setup_features = SetupFeatures::open(
        store.clone(),
        bridge.settings(),
        initial_route == UiRoute::Setup && initially_visible,
    );
    let readiness = UiReadinessSnapshot::checking(bridge.settings(), &store);
    let snapshot = bridge
        .snapshot(initial_status, readiness.clone(), DEFAULT_HISTORY_LIMIT)
        .map_err(|error| error.to_string())?;
    let (readiness_tx, readiness_rx) = mpsc::sync_channel(1);
    let readiness_started = probe_readiness(
        bridge.settings().clone(),
        store.clone(),
        loaded_whisper.clone(),
        initial_vosk_probe,
        readiness_tx,
    );

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
        loaded_whisper,
        history_loader,
        setup_center,
        setup_features,
        readiness_started,
        initially_visible,
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

fn probe_readiness(
    settings: Settings,
    store: SettingsStore,
    loaded_whisper: Option<WhisperReadiness>,
    vosk_probe: UiVoskProbe,
    sender: mpsc::SyncSender<UiReadinessSnapshot>,
) -> bool {
    thread::Builder::new()
        .name("phorminx-readiness".to_owned())
        .spawn(move || {
            let ollama = OllamaClient::new(
                OllamaEndpoint::default(),
                ClientTimeouts {
                    connect: Duration::from_millis(500),
                    response_headers: Duration::from_secs(2),
                    response_body: Duration::from_secs(2),
                    overall: Duration::from_secs(3),
                },
            )
            .expect("the fixed readiness timeout policy is valid");
            let readiness = UiReadinessSnapshot::probe(
                &settings,
                &store,
                &ollama,
                loaded_whisper.as_ref(),
                vosk_probe,
            );
            let _ = sender.send(readiness);
        })
        .is_ok()
}

const AUTOMATIC_REFRESH_VOSK_PROBE: UiVoskProbe = UiVoskProbe::LayoutOnly;

struct ProductShellApp {
    shell: PhorminxUi,
    bridge: UiBridge,
    readiness: UiReadinessSnapshot,
    route: UiRoute,
    runtime_status: UiRuntimeStatus,
    controls: Receiver<ProductShellControl>,
    events: Sender<ProductShellEvent>,
    readiness_rx: Receiver<UiReadinessSnapshot>,
    readiness_in_flight: bool,
    readiness_refresh_pending: bool,
    store: SettingsStore,
    loaded_whisper: Option<WhisperReadiness>,
    notice: Option<InlineNotice>,
    download_active: bool,
    quitting: bool,
    history_loader: HistoryLoader,
    setup_center: Option<SetupCenter>,
    setup_features: SetupFeatures,
    window_visible: bool,
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
        loaded_whisper: Option<WhisperReadiness>,
        history_loader: HistoryLoader,
        setup_center: Option<SetupCenter>,
        setup_features: SetupFeatures,
        readiness_started: bool,
        initially_visible: bool,
    ) -> Self {
        let mut setup = setup_center
            .as_ref()
            .map(|center| center.snapshot(&readiness))
            .unwrap_or_else(unavailable_setup_snapshot);
        setup_features.enrich(&mut setup);
        let mut app = Self {
            shell: PhorminxUi::new(map_snapshot_with_setup(snapshot, route, None, Some(setup))),
            bridge,
            readiness,
            route,
            runtime_status,
            controls,
            events,
            readiness_rx,
            readiness_in_flight: readiness_started,
            readiness_refresh_pending: false,
            store,
            loaded_whisper,
            notice: None,
            download_active: false,
            quitting: false,
            history_loader,
            setup_center,
            setup_features,
            window_visible: initially_visible,
        };
        if !readiness_started {
            app.set_error("Local readiness inspection could not start.".to_owned());
            app.refresh();
        }
        if route == UiRoute::History && initially_visible {
            app.ensure_history_detail();
        }
        app
    }

    fn refresh(&mut self) {
        match self.bridge.snapshot(
            self.runtime_status,
            self.readiness.clone(),
            DEFAULT_HISTORY_LIMIT,
        ) {
            Ok(snapshot) => {
                let mut setup = self
                    .setup_center
                    .as_ref()
                    .map(|center| center.snapshot(&self.readiness))
                    .unwrap_or_else(unavailable_setup_snapshot);
                self.setup_features.enrich(&mut setup);
                self.shell.apply_snapshot(map_snapshot_with_setup(
                    snapshot,
                    self.route,
                    self.notice.clone(),
                    Some(setup),
                ))
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

    fn setup_mutation_active(&self) -> bool {
        self.setup_center.as_ref().is_some_and(|center| {
            matches!(
                center.snapshot(&self.readiness).stage,
                SetupStage::AwaitingConsent | SetupStage::Working | SetupStage::Benchmarking
            )
        })
    }

    fn ensure_history_detail(&mut self) {
        if !self.window_visible {
            return;
        }
        let Some((id, variant)) = self.shell.history_selection() else {
            return;
        };
        if !self.shell.history_detail_loaded(id, variant) && !self.history_loader.has_current() {
            self.request_history_detail(id, variant);
        }
    }

    fn request_history_detail(&mut self, id: i64, variant: HistoryVariant) {
        self.shell.clear_history_detail();
        if let Err(error) = self.history_loader.request(HistoryLoadKey {
            id,
            variant,
            intent: HistoryLoadIntent::Detail,
        }) {
            self.set_error(error);
            self.refresh();
        }
    }

    fn request_history_copy(&mut self, id: i64, variant: HistoryVariant) {
        if let Err(error) = self.history_loader.request(HistoryLoadKey {
            id,
            variant,
            intent: HistoryLoadIntent::Copy,
        }) {
            self.set_error(error);
            self.refresh();
        }
    }

    fn apply_history_result(&mut self, result: HistoryLoadResult, ctx: &egui::Context) {
        let selection = self.shell.history_selection();
        if !history_result_is_current(self.route, selection, result.key) {
            return;
        }
        match result.value {
            Ok(Some(text)) => match result.key.intent {
                HistoryLoadIntent::Detail => {
                    if !self
                        .shell
                        .set_history_detail(result.key.id, result.key.variant, text)
                    {
                        self.history_item_missing();
                    }
                }
                HistoryLoadIntent::Copy => {
                    ctx.copy_text(text);
                    self.notice = Some(InlineNotice {
                        kind: NoticeKind::Information,
                        title: "Copied.".to_owned(),
                        detail: "The selected transcript text is on the clipboard.".to_owned(),
                        action: None,
                    });
                    self.refresh();
                    self.ensure_history_detail();
                }
            },
            Ok(None) => self.history_item_missing(),
            Err(error) => {
                self.set_error(error);
                self.refresh();
            }
        }
    }

    fn history_item_missing(&mut self) {
        self.history_loader.invalidate();
        self.shell.clear_history_detail();
        self.notice = Some(InlineNotice {
            kind: NoticeKind::Information,
            title: "History changed.".to_owned(),
            detail: "That retained dictation or text variant is no longer available.".to_owned(),
            action: None,
        });
        self.refresh();
        if self.route == UiRoute::History {
            self.ensure_history_detail();
        }
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
                    // The resident recognizer still represents the old saved
                    // configuration until the requested restart completes.
                    self.loaded_whisper = None;
                    self.readiness =
                        UiReadinessSnapshot::checking(self.bridge.settings(), &self.store);
                    self.request_readiness(AUTOMATIC_REFRESH_VOSK_PROBE);
                }
                self.refresh();
                if self.route == UiRoute::History {
                    self.ensure_history_detail();
                }
                true
            }
            Err(error) => {
                self.set_error(error.to_string());
                false
            }
        }
    }

    fn request_readiness(&mut self, vosk_probe: UiVoskProbe) {
        if self.readiness_in_flight {
            self.readiness_refresh_pending = true;
            return;
        }
        self.readiness_in_flight = true;
        let (sender, receiver) = mpsc::sync_channel(1);
        self.readiness_rx = receiver;
        if !probe_readiness(
            self.bridge.settings().clone(),
            self.store.clone(),
            self.loaded_whisper.clone(),
            vosk_probe,
            sender,
        ) {
            self.readiness_in_flight = false;
            self.set_error("Local readiness inspection could not start.".to_owned());
        }
    }

    fn handle_shell_event(&mut self, event: ShellEvent) {
        match event {
            ShellEvent::Navigate(route) => {
                let destination = unmap_route(route);
                if self.route == UiRoute::Setup && destination != UiRoute::Setup {
                    self.setup_features.deactivate();
                } else if self.route != UiRoute::Setup && destination == UiRoute::Setup {
                    self.setup_features.activate(self.bridge.settings());
                }
                self.route = destination;
                if self.route != UiRoute::History {
                    self.history_loader.invalidate();
                    self.shell.clear_history_detail();
                }
                self.refresh();
                if self.route == UiRoute::History {
                    self.ensure_history_detail();
                }
            }
            ShellEvent::TestDictation => {
                let _ = self.events.send(ProductShellEvent::TestDictation);
            }
            ShellEvent::CopyHistory { id, variant } => {
                self.request_history_copy(id, variant);
            }
            ShellEvent::ClearHistory => {
                self.history_loader.invalidate();
                self.shell.clear_history_detail();
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
                self.request_readiness(UiVoskProbe::FullValidation);
                self.refresh();
            }
            ShellEvent::RefreshSetup => {
                if let Some(message) = self
                    .setup_center
                    .as_mut()
                    .and_then(|center| center.retry_recovery().err())
                {
                    self.set_error(message.to_owned());
                    self.refresh();
                    return;
                }
                self.readiness = UiReadinessSnapshot::checking(self.bridge.settings(), &self.store);
                self.request_readiness(UiVoskProbe::FullValidation);
                self.setup_features.refresh_local_features();
                self.refresh();
            }
            ShellEvent::StartSetupAction(id) | ShellEvent::RetrySetupAction(id) => {
                if self.setup_features.mutation_conflict() {
                    self.set_error("Finish or cancel calibration, measurement, or model acquisition before changing setup assets.".to_owned());
                    self.refresh();
                    return;
                }
                let result = self
                    .setup_center
                    .as_mut()
                    .ok_or("Setup and repair are unavailable in this session.")
                    .and_then(|center| center.start(&id));
                if let Err(message) = result {
                    self.set_error(message.to_owned());
                }
                self.refresh();
            }
            ShellEvent::CancelSetupAction(id) => {
                let result = self
                    .setup_center
                    .as_mut()
                    .ok_or("Setup and repair are unavailable in this session.")
                    .and_then(|center| center.cancel(&id));
                if let Err(message) = result {
                    self.set_error(message.to_owned());
                }
                self.refresh();
            }
            ShellEvent::NoticeAction if self.download_active => {
                let _ = self
                    .events
                    .send(ProductShellEvent::CancelWhisperModelDownload);
            }
            ShellEvent::NoticeAction => {
                self.readiness = UiReadinessSnapshot::checking(self.bridge.settings(), &self.store);
                self.request_readiness(UiVoskProbe::FullValidation);
                self.refresh();
            }
            ShellEvent::ChangeWhisperModel(variant) => {
                if variant != ShellAccurateModel::Custom {
                    let _ = self.events.send(ProductShellEvent::ChangeWhisperModel(
                        unmap_accurate_model(variant),
                    ));
                }
            }
            ShellEvent::InstallVerifiedVoskAssets => {
                let _ = self
                    .events
                    .send(ProductShellEvent::InstallVerifiedVoskAssets);
            }
            ShellEvent::SelectOllamaModel(model) => {
                let mut settings = self.bridge.settings().clone();
                settings.formatting.ollama_model = Some(model);
                settings.formatting.ollama_model_identity = None;
                self.execute(UiCommand::SaveSettings(settings));
            }
            ShellEvent::InspectOllama => {
                self.setup_features.inspect_ollama();
                self.refresh();
            }
            ShellEvent::OpenOfficialOllamaDownload => {
                self.setup_features.open_official_ollama_page();
                self.refresh();
            }
            ShellEvent::PullCuratedOllamaModel(id) => {
                if self.setup_mutation_active() {
                    self.set_error(
                        "Finish the active setup repair before acquiring an Ollama model."
                            .to_owned(),
                    );
                    self.refresh();
                    return;
                }
                self.setup_features.pull_ollama_model(&id);
                self.refresh();
            }
            ShellEvent::CancelOllamaPull => {
                self.setup_features.cancel_ollama_pull();
                self.refresh();
            }
            ShellEvent::ActivateCuratedOllamaModel(id) => {
                if self.setup_mutation_active() {
                    self.set_error(
                        "Finish the active setup repair before changing the runtime model."
                            .to_owned(),
                    );
                    self.refresh();
                    return;
                }
                self.setup_features.activate_ollama_model(&id);
                self.refresh();
            }
            ShellEvent::SelectBenchmarkCandidate(id) => {
                self.setup_features.select_benchmark_candidate(&id);
                self.refresh();
            }
            ShellEvent::SetPerformancePreference(preference) => {
                self.setup_features.set_preference(preference);
                self.refresh();
            }
            ShellEvent::StartCalibrationCapture(id) => {
                if self.setup_mutation_active() {
                    self.set_error(
                        "Finish the active setup repair before opening the calibration microphone."
                            .to_owned(),
                    );
                    self.refresh();
                    return;
                }
                self.setup_features
                    .start_capture(&id, self.bridge.settings().recognition.microphone.clone());
                self.refresh();
            }
            ShellEvent::StopCalibrationCapture(id) => {
                self.setup_features.stop_capture(&id);
                self.refresh();
            }
            ShellEvent::DiscardCalibrationCapture(id) => {
                self.setup_features.discard_capture(&id);
                self.refresh();
            }
            ShellEvent::ResetPerformanceCalibration => {
                self.setup_features.reset_performance_calibration();
                self.refresh();
            }
            ShellEvent::StartPerformanceBenchmark => {
                if self.setup_mutation_active() {
                    self.set_error(
                        "Finish the active setup repair before measuring performance.".to_owned(),
                    );
                    self.refresh();
                    return;
                }
                self.setup_features.start_benchmark();
                self.refresh();
            }
            ShellEvent::CancelPerformanceBenchmark => {
                self.setup_features.cancel_benchmark();
                self.refresh();
            }
            ShellEvent::ApplyPerformanceRecommendation(id) => {
                if self.setup_mutation_active() {
                    self.set_error(
                        "Finish the active setup repair before applying a recommendation."
                            .to_owned(),
                    );
                    self.refresh();
                    return;
                }
                self.setup_features.apply_recommendation(&id);
                self.refresh();
            }
            ShellEvent::RevertPerformanceRecommendation => {
                if self.setup_mutation_active() {
                    self.set_error(
                        "Finish the active setup repair before reverting a recommendation."
                            .to_owned(),
                    );
                    self.refresh();
                    return;
                }
                self.setup_features.revert_recommendation();
                self.refresh();
            }
            ShellEvent::DiscardPerformanceRollback => {
                if self.setup_mutation_active() {
                    self.set_error(
                        "Finish the active setup repair before discarding rollback state."
                            .to_owned(),
                    );
                    self.refresh();
                    return;
                }
                self.setup_features.discard_persisted_rollback();
                self.refresh();
            }
            ShellEvent::SaveSettings(form) => {
                let settings = apply_settings_snapshot(self.bridge.settings(), &form);
                self.execute(UiCommand::SaveSettings(settings));
            }
            ShellEvent::DismissNotice => {
                self.notice = None;
                self.refresh();
            }
            ShellEvent::SelectHistory(id) => {
                self.shell.select_history(id);
                if let Some((selected_id, variant)) = self.shell.history_selection() {
                    self.request_history_detail(selected_id, variant);
                }
            }
            ShellEvent::SelectHistoryVariant { id, variant } => {
                self.request_history_detail(id, variant);
            }
            ShellEvent::NewLexiconEntry
            | ShellEvent::EditLexicon(_)
            | ShellEvent::NewProfile
            | ShellEvent::EditProfile(_)
            | ShellEvent::ApplySetupRecommendation(_) => {}
            ShellEvent::CancelLexiconEdit => self.shell.close_lexicon_editor(),
            ShellEvent::CancelProfileEdit => self.shell.close_profile_editor(),
        }
    }
}

fn history_result_is_current(
    route: UiRoute,
    selection: Option<(i64, HistoryVariant)>,
    key: HistoryLoadKey,
) -> bool {
    if route != UiRoute::History {
        return false;
    }
    match key.intent {
        HistoryLoadIntent::Detail => selection == Some((key.id, key.variant)),
        HistoryLoadIntent::Copy => selection.is_some_and(|(id, _)| id == key.id),
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
                    if self.route == UiRoute::Setup && route != UiRoute::Setup {
                        self.setup_features.deactivate();
                    } else if self.route != UiRoute::Setup && route == UiRoute::Setup {
                        self.setup_features.activate(self.bridge.settings());
                    }
                    self.window_visible = true;
                    self.route = route;
                    if self.route != UiRoute::History {
                        self.history_loader.invalidate();
                        self.shell.clear_history_detail();
                    }
                    self.shell.request_route_focus();
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.refresh();
                    if self.route == UiRoute::History {
                        self.ensure_history_detail();
                    }
                }
                ProductShellControl::SetRuntimeStatus(status) => {
                    self.runtime_status = status;
                    self.shell.set_runtime_status(map_runtime_status(status));
                }
                ProductShellControl::Refresh => {
                    self.refresh();
                    if self.route == UiRoute::History {
                        self.ensure_history_detail();
                    }
                }
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
                ProductShellControl::ModelDownloaded { path, variant } => {
                    self.download_active = false;
                    let mut settings = self.bridge.settings().clone();
                    settings.recognition.model_path = path;
                    settings.recognition.accurate_model = variant;
                    settings.startup.onboarding_complete = true;
                    self.execute(UiCommand::SaveSettings(settings));
                }
                ProductShellControl::ModelDownloadFailed => {
                    self.download_active = false;
                    self.set_error("The verified model could not be downloaded. Your previous model was not changed.".to_owned());
                    self.refresh();
                }
                ProductShellControl::VoskInstallFailed(message) => {
                    self.set_error(message);
                    self.refresh();
                }
                ProductShellControl::ActivationBlocked(message) => {
                    self.set_error(message);
                    self.refresh();
                }
                ProductShellControl::Quit => {
                    self.setup_features.deactivate();
                    self.window_visible = false;
                    self.history_loader.invalidate();
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        match self.readiness_rx.try_recv() {
            Ok(readiness) => {
                self.readiness_in_flight = false;
                if self.readiness_refresh_pending {
                    self.readiness_refresh_pending = false;
                    self.request_readiness(UiVoskProbe::FullValidation);
                    return;
                }
                self.readiness = readiness;
                self.setup_features.reconfigure(self.bridge.settings());
                if let Some(center) = self.setup_center.as_mut()
                    && center
                        .replan(self.bridge.settings(), &self.readiness)
                        .is_err()
                {
                    self.set_error("A safe setup plan could not be created.".to_owned());
                }
                self.refresh();
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        if let Some(center) = self.setup_center.as_mut() {
            match center.poll() {
                Ok(true) => {
                    let terminal = center.terminal();
                    self.refresh();
                    if terminal {
                        let previous_recognition = self.bridge.settings().recognition.clone();
                        if self.bridge.reload_settings().is_err() {
                            self.set_error(
                                "Updated setup settings could not be reloaded.".to_owned(),
                            );
                        } else if self.bridge.settings().recognition != previous_recognition {
                            let _ = self.events.send(ProductShellEvent::RuntimeReloadRequested(
                                UiMutation::SettingsSaved,
                            ));
                        }
                        self.readiness =
                            UiReadinessSnapshot::checking(self.bridge.settings(), &self.store);
                        self.request_readiness(UiVoskProbe::FullValidation);
                        self.setup_features.refresh_local_features();
                    }
                }
                Ok(false) => {}
                Err(message) => {
                    self.set_error(message.to_owned());
                    self.setup_center = None;
                    self.refresh();
                }
            }
        }
        let feature_poll = self.setup_features.poll();
        if feature_poll.settings_refresh && self.bridge.reload_settings().is_err() {
            self.set_error(
                "Updated setup settings could not be reloaded in the control panel.".to_owned(),
            );
        }
        if feature_poll.runtime_reload {
            if self.bridge.reload_settings().is_err() {
                self.set_error("Updated setup settings could not be reloaded.".to_owned());
            } else {
                let _ = self.events.send(ProductShellEvent::RuntimeReloadRequested(
                    UiMutation::SettingsSaved,
                ));
                self.setup_features.reconfigure(self.bridge.settings());
            }
        }
        if feature_poll.changed {
            self.refresh();
        }
        if ctx.input(|input| input.viewport().close_requested()) && !self.quitting {
            self.setup_features.deactivate();
            self.window_visible = false;
            self.history_loader.invalidate();
            self.shell.clear_history_detail();
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            let _ = self.events.send(ProductShellEvent::Hidden);
        }
        // The fixed repaint cadence below observes background completion. We
        // intentionally process controls and close requests first so a stale
        // copy cannot win a navigation or shutdown race.
        if !self.quitting
            && self.route == UiRoute::History
            && let Some(result) = self.history_loader.take_result()
        {
            self.apply_history_result(result, ctx);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.show(ui);
        for event in self.shell.take_events() {
            self.handle_shell_event(event);
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
            output_preview: item.selected_output_preview,
            preview_truncated: item.preview_truncated,
            variants: HistoryVariantAvailability {
                output: item.variants.output,
                raw: item.variants.raw,
                normalized: item.variants.normalized,
                cleaned: item.variants.cleaned,
            },
            loaded: None,
            latency: item.release_to_insert_duration_ms.map_or_else(
                || {
                    format_duration(
                        item.stt_duration_ms,
                        item.formatting_duration_ms,
                        item.insertion_duration_ms,
                    )
                },
                |duration| format!("{duration} ms"),
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
    let vosk = &snapshot.readiness.vosk;
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
            SystemReadiness::new("Vosk", vosk.message.clone(), map_readiness(vosk.state)),
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
        vosk: ModelSystem {
            name: "Vosk Instant".to_owned(),
            selected: vosk
                .model_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            detail: vosk.message.clone(),
            state: map_readiness(vosk.state),
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
        setup: setup_snapshot(microphone, whisper, vosk, ollama),
        notice,
    }
}

fn map_snapshot_with_setup(
    snapshot: UiSnapshot,
    route: UiRoute,
    notice: Option<InlineNotice>,
    setup: Option<SetupSnapshot>,
) -> ShellSnapshot {
    let mut mapped = map_snapshot(snapshot, route, notice);
    if let Some(setup) = setup {
        mapped.setup = setup;
    } else {
        mapped.setup = unavailable_setup_snapshot();
    }
    mapped
}

fn unavailable_setup_snapshot() -> SetupSnapshot {
    SetupSnapshot {
        stage: SetupStage::Blocked,
        summary: "Transactional asset repair is unavailable in this session. Local refinement and measurement remain independently inspectable.".to_owned(),
        ..SetupSnapshot::default()
    }
}

fn setup_snapshot(
    microphone: &crate::ui_bridge::UiMicrophoneReadiness,
    whisper: &crate::ui_bridge::UiWhisperReadiness,
    vosk: &crate::ui_bridge::UiVoskReadiness,
    ollama: &crate::ui_bridge::UiOllamaReadiness,
) -> SetupSnapshot {
    let core_ready = microphone.state == UiReadinessState::Ready
        && (whisper.state == UiReadinessState::Ready || vosk.state == UiReadinessState::Ready);
    SetupSnapshot {
        stage: if [microphone.state, whisper.state, vosk.state, ollama.state]
            .contains(&UiReadinessState::Checking)
        {
            SetupStage::Discovering
        } else if core_ready {
            SetupStage::Ready
        } else {
            SetupStage::Blocked
        },
        summary: if core_ready {
            "Core local dictation is operational. Optional systems are reported separately."
                .to_owned()
        } else {
            "One or more required local capabilities need attention.".to_owned()
        },
        capabilities: vec![
            SetupCapability {
                id: "microphone".to_owned(),
                name: "Microphone".to_owned(),
                detail: microphone.message.clone(),
                state: map_readiness(microphone.state),
                remedy: (microphone.state == UiReadinessState::NeedsAttention).then(|| {
                    "Choose an available microphone or inspect Windows privacy access.".to_owned()
                }),
            },
            SetupCapability {
                id: "accurate-recognition".to_owned(),
                name: "Accurate recognition".to_owned(),
                detail: whisper.message.clone(),
                state: map_readiness(whisper.state),
                remedy: (whisper.state == UiReadinessState::NeedsAttention)
                    .then(|| "Verify or acquire a compatible Whisper model.".to_owned()),
            },
            SetupCapability {
                id: "instant-recognition".to_owned(),
                name: "Instant recognition".to_owned(),
                detail: vosk.message.clone(),
                state: map_readiness(vosk.state),
                remedy: (vosk.state == UiReadinessState::NeedsAttention).then(|| {
                    "Verify or acquire matching Vosk runtime and model assets.".to_owned()
                }),
            },
            SetupCapability {
                id: "ollama".to_owned(),
                name: "Local refinement".to_owned(),
                detail: ollama.message.clone(),
                state: if ollama.state == UiReadinessState::NeedsAttention {
                    Readiness::Optional
                } else {
                    map_readiness(ollama.state)
                },
                remedy: (ollama.state == UiReadinessState::NeedsAttention)
                    .then(|| "Optional. Light formatting remains ready without Ollama.".to_owned()),
            },
        ],
        actions: Vec::new(),
        recommendation: None,
        ..SetupSnapshot::default()
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
        recognition_mode: match settings.recognition.mode {
            RecognitionMode::Instant => ShellRecognitionMode::Instant,
            RecognitionMode::Accurate => ShellRecognitionMode::Accurate,
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
        accurate_model: map_accurate_model(settings.recognition.accurate_model),
        accurate_backend: match settings.recognition.accurate_backend {
            AccurateBackendPreference::Auto => ShellAccurateBackend::Auto,
            AccurateBackendPreference::Vulkan => ShellAccurateBackend::Vulkan,
            AccurateBackendPreference::Cpu => ShellAccurateBackend::Cpu,
        },
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
    settings.recognition.mode = match form.recognition_mode {
        ShellRecognitionMode::Instant => RecognitionMode::Instant,
        ShellRecognitionMode::Accurate => RecognitionMode::Accurate,
    };
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
    settings.recognition.instant_model_path = PathBuf::from(form.instant_model_path.trim());
    settings.recognition.instant_runtime_path = PathBuf::from(form.instant_runtime_path.trim());
    settings.recognition.accurate_model = unmap_accurate_model(form.accurate_model);
    settings.recognition.accurate_backend = match form.accurate_backend {
        ShellAccurateBackend::Auto => AccurateBackendPreference::Auto,
        ShellAccurateBackend::Vulkan => AccurateBackendPreference::Vulkan,
        ShellAccurateBackend::Cpu => AccurateBackendPreference::Cpu,
    };
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

fn map_accurate_model(value: AccurateModelVariant) -> ShellAccurateModel {
    match value {
        AccurateModelVariant::TinyEnglish => ShellAccurateModel::TinyEnglish,
        AccurateModelVariant::BaseEnglish => ShellAccurateModel::BaseEnglish,
        AccurateModelVariant::TinyMultilingual => ShellAccurateModel::TinyMultilingual,
        AccurateModelVariant::BaseMultilingual => ShellAccurateModel::BaseMultilingual,
        AccurateModelVariant::Custom => ShellAccurateModel::Custom,
    }
}

fn unmap_accurate_model(value: ShellAccurateModel) -> AccurateModelVariant {
    match value {
        ShellAccurateModel::TinyEnglish => AccurateModelVariant::TinyEnglish,
        ShellAccurateModel::BaseEnglish => AccurateModelVariant::BaseEnglish,
        ShellAccurateModel::TinyMultilingual => AccurateModelVariant::TinyMultilingual,
        ShellAccurateModel::BaseMultilingual => AccurateModelVariant::BaseMultilingual,
        ShellAccurateModel::Custom => AccurateModelVariant::Custom,
    }
}

fn optional(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}
fn map_route(route: UiRoute) -> Route {
    match route {
        UiRoute::Home => Route::Home,
        UiRoute::Setup => Route::Setup,
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
        Route::Setup => UiRoute::Setup,
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
    fn automatic_refresh_policy_never_full_loads_vosk() {
        assert_eq!(AUTOMATIC_REFRESH_VOSK_PROBE, UiVoskProbe::LayoutOnly);
        assert_ne!(AUTOMATIC_REFRESH_VOSK_PROBE, UiVoskProbe::FullValidation);
    }

    #[test]
    fn route_mapping_is_bidirectional() {
        for route in [
            UiRoute::Home,
            UiRoute::Setup,
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
    fn history_results_require_the_current_route_selection_and_intent() {
        let detail = HistoryLoadKey {
            id: 7,
            variant: HistoryVariant::Raw,
            intent: HistoryLoadIntent::Detail,
        };
        let copy = HistoryLoadKey {
            intent: HistoryLoadIntent::Copy,
            ..detail
        };
        assert!(history_result_is_current(
            UiRoute::History,
            Some((7, HistoryVariant::Raw)),
            detail
        ));
        assert!(!history_result_is_current(
            UiRoute::Home,
            Some((7, HistoryVariant::Raw)),
            copy
        ));
        assert!(!history_result_is_current(
            UiRoute::History,
            Some((8, HistoryVariant::Raw)),
            copy
        ));
        assert!(!history_result_is_current(
            UiRoute::History,
            Some((7, HistoryVariant::Output)),
            detail
        ));
        assert!(history_result_is_current(
            UiRoute::History,
            Some((7, HistoryVariant::Output)),
            copy
        ));
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

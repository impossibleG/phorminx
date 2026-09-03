//! Typed boundary between the product shell and the dictation runtime.

use std::fmt;

/// Durable routes in the single-window product shell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Route {
    #[default]
    Home,
    Setup,
    History,
    Lexicon,
    Profiles,
    Models,
    Settings,
}

impl Route {
    pub const ALL: [Self; 7] = [
        Self::Home,
        Self::Setup,
        Self::History,
        Self::Lexicon,
        Self::Profiles,
        Self::Models,
        Self::Settings,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Setup => "Setup",
            Self::History => "History",
            Self::Lexicon => "Lexicon",
            Self::Profiles => "Profiles",
            Self::Models => "Models",
            Self::Settings => "Settings",
        }
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Home => "The instrument at rest",
            Self::Setup => "Commissioning",
            Self::History => "Recovered thought",
            Self::Lexicon => "A deliberate vocabulary",
            Self::Profiles => "Policy by application",
            Self::Models => "Local machinery",
            Self::Settings => "Preferences",
        }
    }

    #[must_use]
    pub const fn context(self) -> &'static str {
        match self {
            Self::Home => "Voice, disciplined.",
            Self::Setup => "Every local system, proven.",
            Self::History => "Compare what was spoken with what was kept.",
            Self::Lexicon => "Exact names. Exact replacements.",
            Self::Profiles => "Let context govern the instrument.",
            Self::Models => "Nothing leaves this machine.",
            Self::Settings => "Infrequent choices, held quietly.",
        }
    }
}

impl fmt::Display for Route {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RuntimeStatus {
    #[default]
    Ready,
    Listening,
    Transcribing,
    Refining,
    Inserted,
    Copied,
    NeedsAttention,
}

impl RuntimeStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Listening => "Listening",
            Self::Transcribing => "Transcribing",
            Self::Refining => "Refining",
            Self::Inserted => "Inserted",
            Self::Copied => "Copied",
            Self::NeedsAttention => "Needs attention",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    Ready,
    Working,
    Optional,
    Unavailable,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemReadiness {
    pub name: String,
    pub detail: String,
    pub state: Readiness,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SetupStage {
    #[default]
    Discovering,
    PlanReady,
    AwaitingConsent,
    Working,
    Benchmarking,
    Ready,
    Blocked,
}

impl SetupStage {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Discovering => "Inspecting this machine",
            Self::PlanReady => "Plan ready",
            Self::AwaitingConsent => "Awaiting consent",
            Self::Working => "Applying the plan",
            Self::Benchmarking => "Measuring locally",
            Self::Ready => "Commissioned",
            Self::Blocked => "Needs attention",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupCapability {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub state: Readiness,
    pub remedy: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupAction {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub progress_percent: Option<u8>,
    pub consent: Vec<String>,
    pub running: bool,
    pub can_retry: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupRecommendation {
    pub id: String,
    pub title: String,
    pub rationale: String,
    pub evidence: Vec<String>,
    pub can_apply: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupSnapshot {
    pub stage: SetupStage,
    pub summary: String,
    pub capabilities: Vec<SetupCapability>,
    pub actions: Vec<SetupAction>,
    pub recommendation: Option<SetupRecommendation>,
}

impl Default for SetupSnapshot {
    fn default() -> Self {
        Self {
            stage: SetupStage::Discovering,
            summary: "Reading local capabilities. No changes are being made.".into(),
            capabilities: Vec::new(),
            actions: Vec::new(),
            recommendation: None,
        }
    }
}

impl SystemReadiness {
    #[must_use]
    pub fn new(name: impl Into<String>, detail: impl Into<String>, state: Readiness) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
            state,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HistoryVariant {
    #[default]
    Output,
    Raw,
    Normalized,
    Cleaned,
}

impl HistoryVariant {
    pub const ALL: [Self; 4] = [Self::Output, Self::Raw, Self::Normalized, Self::Cleaned];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Output => "Output",
            Self::Raw => "Raw",
            Self::Normalized => "Normalized",
            Self::Cleaned => "Cleaned",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryItem {
    pub id: i64,
    pub time: String,
    pub application: String,
    pub language: String,
    pub output: String,
    pub raw: Option<String>,
    pub normalized: Option<String>,
    pub cleaned: Option<String>,
    pub latency: String,
    pub warning: Option<String>,
}

impl HistoryItem {
    #[must_use]
    pub fn text_for(&self, variant: HistoryVariant) -> Option<&str> {
        match variant {
            HistoryVariant::Output => Some(&self.output),
            HistoryVariant::Raw => self.raw.as_deref(),
            HistoryVariant::Normalized => self.normalized.as_deref(),
            HistoryVariant::Cleaned => self.cleaned.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexiconEntry {
    pub id: i64,
    pub spoken: String,
    pub written: String,
    pub language: String,
    pub scope: String,
    pub case_policy: LexiconCasePolicy,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexiconDraft {
    pub id: Option<i64>,
    pub spoken: String,
    pub written: String,
    pub language: String,
    pub scope: String,
    pub case_policy: LexiconCasePolicy,
    pub enabled: bool,
}

impl Default for LexiconDraft {
    fn default() -> Self {
        Self {
            id: None,
            spoken: String::new(),
            written: String::new(),
            language: String::new(),
            scope: String::new(),
            case_policy: LexiconCasePolicy::UseCanonical,
            enabled: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LexiconCasePolicy {
    PreserveInput,
    #[default]
    UseCanonical,
    Lowercase,
    Uppercase,
}

impl LexiconCasePolicy {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PreserveInput => "Preserve spoken case",
            Self::UseCanonical => "Use written case",
            Self::Lowercase => "Lowercase",
            Self::Uppercase => "Uppercase",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationProfile {
    pub executable: String,
    pub formatting: String,
    pub language: String,
    pub insertion: String,
    pub blocked: bool,
    pub custom_instruction: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileDraft {
    pub original_executable: Option<String>,
    pub executable: String,
    pub formatting: FormattingStrength,
    pub custom_instruction: String,
    pub language: String,
    pub insertion: ProfileInsertion,
    pub blocked: bool,
}

impl Default for ProfileDraft {
    fn default() -> Self {
        Self {
            original_executable: None,
            executable: String::new(),
            formatting: FormattingStrength::Balanced,
            custom_instruction: String::new(),
            language: String::new(),
            insertion: ProfileInsertion::Automatic,
            blocked: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProfileInsertion {
    #[default]
    Automatic,
    Direct,
    Clipboard,
}

impl ApplicationProfile {
    #[must_use]
    pub fn summary(&self) -> String {
        if self.blocked {
            format!("In {}: Dictation blocked", self.executable)
        } else {
            format!(
                "In {}: {} formatting · {} · {}",
                self.executable, self.formatting, self.language, self.insertion
            )
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelSystem {
    pub name: String,
    pub selected: Option<String>,
    pub detail: String,
    pub state: Readiness,
    pub installed: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordingMode {
    #[default]
    Hold,
    Toggle,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FormattingStrength {
    Raw,
    Light,
    #[default]
    Balanced,
    Strong,
    Custom,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OllamaLifecycle {
    Instant,
    #[default]
    Balanced,
    MemorySaver,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AppearancePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecognitionMode {
    Instant,
    #[default]
    Accurate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsSnapshot {
    pub appearance: AppearancePreference,
    pub microphone: String,
    pub microphones: Vec<String>,
    pub recording_mode: RecordingMode,
    pub language: String,
    pub recognition_mode: RecognitionMode,
    pub formatting: FormattingStrength,
    pub custom_instruction: String,
    pub minimum_rms: String,
    pub ollama_lifecycle: OllamaLifecycle,
    pub model_path: String,
    pub instant_model_path: String,
    pub instant_runtime_path: String,
    pub accurate_model: AccurateModel,
    pub accurate_backend: AccurateBackend,
    pub history_retention: String,
    pub launch_at_login: bool,
}

impl Default for SettingsSnapshot {
    fn default() -> Self {
        Self {
            appearance: AppearancePreference::System,
            microphone: "Windows default".into(),
            microphones: vec!["Windows default".into()],
            recording_mode: RecordingMode::Hold,
            language: "English".into(),
            recognition_mode: RecognitionMode::Accurate,
            formatting: FormattingStrength::Balanced,
            custom_instruction: String::new(),
            minimum_rms: "0.003".into(),
            ollama_lifecycle: OllamaLifecycle::Balanced,
            model_path: "models/ggml-base.en.bin".into(),
            instant_model_path: "models/vosk-model-small-en-us-0.15".into(),
            instant_runtime_path: "runtime/vosk".into(),
            accurate_model: AccurateModel::BaseEnglish,
            accurate_backend: AccurateBackend::Auto,
            history_retention: "7 days".into(),
            launch_at_login: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AccurateModel {
    TinyEnglish,
    #[default]
    BaseEnglish,
    TinyMultilingual,
    BaseMultilingual,
    Custom,
}

impl AccurateModel {
    pub const ALL: [Self; 5] = [
        Self::TinyEnglish,
        Self::BaseEnglish,
        Self::TinyMultilingual,
        Self::BaseMultilingual,
        Self::Custom,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::TinyEnglish => "Tiny · English",
            Self::BaseEnglish => "Base · English",
            Self::TinyMultilingual => "Tiny · Multilingual",
            Self::BaseMultilingual => "Base · Multilingual",
            Self::Custom => "Custom file",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AccurateBackend {
    #[default]
    Auto,
    Vulkan,
    Cpu,
}

impl AccurateBackend {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Vulkan, Self::Cpu];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Vulkan => "Vulkan GPU",
            Self::Cpu => "CPU",
        }
    }
}

/// Immutable view-state supplied by the host runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellSnapshot {
    pub route: Route,
    pub status: RuntimeStatus,
    pub shortcut: String,
    pub systems: Vec<SystemReadiness>,
    pub history_enabled: bool,
    pub history: Vec<HistoryItem>,
    pub lexicon: Vec<LexiconEntry>,
    pub profiles: Vec<ApplicationProfile>,
    pub whisper: ModelSystem,
    pub vosk: ModelSystem,
    pub ollama: ModelSystem,
    pub settings: SettingsSnapshot,
    pub setup: SetupSnapshot,
    pub notice: Option<InlineNotice>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoticeKind {
    Information,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineNotice {
    pub kind: NoticeKind,
    pub title: String,
    pub detail: String,
    pub action: Option<String>,
}

/// Deliberate visual-proof inputs used by the component gallery and screenshot tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GalleryScenario {
    Empty,
    #[default]
    Populated,
    Error,
}

impl GalleryScenario {
    pub const ALL: [Self; 3] = [Self::Empty, Self::Populated, Self::Error];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "Empty",
            Self::Populated => "Populated",
            Self::Error => "Error",
        }
    }
}

impl ShellSnapshot {
    #[must_use]
    pub fn gallery(scenario: GalleryScenario) -> Self {
        let history = match scenario {
            GalleryScenario::Populated => vec![
                HistoryItem {
                    id: 31,
                    time: "14:32".into(),
                    application: "code.exe".into(),
                    language: "EN".into(),
                    output: "The quieter the interface, the more exact each decision must be."
                        .into(),
                    raw: Some(
                        "the quieter the interface the more exact each decision must be".into(),
                    ),
                    normalized: Some(
                        "The quieter the interface, the more exact each decision must be.".into(),
                    ),
                    cleaned: Some(
                        "The quieter the interface, the more exact each decision must be.".into(),
                    ),
                    latency: "1.84 s".into(),
                    warning: None,
                },
                HistoryItem {
                    id: 30,
                    time: "13:08".into(),
                    application: "notepad.exe".into(),
                    language: "EN".into(),
                    output: "Keep the local boundary explicit.".into(),
                    raw: Some("keep the local boundary explicit".into()),
                    normalized: None,
                    cleaned: None,
                    latency: "1.12 s".into(),
                    warning: Some("Copied. Target changed.".into()),
                },
            ],
            GalleryScenario::Empty | GalleryScenario::Error => Vec::new(),
        };

        let lexicon = if scenario == GalleryScenario::Populated {
            vec![
                LexiconEntry {
                    id: 7,
                    spoken: "form inks".into(),
                    written: "Phorminx".into(),
                    language: "English".into(),
                    scope: "Everywhere".into(),
                    case_policy: LexiconCasePolicy::UseCanonical,
                    enabled: true,
                },
                LexiconEntry {
                    id: 8,
                    spoken: "e gooey".into(),
                    written: "egui".into(),
                    language: "English".into(),
                    scope: "code.exe".into(),
                    case_policy: LexiconCasePolicy::PreserveInput,
                    enabled: true,
                },
            ]
        } else {
            Vec::new()
        };

        let profiles = if scenario == GalleryScenario::Populated {
            vec![
                ApplicationProfile {
                    executable: "code.exe".into(),
                    formatting: "Light".into(),
                    language: "English".into(),
                    insertion: "Clipboard only".into(),
                    blocked: false,
                    custom_instruction: String::new(),
                },
                ApplicationProfile {
                    executable: "keepass.exe".into(),
                    formatting: "Raw".into(),
                    language: "English".into(),
                    insertion: "Never".into(),
                    blocked: true,
                    custom_instruction: String::new(),
                },
            ]
        } else {
            Vec::new()
        };

        let error = scenario == GalleryScenario::Error;
        Self {
            route: Route::Home,
            status: if error {
                RuntimeStatus::NeedsAttention
            } else {
                RuntimeStatus::Ready
            },
            shortcut: "Ctrl  Alt  Space".into(),
            systems: vec![
                SystemReadiness::new("Microphone", "Studio USB", Readiness::Ready),
                SystemReadiness::new("Whisper", "base.en · verified", Readiness::Ready),
                SystemReadiness::new(
                    "Ollama",
                    if error {
                        "Not running"
                    } else {
                        "qwen3:4b · resident"
                    },
                    if error {
                        Readiness::Error
                    } else {
                        Readiness::Ready
                    },
                ),
            ],
            history_enabled: true,
            history,
            lexicon,
            profiles,
            whisper: ModelSystem {
                name: "Whisper".into(),
                selected: Some("base.en".into()),
                detail: "English · 141 MB · SHA-256 verified".into(),
                state: Readiness::Ready,
                installed: vec!["base.en".into()],
            },
            vosk: ModelSystem {
                name: "Vosk Instant".into(),
                selected: Some("small-en-us-0.15".into()),
                detail: "English · local streaming".into(),
                state: Readiness::Ready,
                installed: vec!["small-en-us-0.15".into()],
            },
            ollama: ModelSystem {
                name: "Ollama".into(),
                selected: (!error).then(|| "qwen3:4b".into()),
                detail: if error {
                    "Ollama is not running. Dictation will use Light output.".into()
                } else {
                    "Loopback only · balanced residency".into()
                },
                state: if error {
                    Readiness::Error
                } else {
                    Readiness::Ready
                },
                installed: if error {
                    Vec::new()
                } else {
                    vec!["qwen3:4b".into()]
                },
            },
            settings: SettingsSnapshot {
                microphones: vec!["Studio USB".into(), "Windows default".into()],
                microphone: "Studio USB".into(),
                launch_at_login: true,
                ..SettingsSnapshot::default()
            },
            setup: SetupSnapshot {
                stage: if error {
                    SetupStage::Blocked
                } else {
                    SetupStage::PlanReady
                },
                summary: if error {
                    "Core dictation is available; local refinement needs repair.".into()
                } else {
                    "The local stack is healthy. A measured recommendation is available.".into()
                },
                capabilities: vec![
                    SetupCapability {
                        id: "microphone".into(),
                        name: "Microphone".into(),
                        detail: "Studio USB · selected and available".into(),
                        state: Readiness::Ready,
                        remedy: None,
                    },
                    SetupCapability {
                        id: "accurate-en".into(),
                        name: "Accurate recognition".into(),
                        detail: "Base English · Vulkan · resident".into(),
                        state: Readiness::Ready,
                        remedy: None,
                    },
                    SetupCapability {
                        id: "ollama".into(),
                        name: "Local refinement".into(),
                        detail: if error {
                            "Ollama is not running. Light output remains available.".into()
                        } else {
                            "Ollama · loopback verified".into()
                        },
                        state: if error {
                            Readiness::Optional
                        } else {
                            Readiness::Ready
                        },
                        remedy: error.then(|| "Inspect".into()),
                    },
                ],
                actions: error
                    .then(|| SetupAction {
                        id: "repair-ollama".into(),
                        title: "Inspect local refinement".into(),
                        detail: "Determine whether Ollama is absent, stopped, or incompatible."
                            .into(),
                        progress_percent: None,
                        consent: Vec::new(),
                        running: false,
                        can_retry: false,
                    })
                    .into_iter()
                    .collect(),
                recommendation: (!error).then(|| SetupRecommendation {
                    id: "balanced-base-vulkan".into(),
                    title: "Base English on Vulkan".into(),
                    rationale: "The measured quality advantage is worth the small latency cost."
                        .into(),
                    evidence: vec![
                        "Warm release p95 · 1.18 s".into(),
                        "Backend · Vulkan-capable GPU".into(),
                    ],
                    can_apply: true,
                }),
            },
            notice: error.then(|| InlineNotice {
                kind: NoticeKind::Error,
                title: "Local refinement unavailable".into(),
                detail: "Start Ollama or continue with Light output.".into(),
                action: Some("Check again".into()),
            }),
        }
    }
}

impl Default for ShellSnapshot {
    fn default() -> Self {
        Self::gallery(GalleryScenario::Empty)
    }
}

/// User intent emitted by the shell. The host remains the sole owner of side effects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellEvent {
    Navigate(Route),
    TestDictation,
    SelectHistory(i64),
    SelectHistoryVariant(HistoryVariant),
    CopyHistory { id: i64, variant: HistoryVariant },
    ClearHistory,
    NewLexiconEntry,
    EditLexicon(i64),
    SaveLexicon(LexiconDraft),
    CancelLexiconEdit,
    DeleteLexicon(i64),
    NewProfile,
    EditProfile(String),
    SaveProfile(ProfileDraft),
    CancelProfileEdit,
    RemoveProfile(String),
    VerifyModels,
    RefreshSetup,
    StartSetupAction(String),
    CancelSetupAction(String),
    RetrySetupAction(String),
    ApplySetupRecommendation(String),
    ChangeWhisperModel(AccurateModel),
    InstallVerifiedVoskAssets,
    SelectOllamaModel(String),
    SaveSettings(SettingsSnapshot),
    DismissNotice,
    NoticeAction,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_has_distinct_copy() {
        for route in Route::ALL {
            assert!(!route.label().is_empty());
            assert!(!route.title().is_empty());
            assert!(!route.context().is_empty());
        }
    }

    #[test]
    fn history_variant_omits_absent_stages() {
        let item = &ShellSnapshot::gallery(GalleryScenario::Populated).history[1];
        assert!(item.text_for(HistoryVariant::Output).is_some());
        assert!(item.text_for(HistoryVariant::Normalized).is_none());
    }

    #[test]
    fn gallery_scenarios_exercise_product_states() {
        let empty = ShellSnapshot::gallery(GalleryScenario::Empty);
        let populated = ShellSnapshot::gallery(GalleryScenario::Populated);
        let error = ShellSnapshot::gallery(GalleryScenario::Error);

        assert!(empty.history.is_empty());
        assert!(!populated.history.is_empty());
        assert_eq!(error.status, RuntimeStatus::NeedsAttention);
        assert!(error.notice.is_some());
    }

    #[test]
    fn profile_summary_is_a_sentence() {
        let profile = &ShellSnapshot::gallery(GalleryScenario::Populated).profiles[0];
        assert_eq!(
            profile.summary(),
            "In code.exe: Light formatting · English · Clipboard only"
        );
    }
}

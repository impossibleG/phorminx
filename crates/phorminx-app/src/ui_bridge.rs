//! UI-neutral application data and command orchestration.
//!
//! The unified shell renders immutable [`UiSnapshot`] values and submits
//! [`UiCommand`] values. It never receives a mutable database handle, audio
//! samples, a window title, or a target path. Readiness probing is explicit so
//! an eventual GUI can perform blocking device and loopback checks off-thread.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use phorminx_audio::input_devices;
use phorminx_ollama::{CancellationToken, OllamaClient};
use phorminx_persistence::{
    AppProfile, CasePolicy, DictationRecord, ExecutableIdentity, FormattingStyle,
    InsertionPreference, LexiconEntry, NewLexiconEntry, Persistence, RetentionPolicy,
};
#[cfg(test)]
use phorminx_whisper::WhisperBackend;
use phorminx_whisper::{WhisperBackendPreference, WhisperReadiness, probe_backend};

use crate::model::{identify_pinned_model, model_for_variant};
use crate::settings::{
    AccurateModelVariant, FormattingStrength, HistoryRetention, Settings, SettingsError,
    SettingsStore,
};

pub const DEFAULT_HISTORY_LIMIT: usize = 100;
pub const MAX_HISTORY_LIMIT: usize = 500;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UiRoute {
    #[default]
    Home,
    History,
    Lexicon,
    Profiles,
    Models,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UiRuntimeStatus {
    Starting,
    #[default]
    Ready,
    Listening,
    Transcribing,
    Refining,
    Inserted,
    Copied,
    NoSpeech,
    NeedsAttention,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiReadinessState {
    Checking,
    Ready,
    NeedsAttention,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiMicrophone {
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiMicrophoneReadiness {
    pub state: UiReadinessState,
    pub devices: Vec<UiMicrophone>,
    pub selected: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiWhisperReadiness {
    pub state: UiReadinessState,
    pub configured_path: PathBuf,
    pub size_bytes: Option<u64>,
    pub language: String,
    pub selected_backend: Option<String>,
    pub device_name: Option<String>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiVoskReadinessKind {
    Checking,
    Ready,
    InstalledUnvalidated,
    MissingRuntime,
    MissingModel,
    LoadFailed,
    UnsupportedLanguage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiVoskProbe {
    /// The active worker already loaded the model and created a recognizer.
    ResidentReady,
    /// Inspect paths and language identity without loading native code/model.
    LayoutOnly,
    /// Explicit user-requested or pre-save native validation.
    FullValidation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiVoskReadiness {
    pub state: UiReadinessState,
    pub kind: UiVoskReadinessKind,
    pub runtime_path: PathBuf,
    pub model_path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiOllamaModel {
    pub name: String,
    pub size_bytes: Option<u64>,
    pub family: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiOllamaReadiness {
    pub state: UiReadinessState,
    pub models: Vec<UiOllamaModel>,
    pub selected: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiReadinessSnapshot {
    pub microphone: UiMicrophoneReadiness,
    pub whisper: UiWhisperReadiness,
    pub vosk: UiVoskReadiness,
    pub ollama: UiOllamaReadiness,
}

impl UiReadinessSnapshot {
    /// Probes local resources synchronously. Callers should run this away from a
    /// latency-sensitive paint/update callback because CPAL and Ollama may block.
    pub fn probe(
        settings: &Settings,
        store: &SettingsStore,
        ollama: &OllamaClient,
        loaded_whisper: Option<&WhisperReadiness>,
        vosk_probe: UiVoskProbe,
    ) -> Self {
        let selected_microphone = settings.recognition.microphone.clone();
        let microphone = match input_devices() {
            Ok(devices) => {
                let devices = devices
                    .into_iter()
                    .map(|device| UiMicrophone {
                        name: device.name,
                        is_default: device.is_default,
                    })
                    .collect::<Vec<_>>();
                microphone_readiness(selected_microphone, devices)
            }
            Err(_) => UiMicrophoneReadiness {
                state: UiReadinessState::NeedsAttention,
                devices: Vec::new(),
                selected: selected_microphone,
                message: "Microphone check failed.".to_owned(),
            },
        };

        let whisper = whisper_readiness(settings, store, loaded_whisper);

        let vosk = vosk_readiness(settings, store, vosk_probe);

        let ollama = match ollama.discover(&CancellationToken::new()) {
            Ok(catalog) => {
                let models = catalog
                    .models()
                    .iter()
                    .map(|model| UiOllamaModel {
                        name: model.name.to_string(),
                        size_bytes: model.size,
                        family: model.details.family.clone(),
                    })
                    .collect::<Vec<_>>();
                let selected = settings.formatting.ollama_model.clone();
                let selected_missing = selected
                    .as_ref()
                    .is_some_and(|name| !models.iter().any(|model| &model.name == name));
                let (state, message) = if models.is_empty() {
                    (
                        UiReadinessState::NeedsAttention,
                        "Ollama is running but has no installed models.",
                    )
                } else if selected_missing {
                    (
                        UiReadinessState::NeedsAttention,
                        "The selected Ollama model is not installed. Choose an available model.",
                    )
                } else {
                    (
                        UiReadinessState::Ready,
                        "Ollama available; warm-up is independent.",
                    )
                };
                UiOllamaReadiness {
                    state,
                    models,
                    selected,
                    message: message.to_owned(),
                }
            }
            Err(_) => UiOllamaReadiness {
                state: UiReadinessState::NeedsAttention,
                models: Vec::new(),
                selected: settings.formatting.ollama_model.clone(),
                message: "Ollama is not running. Dictation will use Light output.".to_owned(),
            },
        };

        Self {
            microphone,
            whisper,
            vosk,
            ollama,
        }
    }

    pub fn checking(settings: &Settings, store: &SettingsStore) -> Self {
        Self {
            microphone: UiMicrophoneReadiness {
                state: UiReadinessState::Checking,
                devices: Vec::new(),
                selected: settings.recognition.microphone.clone(),
                message: "Checking microphones.".to_owned(),
            },
            whisper: UiWhisperReadiness {
                state: UiReadinessState::Checking,
                configured_path: store.resolve_model_path(&settings.recognition.model_path),
                size_bytes: None,
                language: settings.recognition.language.clone(),
                selected_backend: None,
                device_name: None,
                message: "Checking Whisper model.".to_owned(),
            },
            vosk: UiVoskReadiness {
                state: UiReadinessState::Checking,
                kind: UiVoskReadinessKind::Checking,
                runtime_path: store.resolve_asset_path(&settings.recognition.instant_runtime_path),
                model_path: store.resolve_asset_path(&settings.recognition.instant_model_path),
                message: "Checking Vosk Instant assets.".to_owned(),
            },
            ollama: UiOllamaReadiness {
                state: UiReadinessState::Checking,
                models: Vec::new(),
                selected: settings.formatting.ollama_model.clone(),
                message: "Checking Ollama.".to_owned(),
            },
        }
    }

    fn has_installed_ollama_model(&self, name: &str) -> bool {
        self.ollama.state == UiReadinessState::Ready
            && self.ollama.models.iter().any(|model| model.name == name)
    }
}

fn whisper_readiness(
    settings: &Settings,
    store: &SettingsStore,
    loaded_whisper: Option<&WhisperReadiness>,
) -> UiWhisperReadiness {
    let configured_path = store.resolve_model_path(&settings.recognition.model_path);
    let backend = probe_backend(match settings.recognition.accurate_backend {
        crate::settings::AccurateBackendPreference::Auto => WhisperBackendPreference::Auto,
        crate::settings::AccurateBackendPreference::Vulkan => WhisperBackendPreference::Vulkan,
        crate::settings::AccurateBackendPreference::Cpu => WhisperBackendPreference::Cpu,
    });
    match (std::fs::metadata(&configured_path), loaded_whisper) {
        (Ok(metadata), Some(loaded)) if metadata.is_file() => {
            let fallback = loaded
                .fallback_from
                .map(|backend| format!(" after {} fallback", backend.as_str()))
                .unwrap_or_default();
            UiWhisperReadiness {
                state: UiReadinessState::Ready,
                configured_path,
                size_bytes: Some(metadata.len()),
                language: settings.recognition.language.clone(),
                selected_backend: Some(loaded.backend.as_str().to_owned()),
                device_name: loaded.device_name.clone(),
                message: format!("Whisper loaded on {}{fallback}.", loaded.backend.as_str()),
            }
        }
        (Ok(metadata), None) if metadata.is_file() && backend.is_ok() => {
            let (available_backend, _) = backend.expect("checked as successful");
            UiWhisperReadiness {
                state: UiReadinessState::NeedsAttention,
                configured_path,
                size_bytes: Some(metadata.len()),
                language: settings.recognition.language.clone(),
                selected_backend: None,
                device_name: None,
                message: format!(
                    "Whisper model available; {} is available but no recognizer is loaded.",
                    available_backend.as_str()
                ),
            }
        }
        (Ok(metadata), None) if metadata.is_file() => UiWhisperReadiness {
            state: UiReadinessState::NeedsAttention,
            configured_path,
            size_bytes: Some(metadata.len()),
            language: settings.recognition.language.clone(),
            selected_backend: None,
            device_name: None,
            message: "The requested Whisper backend is unavailable.".to_owned(),
        },
        (Ok(_), _) => UiWhisperReadiness {
            state: UiReadinessState::NeedsAttention,
            configured_path,
            size_bytes: None,
            language: settings.recognition.language.clone(),
            selected_backend: None,
            device_name: None,
            message: "The selected Whisper model is not a file.".to_owned(),
        },
        (Err(_), _) => UiWhisperReadiness {
            state: UiReadinessState::NeedsAttention,
            configured_path,
            size_bytes: None,
            language: settings.recognition.language.clone(),
            selected_backend: None,
            device_name: None,
            message: "Whisper model not found.".to_owned(),
        },
    }
}

fn vosk_readiness(
    settings: &Settings,
    store: &SettingsStore,
    probe: UiVoskProbe,
) -> UiVoskReadiness {
    let runtime = store.resolve_asset_path(&settings.recognition.instant_runtime_path);
    let model = store.resolve_asset_path(&settings.recognition.instant_model_path);
    vosk_readiness_with(
        runtime.clone(),
        model.clone(),
        &settings.recognition.language,
        probe,
        || phorminx_vosk::inspect(&runtime, &model, &settings.recognition.language),
        || phorminx_vosk::validate_asset_layout(&runtime, &model, &settings.recognition.language),
    )
}

fn vosk_readiness_with(
    runtime_path: PathBuf,
    model_path: PathBuf,
    language: &str,
    probe: UiVoskProbe,
    inspect: impl FnOnce() -> phorminx_vosk::Readiness,
    inspect_layout: impl FnOnce() -> phorminx_vosk::AssetLayout,
) -> UiVoskReadiness {
    let (state, kind, message) = match probe {
        UiVoskProbe::ResidentReady => (
            UiReadinessState::Ready,
            UiVoskReadinessKind::Ready,
            if matches!(language, "pt" | "pt-br") {
                "Vosk Instant is resident. Portuguese quality depends strongly on the selected model; Accurate mode is recommended when fidelity matters."
            } else {
                "Vosk Instant is loaded and resident."
            }
            .to_owned(),
        ),
        UiVoskProbe::LayoutOnly => match inspect_layout() {
            phorminx_vosk::AssetLayout::Present { warning } => (
                UiReadinessState::NeedsAttention,
                UiVoskReadinessKind::InstalledUnvalidated,
                warning.map_or_else(
                    || {
                        "Vosk assets are installed but inactive; they will be validated when Instant mode starts."
                            .to_owned()
                    },
                    str::to_owned,
                ),
            ),
            layout => map_vosk_readiness(layout.into()),
        },
        UiVoskProbe::FullValidation => map_vosk_readiness(inspect()),
    };
    UiVoskReadiness {
        state,
        kind,
        runtime_path,
        model_path,
        message,
    }
}

fn map_vosk_readiness(
    readiness: phorminx_vosk::Readiness,
) -> (UiReadinessState, UiVoskReadinessKind, String) {
    match readiness {
        phorminx_vosk::Readiness::Ready {
            warning: Some(warning),
        } => (
            UiReadinessState::Ready,
            UiVoskReadinessKind::Ready,
            warning.to_owned(),
        ),
        phorminx_vosk::Readiness::Ready { warning: None } => (
            UiReadinessState::Ready,
            UiVoskReadinessKind::Ready,
            "Vosk Instant model ready.".to_owned(),
        ),
        phorminx_vosk::Readiness::MissingRuntime { .. } => (
            UiReadinessState::NeedsAttention,
            UiVoskReadinessKind::MissingRuntime,
            "Vosk runtime bundle not found.".to_owned(),
        ),
        phorminx_vosk::Readiness::MissingModel { .. } => (
            UiReadinessState::NeedsAttention,
            UiVoskReadinessKind::MissingModel,
            "Vosk model directory not found.".to_owned(),
        ),
        phorminx_vosk::Readiness::LoadFailed { .. } => (
            UiReadinessState::NeedsAttention,
            UiVoskReadinessKind::LoadFailed,
            "Vosk runtime could not be loaded.".to_owned(),
        ),
        phorminx_vosk::Readiness::UnsupportedLanguage { .. } => (
            UiReadinessState::NeedsAttention,
            UiVoskReadinessKind::UnsupportedLanguage,
            "Instant mode supports en and pt-br.".to_owned(),
        ),
        phorminx_vosk::Readiness::IncompatibleModelLanguage { .. } => (
            UiReadinessState::NeedsAttention,
            UiVoskReadinessKind::UnsupportedLanguage,
            "The Vosk model name does not verify that it matches the selected language.".to_owned(),
        ),
    }
}

fn microphone_readiness(
    selected: Option<String>,
    devices: Vec<UiMicrophone>,
) -> UiMicrophoneReadiness {
    let selected_available = selected
        .as_ref()
        .is_none_or(|name| devices.iter().any(|device| &device.name == name));
    let (state, message) = if !selected_available {
        (
            UiReadinessState::NeedsAttention,
            "Saved microphone is unavailable. Windows default will be used.".to_owned(),
        )
    } else if devices.is_empty() {
        (
            UiReadinessState::NeedsAttention,
            "No microphone input devices were found.".to_owned(),
        )
    } else if selected.is_some() || devices.iter().any(|device| device.is_default) {
        (UiReadinessState::Ready, "Microphone ready.".to_owned())
    } else {
        (
            UiReadinessState::NeedsAttention,
            "Windows has no default microphone.".to_owned(),
        )
    };
    UiMicrophoneReadiness {
        state,
        devices,
        selected,
        message,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiSettingsSnapshot {
    pub values: Settings,
    pub resolved_model_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiHistoryItem {
    pub id: i64,
    pub created_at_ms: i64,
    pub raw_text: String,
    pub normalized_text: Option<String>,
    pub cleaned_text: Option<String>,
    pub selected_output: String,
    pub language: Option<String>,
    /// Basename only; paths and window titles cannot cross this boundary.
    pub target_executable: Option<String>,
    pub audio_duration_ms: Option<u64>,
    pub stt_duration_ms: Option<u64>,
    pub formatting_duration_ms: Option<u64>,
    pub insertion_duration_ms: Option<u64>,
    pub audio_finalization_duration_ms: Option<u64>,
    pub worker_queue_duration_ms: Option<u64>,
    pub release_to_insert_duration_ms: Option<u64>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiLexiconItem {
    pub id: i64,
    pub canonical: String,
    pub alias: String,
    pub language: Option<String>,
    pub app_executable: Option<String>,
    pub case_policy: CasePolicy,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiProfileItem {
    pub executable: String,
    pub formatting_style: FormattingStyle,
    pub custom_instructions: Option<String>,
    pub language: Option<String>,
    pub insertion_preference: InsertionPreference,
    pub deny: bool,
}

impl UiProfileItem {
    pub fn summary(&self) -> String {
        let formatting = match self.formatting_style {
            FormattingStyle::Raw => "Raw",
            FormattingStyle::Light => "Light",
            FormattingStyle::Balanced => "Balanced",
            FormattingStyle::Strong => "Strong",
            FormattingStyle::Custom => "Custom",
        };
        let language = self.language.as_deref().unwrap_or("Default language");
        let insertion = match self.insertion_preference {
            InsertionPreference::Automatic => "Automatic insertion",
            InsertionPreference::Direct => "Direct insertion",
            InsertionPreference::Clipboard => "Clipboard only",
        };
        if self.deny {
            format!("In {}: Dictation blocked", self.executable)
        } else {
            format!(
                "In {}: {formatting} formatting · {language} · {insertion}",
                self.executable
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiSnapshot {
    pub runtime_status: UiRuntimeStatus,
    pub settings: UiSettingsSnapshot,
    pub readiness: UiReadinessSnapshot,
    pub history_enabled: bool,
    pub history: Vec<UiHistoryItem>,
    pub lexicon: Vec<UiLexiconItem>,
    pub profiles: Vec<UiProfileItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiLexiconDraft {
    pub id: Option<i64>,
    pub canonical: String,
    pub alias: String,
    pub language: Option<String>,
    pub app_executable: Option<String>,
    pub case_policy: CasePolicy,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiProfileDraft {
    /// Executable identity before editing. `None` creates or upserts a profile.
    pub original_executable: Option<String>,
    pub executable: String,
    pub formatting_style: FormattingStyle,
    pub custom_instructions: Option<String>,
    pub language: Option<String>,
    pub insertion_preference: InsertionPreference,
    pub deny: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiCommand {
    SaveSettings(Settings),
    ClearHistory,
    SaveLexicon(UiLexiconDraft),
    DeleteLexicon(i64),
    SetLexiconEnabled { id: i64, enabled: bool },
    SaveProfile(UiProfileDraft),
    DeleteProfile(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiEffect {
    ReloadRuntime,
    ApplyLaunchAtLogin(bool),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiMutation {
    SettingsSaved,
    HistoryCleared { removed: usize },
    LexiconSaved { id: i64 },
    LexiconDeleted { deleted: bool },
    LexiconEnabled { updated: bool },
    ProfileSaved { executable: String },
    ProfileDeleted { deleted: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiCommandOutcome {
    pub mutation: UiMutation,
    pub effects: Vec<UiEffect>,
}

pub struct UiBridge {
    store: SettingsStore,
    persistence: Persistence,
    settings: Settings,
}

impl UiBridge {
    pub fn open(
        store: SettingsStore,
        database_path: impl AsRef<Path>,
    ) -> Result<Self, UiBridgeError> {
        let settings = store.load().map_err(UiBridgeError::settings)?;
        let persistence = Persistence::open(database_path).map_err(UiBridgeError::persistence)?;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        persistence
            .history()
            .set_retention(retention_policy(settings.privacy.history_retention), now_ms)
            .map_err(UiBridgeError::persistence)?;
        Ok(Self {
            store,
            persistence,
            settings,
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn snapshot(
        &self,
        runtime_status: UiRuntimeStatus,
        readiness: UiReadinessSnapshot,
        history_limit: usize,
    ) -> Result<UiSnapshot, UiBridgeError> {
        let history_limit = history_limit.min(MAX_HISTORY_LIMIT);
        let history = self
            .persistence
            .history()
            .recent(history_limit)
            .map_err(UiBridgeError::persistence)?
            .into_iter()
            .map(history_item)
            .collect();
        let lexicon = self
            .persistence
            .lexicon()
            .list()
            .map_err(UiBridgeError::persistence)?
            .into_iter()
            .map(lexicon_item)
            .collect();
        let profiles = self
            .persistence
            .app_profiles()
            .list()
            .map_err(UiBridgeError::persistence)?
            .into_iter()
            .map(profile_item)
            .collect();
        Ok(UiSnapshot {
            runtime_status,
            settings: UiSettingsSnapshot {
                values: self.settings.clone(),
                resolved_model_path: self
                    .store
                    .resolve_model_path(&self.settings.recognition.model_path),
            },
            readiness,
            history_enabled: self.settings.privacy.history_retention != HistoryRetention::Disabled,
            history,
            lexicon,
            profiles,
        })
    }

    pub fn execute(
        &mut self,
        command: UiCommand,
        readiness: &UiReadinessSnapshot,
        now_ms: i64,
    ) -> Result<UiCommandOutcome, UiBridgeError> {
        match command {
            UiCommand::SaveSettings(candidate) => self.save_settings(candidate, readiness, now_ms),
            UiCommand::ClearHistory => {
                let removed = self
                    .persistence
                    .history()
                    .clear()
                    .map_err(UiBridgeError::persistence)?;
                Ok(outcome(UiMutation::HistoryCleared { removed }))
            }
            UiCommand::SaveLexicon(draft) => {
                let (id, entry) = lexicon_draft(draft)?;
                let id = if let Some(id) = id {
                    let updated = self
                        .persistence
                        .lexicon()
                        .update(id, &entry)
                        .map_err(UiBridgeError::persistence)?;
                    if !updated {
                        return Err(UiBridgeError::not_found("lexicon_entry"));
                    }
                    id
                } else {
                    self.persistence
                        .lexicon()
                        .insert(&entry)
                        .map_err(UiBridgeError::persistence)?
                };
                Ok(UiCommandOutcome {
                    mutation: UiMutation::LexiconSaved { id },
                    effects: vec![UiEffect::ReloadRuntime],
                })
            }
            UiCommand::DeleteLexicon(id) => {
                let deleted = self
                    .persistence
                    .lexicon()
                    .delete(id)
                    .map_err(UiBridgeError::persistence)?;
                Ok(UiCommandOutcome {
                    mutation: UiMutation::LexiconDeleted { deleted },
                    effects: deleted
                        .then_some(UiEffect::ReloadRuntime)
                        .into_iter()
                        .collect(),
                })
            }
            UiCommand::SetLexiconEnabled { id, enabled } => {
                let updated = self
                    .persistence
                    .lexicon()
                    .set_enabled(id, enabled)
                    .map_err(UiBridgeError::persistence)?;
                Ok(UiCommandOutcome {
                    mutation: UiMutation::LexiconEnabled { updated },
                    effects: updated
                        .then_some(UiEffect::ReloadRuntime)
                        .into_iter()
                        .collect(),
                })
            }
            UiCommand::SaveProfile(draft) => {
                let (original, profile) = profile_draft(draft)?;
                let resolved_model = self
                    .store
                    .resolve_model_path(&self.settings.recognition.model_path);
                let verified_variant = identify_pinned_model(&resolved_model).map_err(|_| {
                    UiBridgeError::validation(
                        "language",
                        "The active Whisper model could not be verified for this language.",
                    )
                })?;
                validate_profile_model_language(verified_variant, profile.language.as_deref())?;
                let executable = profile.executable.to_string();
                let profiles = self.persistence.app_profiles();
                if let Some(original) = original {
                    if !profiles
                        .replace(&original, &profile)
                        .map_err(UiBridgeError::persistence)?
                    {
                        return Err(UiBridgeError::not_found("profile"));
                    }
                } else {
                    profiles
                        .upsert(&profile)
                        .map_err(UiBridgeError::persistence)?;
                }
                Ok(UiCommandOutcome {
                    mutation: UiMutation::ProfileSaved { executable },
                    effects: vec![UiEffect::ReloadRuntime],
                })
            }
            UiCommand::DeleteProfile(executable) => {
                let executable = ExecutableIdentity::new(executable.trim())
                    .map_err(|_| UiBridgeError::validation("executable", "Use a basename only."))?;
                let deleted = self
                    .persistence
                    .app_profiles()
                    .delete(&executable)
                    .map_err(UiBridgeError::persistence)?;
                Ok(UiCommandOutcome {
                    mutation: UiMutation::ProfileDeleted { deleted },
                    effects: deleted
                        .then_some(UiEffect::ReloadRuntime)
                        .into_iter()
                        .collect(),
                })
            }
        }
    }

    fn save_settings(
        &mut self,
        mut candidate: Settings,
        readiness: &UiReadinessSnapshot,
        now_ms: i64,
    ) -> Result<UiCommandOutcome, UiBridgeError> {
        candidate
            .validate_and_normalize()
            .map_err(UiBridgeError::settings)?;
        candidate
            .ensure_runtime_supported()
            .map_err(UiBridgeError::settings)?;
        match candidate.recognition.mode {
            crate::settings::RecognitionMode::Accurate => {
                let resolved_model = self
                    .store
                    .resolve_model_path(&candidate.recognition.model_path);
                if !resolved_model.is_file() {
                    return Err(UiBridgeError::validation(
                        "model_path",
                        "Select an existing Whisper model file.",
                    ));
                }
                let verified_variant = identify_pinned_model(&resolved_model).map_err(|_| {
                    UiBridgeError::validation(
                        "model_path",
                        "The Whisper model could not be verified.",
                    )
                })?;
                if candidate.recognition.accurate_model != AccurateModelVariant::Custom {
                    model_for_variant(candidate.recognition.accurate_model).map_err(|_| {
                        UiBridgeError::validation(
                            "model_path",
                            "The pinned Whisper model is unavailable.",
                        )
                    })?;
                    if verified_variant != Some(candidate.recognition.accurate_model) {
                        return Err(UiBridgeError::validation(
                            "model_path",
                            "Download and verify the selected pinned Whisper model before switching.",
                        ));
                    }
                }
                if verified_variant.is_some_and(|variant| {
                    !variant.supports_language(&candidate.recognition.language)
                }) {
                    return Err(UiBridgeError::validation(
                        "language",
                        "Select a multilingual Whisper model for this language.",
                    ));
                }
            }
            crate::settings::RecognitionMode::Instant => {
                let runtime = self
                    .store
                    .resolve_asset_path(&candidate.recognition.instant_runtime_path);
                let model = self
                    .store
                    .resolve_asset_path(&candidate.recognition.instant_model_path);
                match phorminx_vosk::inspect(&runtime, &model, &candidate.recognition.language) {
                    phorminx_vosk::Readiness::Ready { .. } => {}
                    phorminx_vosk::Readiness::MissingRuntime { .. } => {
                        return Err(UiBridgeError::validation(
                            "instant_runtime_path",
                            "Select a local Vosk runtime bundle containing libvosk.dll.",
                        ));
                    }
                    phorminx_vosk::Readiness::MissingModel { .. } => {
                        return Err(UiBridgeError::validation(
                            "instant_model_path",
                            "Select an unpacked local Vosk model directory.",
                        ));
                    }
                    phorminx_vosk::Readiness::UnsupportedLanguage { .. } => {
                        return Err(UiBridgeError::validation(
                            "language",
                            "Instant mode currently supports en and pt-br.",
                        ));
                    }
                    phorminx_vosk::Readiness::IncompatibleModelLanguage { .. } => {
                        return Err(UiBridgeError::validation(
                            "instant_model_path",
                            "Choose a Vosk model whose official directory name matches the selected language.",
                        ));
                    }
                    phorminx_vosk::Readiness::LoadFailed { .. } => {
                        return Err(UiBridgeError::validation(
                            "instant_runtime_path",
                            "The selected Vosk runtime could not be loaded.",
                        ));
                    }
                }
            }
        }
        if requires_ollama(candidate.formatting.strength) {
            let selected = candidate
                .formatting
                .ollama_model
                .as_deref()
                .ok_or_else(|| {
                    UiBridgeError::validation("ollama_model", "Select an installed Ollama model.")
                })?;
            if !readiness.has_installed_ollama_model(selected) {
                return Err(UiBridgeError::validation(
                    "ollama_model",
                    "The selected Ollama model is not currently installed.",
                ));
            }
        }

        let old_settings = self.settings.clone();
        self.store
            .save(&candidate)
            .map_err(UiBridgeError::settings)?;
        if self.settings.privacy.history_retention != candidate.privacy.history_retention
            && let Err(error) = self.persistence.history().set_retention(
                retention_policy(candidate.privacy.history_retention),
                now_ms,
            )
        {
            if self.store.save(&old_settings).is_err() {
                return Err(UiBridgeError::consistency());
            }
            return Err(UiBridgeError::persistence(error));
        }

        let launch_changed =
            self.settings.startup.launch_at_login != candidate.startup.launch_at_login;
        let launch_value = candidate.startup.launch_at_login;
        self.settings = candidate;
        let mut effects = vec![UiEffect::ReloadRuntime];
        if launch_changed {
            effects.push(UiEffect::ApplyLaunchAtLogin(launch_value));
        }
        Ok(UiCommandOutcome {
            mutation: UiMutation::SettingsSaved,
            effects,
        })
    }
}

fn requires_ollama(strength: FormattingStrength) -> bool {
    matches!(
        strength,
        FormattingStrength::Balanced | FormattingStrength::Strong | FormattingStrength::Custom
    )
}

fn validate_profile_model_language(
    verified_variant: Option<AccurateModelVariant>,
    language: Option<&str>,
) -> Result<(), UiBridgeError> {
    if language.is_some_and(|language| {
        verified_variant.is_some_and(|variant| !variant.supports_language(language))
    }) {
        return Err(UiBridgeError::validation(
            "language",
            "Select a multilingual Whisper model before using this profile language.",
        ));
    }
    Ok(())
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

fn outcome(mutation: UiMutation) -> UiCommandOutcome {
    UiCommandOutcome {
        mutation,
        effects: Vec::new(),
    }
}

fn history_item(record: DictationRecord) -> UiHistoryItem {
    let draft = record.dictation;
    UiHistoryItem {
        id: record.id,
        created_at_ms: draft.created_at_ms,
        raw_text: draft.raw_text,
        normalized_text: draft.normalized_text,
        cleaned_text: draft.cleaned_text,
        selected_output: draft.selected_output,
        language: draft.language,
        // Defense in depth for databases created by an older build.
        target_executable: draft
            .target_executable
            .filter(|value| ExecutableIdentity::new(value.clone()).is_ok()),
        audio_duration_ms: draft.timings.audio_duration_ms,
        stt_duration_ms: draft.timings.stt_duration_ms,
        formatting_duration_ms: draft.timings.formatting_duration_ms,
        insertion_duration_ms: draft.timings.insertion_duration_ms,
        audio_finalization_duration_ms: draft.timings.audio_finalization_duration_ms,
        worker_queue_duration_ms: draft.timings.worker_queue_duration_ms,
        release_to_insert_duration_ms: draft.timings.release_to_insert_duration_ms,
        warnings: draft.warnings,
    }
}

fn lexicon_item(entry: LexiconEntry) -> UiLexiconItem {
    UiLexiconItem {
        id: entry.id,
        canonical: entry.entry.canonical,
        alias: entry.entry.alias,
        language: entry.entry.language,
        app_executable: entry.entry.app_executable,
        case_policy: entry.entry.case_policy,
        enabled: entry.entry.enabled,
    }
}

fn profile_item(profile: AppProfile) -> UiProfileItem {
    UiProfileItem {
        executable: profile.executable.to_string(),
        formatting_style: profile.formatting_style,
        custom_instructions: profile.custom_instructions,
        language: profile.language,
        insertion_preference: profile.insertion_preference,
        deny: profile.deny,
    }
}

fn lexicon_draft(draft: UiLexiconDraft) -> Result<(Option<i64>, NewLexiconEntry), UiBridgeError> {
    let canonical = draft.canonical.trim();
    if canonical.is_empty() {
        return Err(UiBridgeError::validation(
            "canonical",
            "Written form is required.",
        ));
    }
    let alias = draft.alias.trim();
    if alias.is_empty() {
        return Err(UiBridgeError::validation(
            "alias",
            "Spoken alias is required.",
        ));
    }
    let app_executable = normalize_executable_scope(draft.app_executable)?;
    let language = normalize_optional_language(draft.language)?;
    Ok((
        draft.id,
        NewLexiconEntry {
            canonical: canonical.to_owned(),
            alias: alias.to_owned(),
            language,
            app_executable,
            case_policy: draft.case_policy,
            enabled: draft.enabled,
        },
    ))
}

fn profile_draft(
    draft: UiProfileDraft,
) -> Result<(Option<ExecutableIdentity>, AppProfile), UiBridgeError> {
    let original = draft
        .original_executable
        .map(|value| {
            ExecutableIdentity::new(value.trim())
                .map_err(|_| UiBridgeError::validation("executable", "Use a basename only."))
        })
        .transpose()?;
    let executable = ExecutableIdentity::new(draft.executable.trim())
        .map_err(|_| UiBridgeError::validation("executable", "Use a basename only."))?;
    let language = normalize_optional_language(draft.language)?;
    let custom_instructions = draft
        .custom_instructions
        .and_then(|value| (!value.trim().is_empty()).then_some(value));
    if draft.formatting_style == FormattingStyle::Custom && custom_instructions.is_none() {
        return Err(UiBridgeError::validation(
            "custom_instructions",
            "Custom formatting requires instructions.",
        ));
    }
    Ok((
        original,
        AppProfile {
            executable,
            formatting_style: draft.formatting_style,
            custom_instructions,
            language,
            insertion_preference: draft.insertion_preference,
            deny: draft.deny,
        },
    ))
}

fn normalize_executable_scope(value: Option<String>) -> Result<Option<String>, UiBridgeError> {
    value
        .and_then(|value| {
            let trimmed = value.trim().to_owned();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .map(|value| {
            ExecutableIdentity::new(value)
                .map(|identity| identity.to_string())
                .map_err(|_| UiBridgeError::validation("executable", "Use a basename only."))
        })
        .transpose()
}

fn normalize_optional_language(value: Option<String>) -> Result<Option<String>, UiBridgeError> {
    value
        .and_then(|value| (!value.trim().is_empty()).then_some(value))
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            if (2..=16).contains(&normalized.len())
                && normalized
                    .bytes()
                    .all(|character| character.is_ascii_alphabetic() || character == b'-')
            {
                Ok(normalized)
            } else {
                Err(UiBridgeError::validation(
                    "language",
                    "Use a language tag such as en or pt-br.",
                ))
            }
        })
        .transpose()
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct UiBridgeError {
    pub code: &'static str,
    pub field: Option<&'static str>,
    pub message: &'static str,
}

impl UiBridgeError {
    fn validation(field: &'static str, message: &'static str) -> Self {
        Self {
            code: "validation",
            field: Some(field),
            message,
        }
    }

    fn not_found(field: &'static str) -> Self {
        Self {
            code: "not_found",
            field: Some(field),
            message: "The item no longer exists.",
        }
    }

    fn settings(error: SettingsError) -> Self {
        let (field, message) = match error {
            SettingsError::EmptyModelPath => (Some("model_path"), "Select a Whisper model."),
            SettingsError::InvalidMinimumRms(_) => {
                (Some("minimum_rms"), "Use a speech level between 0 and 1.")
            }
            SettingsError::InvalidLanguage(_) => {
                (Some("language"), "Use a language tag such as en or pt-br.")
            }
            SettingsError::MicrophoneNameTooLong => {
                (Some("microphone"), "The microphone name is too long.")
            }
            SettingsError::CustomInstructionsTooLong => (
                Some("custom_instructions"),
                "Custom instructions are too long.",
            ),
            SettingsError::MissingCustomInstructions => (
                Some("custom_instructions"),
                "Custom formatting requires instructions.",
            ),
            SettingsError::OllamaModelNameTooLong => {
                (Some("ollama_model"), "The model name is too long.")
            }
            SettingsError::FormattingModelRequired(_) => (
                Some("ollama_model"),
                "This formatting strength requires an Ollama model.",
            ),
            _ => (None, "Settings could not be saved."),
        };
        Self {
            code: "settings",
            field,
            message,
        }
    }

    fn persistence(_error: phorminx_persistence::PersistenceError) -> Self {
        Self {
            code: "local_data",
            field: None,
            message: "Local data could not be updated.",
        }
    }

    fn consistency() -> Self {
        Self {
            code: "settings_consistency",
            field: None,
            message: "Settings need attention before Phorminx can continue.",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use phorminx_persistence::{DictationDraft, TimingMetadata};

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn resident_and_layout_readiness_never_invoke_the_full_vosk_loader() {
        for (probe, expected_full, expected_layout) in [
            (UiVoskProbe::ResidentReady, 0, 0),
            (UiVoskProbe::LayoutOnly, 0, 1),
            (UiVoskProbe::FullValidation, 1, 0),
        ] {
            let full_loads = Cell::new(0);
            let layout_checks = Cell::new(0);
            let readiness = vosk_readiness_with(
                PathBuf::from("runtime"),
                PathBuf::from("vosk-model-small-en-us-0.15"),
                "en",
                probe,
                || {
                    full_loads.set(full_loads.get() + 1);
                    phorminx_vosk::Readiness::Ready { warning: None }
                },
                || {
                    layout_checks.set(layout_checks.get() + 1);
                    phorminx_vosk::AssetLayout::Present { warning: None }
                },
            );
            let expected_state = if probe == UiVoskProbe::LayoutOnly {
                UiReadinessState::NeedsAttention
            } else {
                UiReadinessState::Ready
            };
            assert_eq!(readiness.state, expected_state);
            if probe == UiVoskProbe::LayoutOnly {
                assert_eq!(readiness.kind, UiVoskReadinessKind::InstalledUnvalidated);
            }
            assert_eq!(full_loads.get(), expected_full);
            assert_eq!(layout_checks.get(), expected_layout);
        }

        let failed = vosk_readiness_with(
            PathBuf::from("runtime"),
            PathBuf::from("vosk-model-small-en-us-0.15"),
            "en",
            UiVoskProbe::FullValidation,
            || phorminx_vosk::Readiness::LoadFailed {
                component: "runtime_or_model",
            },
            || unreachable!("full validation must not fall back to layout readiness"),
        );
        assert_eq!(failed.state, UiReadinessState::NeedsAttention);
        assert_eq!(failed.kind, UiVoskReadinessKind::LoadFailed);
    }

    struct TestBridge {
        root: PathBuf,
        bridge: UiBridge,
    }

    impl TestBridge {
        fn new() -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "phorminx-ui-bridge-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            let model = root.join("model.bin");
            fs::write(&model, b"model").unwrap();
            let store = SettingsStore::new(root.join("settings.toml")).unwrap();
            let mut settings = Settings::default();
            settings.recognition.model_path = model;
            settings.recognition.accurate_model = AccurateModelVariant::Custom;
            store.save(&settings).unwrap();
            let bridge = UiBridge::open(store, root.join("phorminx.sqlite3")).unwrap();
            Self { root, bridge }
        }

        fn readiness(&self) -> UiReadinessSnapshot {
            ready(&self.bridge)
        }
    }

    impl Drop for TestBridge {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn ready(bridge: &UiBridge) -> UiReadinessSnapshot {
        let store = &bridge.store;
        let settings = bridge.settings();
        UiReadinessSnapshot {
            microphone: microphone_readiness(
                None,
                vec![UiMicrophone {
                    name: "Test microphone".to_owned(),
                    is_default: true,
                }],
            ),
            whisper: UiWhisperReadiness {
                state: UiReadinessState::Ready,
                configured_path: store.resolve_model_path(&settings.recognition.model_path),
                size_bytes: Some(5),
                language: "en".to_owned(),
                selected_backend: Some("cpu".to_owned()),
                device_name: None,
                message: "Whisper model available; cpu backend selected.".to_owned(),
            },
            vosk: UiVoskReadiness {
                state: UiReadinessState::NeedsAttention,
                kind: UiVoskReadinessKind::MissingRuntime,
                runtime_path: store.resolve_asset_path(&settings.recognition.instant_runtime_path),
                model_path: store.resolve_asset_path(&settings.recognition.instant_model_path),
                message: "Vosk runtime bundle not found.".to_owned(),
            },
            ollama: UiOllamaReadiness {
                state: UiReadinessState::Ready,
                models: vec![UiOllamaModel {
                    name: "qwen2.5:3b".to_owned(),
                    size_bytes: Some(1_000),
                    family: Some("qwen".to_owned()),
                }],
                selected: None,
                message: "Ollama available; warm-up is independent.".to_owned(),
            },
        }
    }

    #[test]
    fn checking_snapshot_is_content_free_and_resolves_the_model() {
        let test = TestBridge::new();
        let readiness = UiReadinessSnapshot::checking(test.bridge.settings(), &test.bridge.store);
        assert_eq!(readiness.microphone.state, UiReadinessState::Checking);
        assert_eq!(readiness.ollama.message, "Checking Ollama.");
        assert!(readiness.whisper.configured_path.ends_with("model.bin"));
    }

    #[test]
    fn microphone_readiness_handles_default_selected_and_missing_devices() {
        let devices = vec![UiMicrophone {
            name: "Desk mic".to_owned(),
            is_default: true,
        }];
        assert_eq!(
            microphone_readiness(None, devices.clone()).state,
            UiReadinessState::Ready
        );
        assert_eq!(
            microphone_readiness(Some("Desk mic".to_owned()), devices.clone()).state,
            UiReadinessState::Ready
        );
        assert_eq!(
            microphone_readiness(Some("Gone".to_owned()), devices).state,
            UiReadinessState::NeedsAttention
        );
        assert_eq!(
            microphone_readiness(None, Vec::new()).state,
            UiReadinessState::NeedsAttention
        );
    }

    #[test]
    fn english_only_verified_model_rejects_portuguese_profile_language() {
        let error =
            validate_profile_model_language(Some(AccurateModelVariant::BaseEnglish), Some("pt-br"))
                .unwrap_err();
        assert_eq!(error.field, Some("language"));
        assert!(
            validate_profile_model_language(
                Some(AccurateModelVariant::BaseMultilingual),
                Some("pt-br")
            )
            .is_ok()
        );
    }

    #[test]
    fn loaded_cpu_fallback_is_authoritative_over_vulkan_availability() {
        let test = TestBridge::new();
        let loaded = WhisperReadiness {
            requested: WhisperBackendPreference::Auto,
            backend: WhisperBackend::Cpu,
            device_name: None,
            fallback_from: Some(WhisperBackend::Vulkan),
            model_load_time: std::time::Duration::from_millis(12),
        };

        let readiness =
            whisper_readiness(test.bridge.settings(), &test.bridge.store, Some(&loaded));

        assert_eq!(readiness.state, UiReadinessState::Ready);
        assert_eq!(readiness.selected_backend.as_deref(), Some("cpu"));
        assert_eq!(readiness.device_name, None);
        assert!(readiness.message.contains("after vulkan fallback"));
    }

    #[test]
    fn explicit_vulkan_without_a_loaded_context_is_never_ready() {
        let mut test = TestBridge::new();
        test.bridge.settings.recognition.accurate_backend =
            crate::settings::AccurateBackendPreference::Vulkan;

        let readiness = whisper_readiness(test.bridge.settings(), &test.bridge.store, None);

        assert_eq!(readiness.state, UiReadinessState::NeedsAttention);
        assert_eq!(readiness.selected_backend, None);
        assert_eq!(readiness.device_name, None);
    }

    #[test]
    fn settings_save_normalizes_and_returns_external_effects() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        let mut candidate = test.bridge.settings().clone();
        candidate.recognition.language = "PT-BR".to_owned();
        candidate.startup.launch_at_login = true;

        let result = test
            .bridge
            .execute(UiCommand::SaveSettings(candidate), &readiness, 10)
            .unwrap();

        assert_eq!(test.bridge.settings().recognition.language, "pt-br");
        assert_eq!(
            result.effects,
            [UiEffect::ReloadRuntime, UiEffect::ApplyLaunchAtLogin(true)]
        );
    }

    #[test]
    fn settings_reject_missing_whisper_and_unverified_ollama_models() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        let mut missing_whisper = test.bridge.settings().clone();
        missing_whisper.recognition.model_path = test.root.join("missing.bin");
        let error = test
            .bridge
            .execute(UiCommand::SaveSettings(missing_whisper), &readiness, 10)
            .unwrap_err();
        assert_eq!(error.field, Some("model_path"));

        let mut instant_without_whisper = test.bridge.settings().clone();
        instant_without_whisper.recognition.mode = crate::settings::RecognitionMode::Instant;
        instant_without_whisper.recognition.model_path = test.root.join("also-missing.bin");
        let error = test
            .bridge
            .execute(
                UiCommand::SaveSettings(instant_without_whisper),
                &readiness,
                11,
            )
            .unwrap_err();
        assert_eq!(error.field, Some("instant_runtime_path"));

        let mut missing_ollama = test.bridge.settings().clone();
        missing_ollama.formatting.strength = FormattingStrength::Strong;
        missing_ollama.formatting.ollama_model = Some("missing:7b".to_owned());
        let error = test
            .bridge
            .execute(UiCommand::SaveSettings(missing_ollama), &readiness, 10)
            .unwrap_err();
        assert_eq!(error.field, Some("ollama_model"));
    }

    #[test]
    fn disabling_history_clears_existing_records() {
        let mut test = TestBridge::new();
        test.bridge
            .persistence
            .history()
            .insert(&DictationDraft {
                created_at_ms: 1,
                raw_text: "raw".to_owned(),
                normalized_text: None,
                cleaned_text: None,
                selected_output: "out".to_owned(),
                language: Some("en".to_owned()),
                target_executable: Some("code.exe".to_owned()),
                timings: TimingMetadata::default(),
                warnings: Vec::new(),
            })
            .unwrap();
        let readiness = test.readiness();
        let mut candidate = test.bridge.settings().clone();
        candidate.privacy.history_retention = HistoryRetention::Disabled;
        test.bridge
            .execute(UiCommand::SaveSettings(candidate), &readiness, 100)
            .unwrap();
        assert_eq!(test.bridge.persistence.history().count().unwrap(), 0);
    }

    #[test]
    fn lexicon_commands_round_trip_and_reload_runtime() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        let saved = test
            .bridge
            .execute(
                UiCommand::SaveLexicon(UiLexiconDraft {
                    id: None,
                    canonical: "Phorminx".to_owned(),
                    alias: "form inks".to_owned(),
                    language: Some("PT-BR".to_owned()),
                    app_executable: Some("code.exe".to_owned()),
                    case_policy: CasePolicy::UseCanonical,
                    enabled: true,
                }),
                &readiness,
                0,
            )
            .unwrap();
        let UiMutation::LexiconSaved { id } = saved.mutation else {
            panic!("unexpected mutation")
        };
        assert_eq!(saved.effects, [UiEffect::ReloadRuntime]);
        let entry = test.bridge.persistence.lexicon().get(id).unwrap().unwrap();
        assert_eq!(entry.entry.language.as_deref(), Some("pt-br"));

        let disabled = test
            .bridge
            .execute(
                UiCommand::SetLexiconEnabled { id, enabled: false },
                &readiness,
                0,
            )
            .unwrap();
        assert_eq!(
            disabled.mutation,
            UiMutation::LexiconEnabled { updated: true }
        );
        assert!(
            !test
                .bridge
                .persistence
                .lexicon()
                .get(id)
                .unwrap()
                .unwrap()
                .entry
                .enabled
        );
    }

    #[test]
    fn lexicon_rejects_path_scopes_before_persistence() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        let error = test
            .bridge
            .execute(
                UiCommand::SaveLexicon(UiLexiconDraft {
                    id: None,
                    canonical: "Name".to_owned(),
                    alias: "name".to_owned(),
                    language: None,
                    app_executable: Some(r"C:\private\code.exe".to_owned()),
                    case_policy: CasePolicy::UseCanonical,
                    enabled: true,
                }),
                &readiness,
                0,
            )
            .unwrap_err();
        assert_eq!(error.field, Some("executable"));
        assert_eq!(test.bridge.persistence.lexicon().list().unwrap(), []);
    }

    #[test]
    fn profiles_round_trip_summarize_and_reject_paths() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        let draft = UiProfileDraft {
            original_executable: None,
            executable: "code.exe".to_owned(),
            formatting_style: FormattingStyle::Light,
            custom_instructions: None,
            language: Some("EN".to_owned()),
            insertion_preference: InsertionPreference::Clipboard,
            deny: false,
        };
        test.bridge
            .execute(UiCommand::SaveProfile(draft), &readiness, 0)
            .unwrap();
        let snapshot = test
            .bridge
            .snapshot(UiRuntimeStatus::Ready, readiness.clone(), 10)
            .unwrap();
        assert_eq!(snapshot.profiles.len(), 1);
        assert_eq!(
            snapshot.profiles[0].summary(),
            "In code.exe: Light formatting · en · Clipboard only"
        );

        let error = test
            .bridge
            .execute(
                UiCommand::DeleteProfile(r"C:\private\code.exe".to_owned()),
                &readiness,
                0,
            )
            .unwrap_err();
        assert_eq!(error.field, Some("executable"));
    }

    #[test]
    fn profile_save_atomically_handles_case_only_renames() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        test.bridge
            .execute(
                UiCommand::SaveProfile(UiProfileDraft {
                    original_executable: None,
                    executable: "Code.exe".to_owned(),
                    formatting_style: FormattingStyle::Light,
                    custom_instructions: None,
                    language: None,
                    insertion_preference: InsertionPreference::Automatic,
                    deny: false,
                }),
                &readiness,
                0,
            )
            .unwrap();

        test.bridge
            .execute(
                UiCommand::SaveProfile(UiProfileDraft {
                    original_executable: Some("Code.exe".to_owned()),
                    executable: "code.EXE".to_owned(),
                    formatting_style: FormattingStyle::Strong,
                    custom_instructions: None,
                    language: Some("PT-BR".to_owned()),
                    insertion_preference: InsertionPreference::Clipboard,
                    deny: false,
                }),
                &readiness,
                0,
            )
            .unwrap();

        let profiles = test.bridge.persistence.app_profiles().list().unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].executable.as_str(), "code.EXE");
        assert_eq!(profiles[0].formatting_style, FormattingStyle::Strong);
        assert_eq!(profiles[0].language.as_deref(), Some("pt-br"));
    }

    #[test]
    fn profile_save_rolls_back_when_renamed_identity_is_occupied() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        for executable in ["code.exe", "notes.exe"] {
            test.bridge
                .execute(
                    UiCommand::SaveProfile(UiProfileDraft {
                        original_executable: None,
                        executable: executable.to_owned(),
                        formatting_style: FormattingStyle::Light,
                        custom_instructions: None,
                        language: None,
                        insertion_preference: InsertionPreference::Automatic,
                        deny: false,
                    }),
                    &readiness,
                    0,
                )
                .unwrap();
        }

        let error = test
            .bridge
            .execute(
                UiCommand::SaveProfile(UiProfileDraft {
                    original_executable: Some("code.exe".to_owned()),
                    executable: "NOTES.EXE".to_owned(),
                    formatting_style: FormattingStyle::Strong,
                    custom_instructions: None,
                    language: None,
                    insertion_preference: InsertionPreference::Clipboard,
                    deny: false,
                }),
                &readiness,
                0,
            )
            .unwrap_err();
        assert_eq!(error.code, "local_data");

        let profiles = test.bridge.persistence.app_profiles().list().unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].executable.as_str(), "code.exe");
        assert_eq!(profiles[0].formatting_style, FormattingStyle::Light);
        assert_eq!(profiles[1].executable.as_str(), "notes.exe");
    }

    #[test]
    fn profile_save_rejects_invalid_original_before_mutating_data() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        test.bridge
            .execute(
                UiCommand::SaveProfile(UiProfileDraft {
                    original_executable: None,
                    executable: "code.exe".to_owned(),
                    formatting_style: FormattingStyle::Light,
                    custom_instructions: None,
                    language: None,
                    insertion_preference: InsertionPreference::Automatic,
                    deny: false,
                }),
                &readiness,
                0,
            )
            .unwrap();

        let error = test
            .bridge
            .execute(
                UiCommand::SaveProfile(UiProfileDraft {
                    original_executable: Some(r"C:\private\code.exe".to_owned()),
                    executable: "renamed.exe".to_owned(),
                    formatting_style: FormattingStyle::Strong,
                    custom_instructions: None,
                    language: None,
                    insertion_preference: InsertionPreference::Automatic,
                    deny: false,
                }),
                &readiness,
                0,
            )
            .unwrap_err();
        assert_eq!(error.field, Some("executable"));

        let profiles = test.bridge.persistence.app_profiles().list().unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].executable.as_str(), "code.exe");
    }

    #[test]
    fn custom_profiles_require_instructions() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        let error = test
            .bridge
            .execute(
                UiCommand::SaveProfile(UiProfileDraft {
                    original_executable: None,
                    executable: "notes.exe".to_owned(),
                    formatting_style: FormattingStyle::Custom,
                    custom_instructions: Some("  ".to_owned()),
                    language: None,
                    insertion_preference: InsertionPreference::Automatic,
                    deny: false,
                }),
                &readiness,
                0,
            )
            .unwrap_err();
        assert_eq!(error.field, Some("custom_instructions"));
    }

    #[test]
    fn snapshot_maps_history_variants_and_all_workspaces() {
        let test = TestBridge::new();
        test.bridge
            .persistence
            .history()
            .insert(&DictationDraft {
                created_at_ms: 42,
                raw_text: "raw".to_owned(),
                normalized_text: Some("normalized".to_owned()),
                cleaned_text: Some("cleaned".to_owned()),
                selected_output: "selected".to_owned(),
                language: Some("en".to_owned()),
                target_executable: Some("code.exe".to_owned()),
                timings: TimingMetadata {
                    audio_duration_ms: Some(1),
                    stt_duration_ms: Some(2),
                    formatting_duration_ms: Some(3),
                    insertion_duration_ms: Some(4),
                    audio_finalization_duration_ms: Some(5),
                    worker_queue_duration_ms: Some(6),
                    release_to_insert_duration_ms: Some(7),
                },
                warnings: vec!["clipboard_fallback".to_owned()],
            })
            .unwrap();
        let readiness = test.readiness();
        let snapshot = test
            .bridge
            .snapshot(UiRuntimeStatus::Refining, readiness, DEFAULT_HISTORY_LIMIT)
            .unwrap();
        assert_eq!(snapshot.runtime_status, UiRuntimeStatus::Refining);
        assert_eq!(snapshot.history.len(), 1);
        assert_eq!(snapshot.history[0].cleaned_text.as_deref(), Some("cleaned"));
        assert_eq!(
            snapshot.history[0].target_executable.as_deref(),
            Some("code.exe")
        );
        assert_eq!(snapshot.history[0].formatting_duration_ms, Some(3));
    }

    #[test]
    fn clear_and_missing_delete_commands_are_idempotent() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        let clear = test
            .bridge
            .execute(UiCommand::ClearHistory, &readiness, 0)
            .unwrap();
        assert_eq!(clear.mutation, UiMutation::HistoryCleared { removed: 0 });
        let delete = test
            .bridge
            .execute(UiCommand::DeleteLexicon(999), &readiness, 0)
            .unwrap();
        assert_eq!(
            delete.mutation,
            UiMutation::LexiconDeleted { deleted: false }
        );
        assert!(delete.effects.is_empty());
    }

    #[test]
    fn bridge_errors_never_embed_private_input_or_paths() {
        let mut test = TestBridge::new();
        let readiness = test.readiness();
        let private_path = r"C:\Users\Someone\Secret\app.exe";
        let error = test
            .bridge
            .execute(
                UiCommand::DeleteProfile(private_path.to_owned()),
                &readiness,
                0,
            )
            .unwrap_err();
        assert!(!error.to_string().contains("Secret"));
        assert_eq!(error.message, "Use a basename only.");
    }
}

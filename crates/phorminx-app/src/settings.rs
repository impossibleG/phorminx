use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use phorminx_windows::{
    DEFAULT_LAUNCHER_SHORTCUT, ShortcutBindings, ShortcutError, atomic_replace_file,
};
use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: u32 = 7;
pub const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
pub const MAX_CUSTOM_INSTRUCTIONS_CHARS: usize = 4_096;

static SAVE_LOCK: Mutex<()> = Mutex::new(());
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    #[serde(default)]
    pub recognition: RecognitionSettings,
    #[serde(default)]
    pub formatting: FormattingSettings,
    #[serde(default)]
    pub interaction: InteractionSettings,
    #[serde(default)]
    pub privacy: PrivacySettings,
    #[serde(default)]
    pub startup: StartupSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub search: SearchSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            recognition: RecognitionSettings::default(),
            formatting: FormattingSettings::default(),
            interaction: InteractionSettings::default(),
            privacy: PrivacySettings::default(),
            startup: StartupSettings::default(),
            appearance: AppearanceSettings::default(),
            search: SearchSettings::default(),
        }
    }
}

impl Settings {
    pub fn validate_and_normalize(&mut self) -> Result<(), SettingsError> {
        match self.schema_version {
            CURRENT_SCHEMA_VERSION => {}
            1..=6 => self.schema_version = CURRENT_SCHEMA_VERSION,
            0 => return Err(SettingsError::MissingOrInvalidVersion),
            version if version > CURRENT_SCHEMA_VERSION => {
                return Err(SettingsError::FutureVersion {
                    found: version,
                    supported: CURRENT_SCHEMA_VERSION,
                });
            }
            version => return Err(SettingsError::UnsupportedOldVersion(version)),
        }

        let bindings = ShortcutBindings::parse(
            &self.interaction.launcher_shortcut,
            self.interaction.direct_dictation_shortcut.as_deref(),
        )
        .map_err(SettingsError::InvalidShortcut)?;
        self.interaction.launcher_shortcut = bindings.launcher.to_string();
        self.interaction.direct_dictation_shortcut = bindings
            .direct_dictation
            .map(|shortcut| shortcut.to_string());
        if let Some(model) = &self.search.embedding_model {
            let normalized = model.trim();
            if normalized.is_empty() {
                self.search.embedding_model = None;
            } else if normalized.len() > 256 || normalized.chars().any(char::is_control) {
                return Err(SettingsError::InvalidEmbeddingModel);
            } else {
                self.search.embedding_model = Some(normalized.to_owned());
            }
        }

        if self.recognition.model_path.as_os_str().is_empty() {
            return Err(SettingsError::EmptyModelPath);
        }
        if self.recognition.instant_model_path.as_os_str().is_empty() {
            return Err(SettingsError::EmptyInstantModelPath);
        }
        if self.recognition.instant_runtime_path.as_os_str().is_empty() {
            return Err(SettingsError::EmptyInstantRuntimePath);
        }
        if !self.recognition.minimum_rms.is_finite()
            || !(0.0..=1.0).contains(&self.recognition.minimum_rms)
        {
            return Err(SettingsError::InvalidMinimumRms(
                self.recognition.minimum_rms,
            ));
        }

        let language = self.recognition.language.trim().to_ascii_lowercase();
        if !(2..=16).contains(&language.len())
            || !language
                .bytes()
                .all(|character| character.is_ascii_alphabetic() || character == b'-')
        {
            return Err(SettingsError::InvalidLanguage(
                self.recognition.language.clone(),
            ));
        }
        self.recognition.language = language;

        if let Some(microphone) = &mut self.recognition.microphone {
            let normalized = microphone.trim();
            if normalized.is_empty() {
                self.recognition.microphone = None;
            } else if normalized.chars().count() > 512 {
                return Err(SettingsError::MicrophoneNameTooLong);
            } else if normalized.len() != microphone.len() {
                *microphone = normalized.to_owned();
            }
        }

        if let Some(instructions) = &self.formatting.custom_instructions
            && instructions.chars().count() > MAX_CUSTOM_INSTRUCTIONS_CHARS
        {
            return Err(SettingsError::CustomInstructionsTooLong);
        }
        if self.formatting.strength == FormattingStrength::Custom
            && self
                .formatting
                .custom_instructions
                .as_deref()
                .is_none_or(|instructions| instructions.trim().is_empty())
        {
            return Err(SettingsError::MissingCustomInstructions);
        }

        if let Some(model) = &mut self.formatting.ollama_model {
            let normalized = model.trim();
            if normalized.is_empty() {
                self.formatting.ollama_model = None;
            } else if normalized.chars().count() > 256 {
                return Err(SettingsError::OllamaModelNameTooLong);
            } else if normalized.len() != model.len() {
                *model = normalized.to_owned();
            }
        }
        if let Some(identity) = &mut self.formatting.ollama_model_identity {
            identity.validate_and_normalize()?;
            if self.formatting.ollama_model.is_none() {
                return Err(SettingsError::OrphanedOllamaModelIdentity);
            }
        }

        Ok(())
    }

    pub fn ensure_runtime_supported(&self) -> Result<(), SettingsError> {
        match self.formatting.strength {
            FormattingStrength::Raw | FormattingStrength::Light => Ok(()),
            _ if self.formatting.ollama_model.is_none() => Err(
                SettingsError::FormattingModelRequired(self.formatting.strength),
            ),
            _ if self.formatting.ollama_model_identity.is_none() => {
                Err(SettingsError::FormattingModelIdentityRequired)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecognitionSettings {
    pub mode: RecognitionMode,
    /// Whisper GGML model used by Accurate mode. The field name is retained
    /// so schema 1-3 files migrate without losing their configured path.
    pub model_path: PathBuf,
    /// Unpacked Vosk model directory matching `language`.
    pub instant_model_path: PathBuf,
    /// Vosk runtime bundle directory containing libvosk.dll and its dependencies.
    pub instant_runtime_path: PathBuf,
    /// A UI/catalog hint. `model_path` remains authoritative and the runtime
    /// verifies this identity before presenting a pinned variant.
    pub accurate_model: AccurateModelVariant,
    pub accurate_backend: AccurateBackendPreference,
    pub language: String,
    pub minimum_rms: f32,
    /// Exact CPAL/Windows input-device name. `None` follows the system default.
    pub microphone: Option<String>,
}

impl Default for RecognitionSettings {
    fn default() -> Self {
        Self {
            mode: RecognitionMode::Accurate,
            model_path: PathBuf::from("models/ggml-base.en.bin"),
            instant_model_path: PathBuf::from("models/vosk-model-small-en-us-0.15"),
            instant_runtime_path: PathBuf::from("runtime/vosk"),
            accurate_model: AccurateModelVariant::BaseEnglish,
            accurate_backend: AccurateBackendPreference::Auto,
            language: "en".to_owned(),
            minimum_rms: 0.003,
            microphone: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionMode {
    Instant,
    #[default]
    Accurate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccurateModelVariant {
    TinyEnglish,
    BaseEnglish,
    TinyMultilingual,
    BaseMultilingual,
    /// Existing and user-supplied paths deserialize conservatively as custom.
    #[default]
    Custom,
}

impl AccurateModelVariant {
    pub const fn manifest_id(self) -> Option<&'static str> {
        match self {
            Self::TinyEnglish => Some("whisper-tiny-en-f16"),
            Self::BaseEnglish => Some("whisper-base-en-f16"),
            Self::TinyMultilingual => Some("whisper-tiny-multilingual-f16"),
            Self::BaseMultilingual => Some("whisper-base-multilingual-f16"),
            Self::Custom => None,
        }
    }

    pub fn from_manifest_id(id: &str) -> Option<Self> {
        match id {
            "whisper-tiny-en-f16" => Some(Self::TinyEnglish),
            "whisper-base-en-f16" => Some(Self::BaseEnglish),
            "whisper-tiny-multilingual-f16" => Some(Self::TinyMultilingual),
            "whisper-base-multilingual-f16" => Some(Self::BaseMultilingual),
            _ => None,
        }
    }

    pub fn supports_language(self, language: &str) -> bool {
        match self {
            Self::TinyEnglish | Self::BaseEnglish => {
                language == "en" || language.starts_with("en-")
            }
            Self::TinyMultilingual | Self::BaseMultilingual | Self::Custom => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccurateBackendPreference {
    #[default]
    Auto,
    Vulkan,
    Cpu,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FormattingSettings {
    pub strength: FormattingStrength,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// Explicitly selected installed Ollama model. Never chosen implicitly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ollama_model: Option<String>,
    /// Manifest identity observed when the user explicitly selected
    /// `ollama_model`. It is revalidated when the resident worker starts;
    /// external Ollama clients can still mutate a tag after that boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ollama_model_identity: Option<OllamaModelIdentity>,
    pub ollama_lifecycle: OllamaLifecycle,
}

impl Default for FormattingSettings {
    fn default() -> Self {
        Self {
            strength: FormattingStrength::Light,
            custom_instructions: None,
            ollama_model: None,
            ollama_model_identity: None,
            ollama_lifecycle: OllamaLifecycle::Balanced,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OllamaModelIdentity {
    pub manifest_sha256: String,
    pub bytes: u64,
}

impl OllamaModelIdentity {
    pub fn new(manifest_sha256: impl Into<String>, bytes: u64) -> Result<Self, SettingsError> {
        let mut identity = Self {
            manifest_sha256: manifest_sha256.into(),
            bytes,
        };
        identity.validate_and_normalize()?;
        Ok(identity)
    }

    fn validate_and_normalize(&mut self) -> Result<(), SettingsError> {
        let digest = self
            .manifest_sha256
            .strip_prefix("sha256:")
            .unwrap_or(&self.manifest_sha256)
            .to_ascii_lowercase();
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(SettingsError::InvalidOllamaModelDigest);
        }
        if self.bytes == 0 {
            return Err(SettingsError::InvalidOllamaModelSize);
        }
        self.manifest_sha256 = digest;
        Ok(())
    }

    #[must_use]
    pub fn matches(&self, digest: Option<&str>, bytes: Option<u64>) -> bool {
        digest
            .and_then(|value| value.strip_prefix("sha256:").or(Some(value)))
            .is_some_and(|value| value.eq_ignore_ascii_case(&self.manifest_sha256))
            && bytes == Some(self.bytes)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OllamaLifecycle {
    Instant,
    #[default]
    Balanced,
    MemorySaver,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InteractionSettings {
    pub recording_mode: RecordingMode,
    pub launcher_shortcut: String,
    pub direct_dictation_shortcut: Option<String>,
}

impl Default for InteractionSettings {
    fn default() -> Self {
        Self {
            recording_mode: RecordingMode::default(),
            launcher_shortcut: DEFAULT_LAUNCHER_SHORTCUT.to_owned(),
            direct_dictation_shortcut: None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchSettings {
    pub embedding_model: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    #[default]
    Hold,
    Toggle,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacySettings {
    pub history_retention: HistoryRetention,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            history_retention: HistoryRetention::SevenDays,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRetention {
    Disabled,
    OneDay,
    #[default]
    SevenDays,
    ThirtyDays,
    Indefinite,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StartupSettings {
    pub launch_at_login: bool,
    pub onboarding_complete: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppearanceSettings {
    pub theme: AppearancePreference,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearancePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormattingStrength {
    Raw,
    #[default]
    Light,
    Balanced,
    Strong,
    Custom,
}

impl std::fmt::Display for FormattingStrength {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Raw => "raw",
            Self::Light => "light",
            Self::Balanced => "balanced",
            Self::Strong => "strong",
            Self::Custom => "custom",
        };
        formatter.write_str(name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeFormatting {
    Raw,
    Light,
    Balanced,
    Strong,
    Custom,
}

impl TryFrom<FormattingStrength> for RuntimeFormatting {
    type Error = SettingsError;

    fn try_from(strength: FormattingStrength) -> Result<Self, Self::Error> {
        match strength {
            FormattingStrength::Raw => Ok(Self::Raw),
            FormattingStrength::Light => Ok(Self::Light),
            FormattingStrength::Balanced => Ok(Self::Balanced),
            FormattingStrength::Strong => Ok(Self::Strong),
            FormattingStrength::Custom => Ok(Self::Custom),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn default_for_current_user() -> Result<Self, SettingsError> {
        let local_app_data = env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .ok_or(SettingsError::LocalAppDataUnavailable)?;
        Self::new(PathBuf::from(local_app_data).join("Phorminx/settings.toml"))
    }

    pub fn new(path: PathBuf) -> Result<Self, SettingsError> {
        let path = if path.is_absolute() {
            path
        } else {
            env::current_dir()
                .map_err(SettingsError::CurrentDirectory)?
                .join(path)
        };
        if path.file_name().is_none() || path.parent().is_none() {
            return Err(SettingsError::InvalidSettingsPath(path));
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Settings, SettingsError> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(source) => {
                return Err(SettingsError::Open {
                    path: self.path.clone(),
                    source,
                });
            }
        };

        if file
            .metadata()
            .map_err(|source| SettingsError::Read {
                path: self.path.clone(),
                source,
            })?
            .len()
            > MAX_SETTINGS_BYTES
        {
            return Err(SettingsError::TooLarge(self.path.clone()));
        }

        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| SettingsError::Read {
                path: self.path.clone(),
                source,
            })?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(SettingsError::TooLarge(self.path.clone()));
        }
        let text = std::str::from_utf8(&bytes).map_err(|source| SettingsError::Utf8 {
            path: self.path.clone(),
            source,
        })?;
        let raw = toml::from_str::<toml::Value>(text).map_err(|source| SettingsError::Parse {
            path: self.path.clone(),
            source,
        })?;
        let source_version = raw
            .get("schema_version")
            .and_then(toml::Value::as_integer)
            .and_then(|version| u32::try_from(version).ok());
        let recognition = raw.get("recognition").and_then(toml::Value::as_table);
        let missing_accurate_model = source_version.is_some_and(|version| version <= 4)
            && recognition.is_none_or(|table| !table.contains_key("accurate_model"));
        let missing_accurate_backend = source_version.is_some_and(|version| version <= 4)
            && recognition.is_none_or(|table| !table.contains_key("accurate_backend"));
        let mut settings =
            toml::from_str::<Settings>(text).map_err(|source| SettingsError::Parse {
                path: self.path.clone(),
                source,
            })?;
        let identified_legacy_model = missing_accurate_model
            .then(|| {
                crate::model::identify_pinned_model(
                    &self.resolve_model_path(&settings.recognition.model_path),
                )
                .ok()
                .flatten()
            })
            .flatten();
        migrate_legacy_accurate_fields(
            &mut settings,
            missing_accurate_model,
            missing_accurate_backend,
            identified_legacy_model,
        );
        settings.validate_and_normalize()?;
        Ok(settings)
    }

    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        let _save_guard = SAVE_LOCK
            .lock()
            .map_err(|_| SettingsError::SaveLockPoisoned)?;
        self.save_locked(settings)
    }

    /// Atomically commits `replacement` only while the durable settings still
    /// equal the snapshot that was validated. Long-running work cannot
    /// overwrite a newer edit made in the unified UI.
    pub fn compare_and_save(
        &self,
        expected: &Settings,
        replacement: &Settings,
    ) -> Result<bool, SettingsError> {
        let _save_guard = SAVE_LOCK
            .lock()
            .map_err(|_| SettingsError::SaveLockPoisoned)?;
        let temporary_path = self.prepare_locked(replacement)?;
        self.commit_temporary_if_unchanged(temporary_path, expected)
    }

    fn commit_temporary_if_unchanged(
        &self,
        temporary_path: PathBuf,
        expected: &Settings,
    ) -> Result<bool, SettingsError> {
        let current = match self.load() {
            Ok(current) => current,
            Err(error) => {
                let _ = fs::remove_file(temporary_path);
                return Err(error);
            }
        };
        if current != *expected {
            let _ = fs::remove_file(temporary_path);
            return Ok(false);
        }
        self.commit_temporary(temporary_path)?;
        Ok(true)
    }

    fn save_locked(&self, settings: &Settings) -> Result<(), SettingsError> {
        let temporary_path = self.prepare_locked(settings)?;
        self.commit_temporary(temporary_path)
    }

    fn prepare_locked(&self, settings: &Settings) -> Result<PathBuf, SettingsError> {
        let mut canonical = settings.clone();
        canonical.validate_and_normalize()?;
        let serialized = toml::to_string_pretty(&canonical).map_err(SettingsError::Serialize)?;
        let mut reparsed = toml::from_str::<Settings>(&serialized)
            .map_err(|source| SettingsError::InternalRoundTrip(source.to_string()))?;
        reparsed.validate_and_normalize()?;
        if reparsed != canonical {
            return Err(SettingsError::InternalRoundTrip(
                "serialized settings changed meaning".to_owned(),
            ));
        }

        let directory = self
            .path
            .parent()
            .ok_or_else(|| SettingsError::InvalidSettingsPath(self.path.clone()))?;
        fs::create_dir_all(directory).map_err(|source| SettingsError::CreateDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
        self.preserve_schema_3_backup(directory)?;

        let (temporary_path, mut temporary_file) = self.create_temporary_file(directory)?;
        let write_result = (|| {
            temporary_file
                .write_all(serialized.as_bytes())
                .and_then(|_| temporary_file.flush())
                .and_then(|_| temporary_file.sync_all())
                .map_err(|source| SettingsError::Write {
                    path: temporary_path.clone(),
                    source,
                })?;
            drop(temporary_file);
            Ok(temporary_path.clone())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        write_result
    }

    fn commit_temporary(&self, temporary_path: PathBuf) -> Result<(), SettingsError> {
        let result =
            atomic_replace_file(&temporary_path, &self.path).map_err(SettingsError::AtomicReplace);
        if result.is_err() {
            let _ = fs::remove_file(temporary_path);
        }
        result
    }

    pub fn resolve_model_path(&self, model_path: &Path) -> PathBuf {
        self.resolve_asset_path(model_path)
    }

    pub fn resolve_asset_path(&self, asset_path: &Path) -> PathBuf {
        if asset_path.is_absolute() {
            return asset_path.to_path_buf();
        }
        self.path
            .parent()
            .expect("validated settings path has a parent")
            .join(asset_path)
    }

    fn create_temporary_file(&self, directory: &Path) -> Result<(PathBuf, File), SettingsError> {
        for _ in 0..100 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary_path = directory.join(format!(
                ".settings.toml.{}.{}.tmp",
                std::process::id(),
                sequence
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
            {
                Ok(file) => return Ok((temporary_path, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(SettingsError::CreateTemporary {
                        path: temporary_path,
                        source,
                    });
                }
            }
        }
        Err(SettingsError::TemporaryNameExhausted(
            directory.to_path_buf(),
        ))
    }

    fn preserve_schema_3_backup(&self, directory: &Path) -> Result<(), SettingsError> {
        let backup_path = directory.join("settings.schema-3.backup.toml");
        if backup_path.exists() || !self.path.is_file() {
            return Ok(());
        }
        let original = fs::read(&self.path).map_err(|source| SettingsError::Read {
            path: self.path.clone(),
            source,
        })?;
        if original.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(SettingsError::TooLarge(self.path.clone()));
        }
        let value =
            toml::from_str::<toml::Value>(std::str::from_utf8(&original).map_err(|source| {
                SettingsError::Utf8 {
                    path: self.path.clone(),
                    source,
                }
            })?)
            .map_err(|source| SettingsError::Parse {
                path: self.path.clone(),
                source,
            })?;
        let version = value
            .get("schema_version")
            .and_then(toml::Value::as_integer)
            .unwrap_or_default();
        if !(1..=3).contains(&version) {
            return Ok(());
        }
        let mut backup = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(source) => {
                return Err(SettingsError::CreateTemporary {
                    path: backup_path,
                    source,
                });
            }
        };
        backup
            .write_all(&original)
            .and_then(|_| backup.flush())
            .and_then(|_| backup.sync_all())
            .map_err(|source| SettingsError::Write {
                path: backup_path,
                source,
            })
    }
}

fn migrate_legacy_accurate_fields(
    settings: &mut Settings,
    missing_model: bool,
    missing_backend: bool,
    identified_model: Option<AccurateModelVariant>,
) {
    if missing_model {
        settings.recognition.accurate_model =
            identified_model.unwrap_or(AccurateModelVariant::Custom);
    }
    if missing_backend {
        settings.recognition.accurate_backend = AccurateBackendPreference::Auto;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    InvalidShortcut(ShortcutError),
    #[error("Choose an embedding model name of at most 256 bytes without control characters.")]
    InvalidEmbeddingModel,
    #[error("Windows did not provide a Local AppData directory")]
    LocalAppDataUnavailable,
    #[error("failed to resolve the current directory: {0}")]
    CurrentDirectory(std::io::Error),
    #[error("invalid settings path: {0}")]
    InvalidSettingsPath(PathBuf),
    #[error("settings schema_version is missing or invalid")]
    MissingOrInvalidVersion,
    #[error("settings version {found} is newer than the supported version {supported}")]
    FutureVersion { found: u32, supported: u32 },
    #[error("settings version {0} has no migration path")]
    UnsupportedOldVersion(u32),
    #[error("recognition.model_path must not be empty")]
    EmptyModelPath,
    #[error("recognition.instant_model_path must not be empty")]
    EmptyInstantModelPath,
    #[error("recognition.instant_runtime_path must not be empty")]
    EmptyInstantRuntimePath,
    #[error("recognition.minimum_rms must be finite and between 0 and 1, got {0}")]
    InvalidMinimumRms(f32),
    #[error("recognition.language must contain 2-16 ASCII letters or hyphens, got {0:?}")]
    InvalidLanguage(String),
    #[error("recognition.microphone must not exceed 512 characters")]
    MicrophoneNameTooLong,
    #[error("formatting.custom_instructions must not exceed 4096 characters")]
    CustomInstructionsTooLong,
    #[error("custom formatting requires nonblank custom_instructions")]
    MissingCustomInstructions,
    #[error("formatting.ollama_model must not exceed 256 characters")]
    OllamaModelNameTooLong,
    #[error("formatting.ollama_model_identity cannot exist without formatting.ollama_model")]
    OrphanedOllamaModelIdentity,
    #[error("formatting.ollama_model_identity.manifest_sha256 must be a SHA-256 digest")]
    InvalidOllamaModelDigest,
    #[error("formatting.ollama_model_identity.bytes must be positive")]
    InvalidOllamaModelSize,
    #[error("{0} formatting requires an explicitly selected installed Ollama model")]
    FormattingModelRequired(FormattingStrength),
    #[error("AI formatting requires a verified pinned Ollama model identity")]
    FormattingModelIdentityRequired,
    #[error("failed to open settings file {path}: {source}")]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read settings file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("settings file is larger than 64 KiB: {0}")]
    TooLarge(PathBuf),
    #[error("settings file is not UTF-8 ({path}): {source}")]
    Utf8 {
        path: PathBuf,
        source: std::str::Utf8Error,
    },
    #[error("could not parse settings file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("could not serialize settings: {0}")]
    Serialize(toml::ser::Error),
    #[error("settings serialization self-check failed: {0}")]
    InternalRoundTrip(String),
    #[error("the settings save lock is poisoned")]
    SaveLockPoisoned,
    #[error("failed to create settings directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to create temporary settings file {path}: {source}")]
    CreateTemporary {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not allocate a unique temporary settings file in {0}")]
    TemporaryNameExhausted(PathBuf),
    #[error("failed to write temporary settings file {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    AtomicReplace(phorminx_windows::AtomicReplaceError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_six_migrates_with_launcher_defaults_without_changing_direct_recording_mode() {
        let mut settings: Settings =
            toml::from_str("schema_version = 6\n[interaction]\nrecording_mode = 'hold'\n").unwrap();
        settings.validate_and_normalize().unwrap();
        assert_eq!(settings.schema_version, 7);
        assert_eq!(settings.interaction.launcher_shortcut, "Ctrl+Alt+Space");
        assert_eq!(settings.interaction.direct_dictation_shortcut, None);
        assert_eq!(settings.interaction.recording_mode, RecordingMode::Hold);
        assert_eq!(settings.search.embedding_model, None);
    }

    #[test]
    fn shortcut_and_embedding_preferences_round_trip_and_normalize() {
        let mut settings = Settings::default();
        settings.interaction.launcher_shortcut = " control + alt + f8 ".to_owned();
        settings.interaction.direct_dictation_shortcut = Some("f9".to_owned());
        settings.search.embedding_model = Some(" local-embedding:latest ".to_owned());
        settings.validate_and_normalize().unwrap();
        assert_eq!(settings.interaction.launcher_shortcut, "Ctrl+Alt+F8");
        assert_eq!(
            settings.interaction.direct_dictation_shortcut.as_deref(),
            Some("F9")
        );
        assert_eq!(
            settings.search.embedding_model.as_deref(),
            Some("local-embedding:latest")
        );
        let encoded = toml::to_string(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&encoded).unwrap(), settings);
    }

    #[test]
    fn invalid_shortcuts_and_embedding_models_are_rejected() {
        let mut settings = Settings::default();
        settings.interaction.direct_dictation_shortcut = Some("Alt+Ctrl+Space".to_owned());
        assert!(matches!(
            settings.validate_and_normalize(),
            Err(SettingsError::InvalidShortcut(ShortcutError::Duplicate))
        ));
        settings.interaction.direct_dictation_shortcut = None;
        for model in ["a".repeat(257), "model\nname".to_owned()] {
            settings.search.embedding_model = Some(model);
            assert!(matches!(
                settings.validate_and_normalize(),
                Err(SettingsError::InvalidEmbeddingModel)
            ));
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "phorminx-settings-{name}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_preserve_current_runtime_behavior() {
        let settings = Settings::default();
        assert_eq!(settings.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            settings.recognition.model_path,
            PathBuf::from("models/ggml-base.en.bin")
        );
        assert_eq!(settings.recognition.language, "en");
        assert_eq!(settings.recognition.minimum_rms, 0.003);
        assert_eq!(settings.recognition.microphone, None);
        assert_eq!(settings.recognition.mode, RecognitionMode::Accurate);
        assert_eq!(
            settings.recognition.instant_model_path,
            PathBuf::from("models/vosk-model-small-en-us-0.15")
        );
        assert_eq!(
            settings.recognition.instant_runtime_path,
            PathBuf::from("runtime/vosk")
        );
        assert_eq!(settings.formatting.strength, FormattingStrength::Light);
        assert_eq!(
            settings.privacy.history_retention,
            HistoryRetention::SevenDays
        );
        assert_eq!(settings.appearance.theme, AppearancePreference::System);
    }

    #[test]
    fn every_formatting_strength_round_trips() {
        for strength in [
            FormattingStrength::Raw,
            FormattingStrength::Light,
            FormattingStrength::Balanced,
            FormattingStrength::Strong,
            FormattingStrength::Custom,
        ] {
            let mut settings = Settings::default();
            settings.formatting.strength = strength;
            if strength == FormattingStrength::Custom {
                settings.formatting.custom_instructions =
                    Some("Keep nomes em português.".to_owned());
            }
            let text = toml::to_string_pretty(&settings).unwrap();
            let decoded: Settings = toml::from_str(&text).unwrap();
            assert_eq!(decoded, settings);
        }
    }

    #[test]
    fn missing_file_returns_defaults_without_creating_it() {
        let directory = TestDirectory::new("missing");
        let path = directory.0.join("settings.toml");
        let store = SettingsStore::new(path.clone()).unwrap();
        assert_eq!(store.load().unwrap(), Settings::default());
        assert!(!path.exists());
    }

    #[test]
    fn compare_and_save_never_overwrites_a_newer_snapshot() {
        let directory = TestDirectory::new("compare-and-save");
        let store = SettingsStore::new(directory.0.join("settings.toml")).unwrap();
        let original = Settings::default();
        store.save(&original).unwrap();

        let mut concurrent = original.clone();
        concurrent.recognition.minimum_rms = 0.02;
        store.save(&concurrent).unwrap();

        let mut stale_candidate = original.clone();
        stale_candidate.startup.launch_at_login = true;
        assert!(!store.compare_and_save(&original, &stale_candidate).unwrap());
        assert_eq!(store.load().unwrap(), concurrent);
    }

    #[test]
    fn partial_sections_use_field_defaults() {
        let decoded: Settings = toml::from_str(
            r#"
schema_version = 1
[recognition]
language = "pt-BR"
"#,
        )
        .unwrap();
        assert_eq!(decoded.recognition.language, "pt-BR");
        assert_eq!(decoded.recognition.minimum_rms, 0.003);
        assert_eq!(decoded.formatting, FormattingSettings::default());
    }

    #[test]
    fn version_one_files_migrate_in_memory_with_private_defaults() {
        let directory = TestDirectory::new("migrate-v1");
        let path = directory.0.join("settings.toml");
        fs::write(
            &path,
            "schema_version = 1\n[recognition]\nlanguage = \"pt-BR\"\n",
        )
        .unwrap();

        let loaded = SettingsStore::new(path).unwrap().load().unwrap();

        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.recognition.language, "pt-br");
        assert_eq!(loaded.recognition.microphone, None);
        assert_eq!(
            loaded.privacy.history_retention,
            HistoryRetention::SevenDays
        );
        assert!(!loaded.startup.launch_at_login);
        assert_eq!(loaded.appearance.theme, AppearancePreference::System);
    }

    #[test]
    fn version_two_files_migrate_with_system_appearance() {
        let directory = TestDirectory::new("migrate-v2");
        let path = directory.0.join("settings.toml");
        fs::write(&path, "schema_version = 2\n").unwrap();

        let loaded = SettingsStore::new(path).unwrap().load().unwrap();

        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.appearance.theme, AppearancePreference::System);
    }

    #[test]
    fn version_three_custom_path_migrates_conservatively_instead_of_claiming_base() {
        let directory = TestDirectory::new("migrate-v3-custom");
        let path = directory.0.join("settings.toml");
        fs::write(
            directory.0.join("personal-model.bin"),
            b"not a pinned model",
        )
        .unwrap();
        fs::write(
            &path,
            "schema_version = 3\n[recognition]\nmodel_path = \"personal-model.bin\"\nlanguage = \"pt-br\"\n",
        )
        .unwrap();

        let loaded = SettingsStore::new(path).unwrap().load().unwrap();
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.recognition.mode, RecognitionMode::Accurate);
        assert_eq!(
            loaded.recognition.accurate_model,
            AccurateModelVariant::Custom
        );
        assert_eq!(
            loaded.recognition.accurate_backend,
            AccurateBackendPreference::Auto
        );
    }

    #[test]
    fn version_three_missing_fields_adopt_verified_multilingual_identity() {
        let mut settings = Settings {
            schema_version: 3,
            ..Settings::default()
        };

        migrate_legacy_accurate_fields(
            &mut settings,
            true,
            true,
            Some(AccurateModelVariant::BaseMultilingual),
        );

        assert_eq!(
            settings.recognition.accurate_model,
            AccurateModelVariant::BaseMultilingual
        );
        assert_eq!(
            settings.recognition.accurate_backend,
            AccurateBackendPreference::Auto
        );
    }

    #[test]
    fn version_three_missing_fields_adopt_verified_english_pinned_identity() {
        let mut settings = Settings {
            schema_version: 3,
            ..Settings::default()
        };

        migrate_legacy_accurate_fields(
            &mut settings,
            true,
            true,
            Some(AccurateModelVariant::TinyEnglish),
        );

        assert_eq!(
            settings.recognition.accurate_model,
            AccurateModelVariant::TinyEnglish
        );
    }

    #[test]
    fn first_schema_five_save_preserves_exact_schema_three_rollback_backup() {
        let directory = TestDirectory::new("schema-v3-backup");
        let path = directory.0.join("settings.toml");
        let original = "schema_version = 3\n[recognition]\nmodel_path = 'models/custom.bin'\n";
        fs::write(&path, original).unwrap();
        let store = SettingsStore::new(path).unwrap();
        let loaded = store.load().unwrap();
        store.save(&loaded).unwrap();
        let backup = directory.0.join("settings.schema-3.backup.toml");
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);

        fs::write(&backup, "do not replace").unwrap();
        store.save(&loaded).unwrap();
        assert_eq!(fs::read_to_string(backup).unwrap(), "do not replace");
    }

    #[test]
    fn every_appearance_preference_round_trips() {
        for preference in [
            AppearancePreference::System,
            AppearancePreference::Light,
            AppearancePreference::Dark,
        ] {
            let mut settings = Settings::default();
            settings.appearance.theme = preference;
            let encoded = toml::to_string_pretty(&settings).unwrap();
            let decoded: Settings = toml::from_str(&encoded).unwrap();
            assert_eq!(decoded.appearance.theme, preference);
        }
    }

    #[test]
    fn ai_profiles_require_explicit_model_selection() {
        for strength in [
            FormattingStrength::Balanced,
            FormattingStrength::Strong,
            FormattingStrength::Custom,
        ] {
            let mut settings = Settings::default();
            settings.formatting.strength = strength;
            if strength == FormattingStrength::Custom {
                settings.formatting.custom_instructions = Some("Use bullets.".to_owned());
            }
            assert!(matches!(
                settings.ensure_runtime_supported(),
                Err(SettingsError::FormattingModelRequired(found)) if found == strength
            ));
            settings.formatting.ollama_model = Some("qwen2.5:3b".to_owned());
            assert!(matches!(
                settings.ensure_runtime_supported(),
                Err(SettingsError::FormattingModelIdentityRequired)
            ));
            settings.formatting.ollama_model_identity =
                Some(OllamaModelIdentity::new("a".repeat(64), 42).unwrap());
            assert!(settings.ensure_runtime_supported().is_ok());
        }
    }

    #[test]
    fn schema_five_model_name_migrates_without_trusting_the_live_tag() {
        let directory = TestDirectory::new("migrate-v5-ollama-identity");
        let path = directory.0.join("settings.toml");
        fs::write(
            &path,
            "schema_version = 5\n[formatting]\nstrength = 'balanced'\nollama_model = 'qwen2.5:3b'\n",
        )
        .unwrap();

        let loaded = SettingsStore::new(path).unwrap().load().unwrap();

        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            loaded.formatting.ollama_model.as_deref(),
            Some("qwen2.5:3b")
        );
        assert_eq!(loaded.formatting.ollama_model_identity, None);
        assert!(matches!(
            loaded.ensure_runtime_supported(),
            Err(SettingsError::FormattingModelIdentityRequired)
        ));
    }

    #[test]
    fn ollama_identity_is_normalized_and_strictly_validated() {
        let identity = OllamaModelIdentity::new(format!("sha256:{}", "A".repeat(64)), 42).unwrap();
        assert_eq!(identity.manifest_sha256, "a".repeat(64));
        assert!(identity.matches(Some(&format!("sha256:{}", "A".repeat(64))), Some(42)));
        assert!(!identity.matches(Some(&"a".repeat(64)), Some(43)));
        assert!(OllamaModelIdentity::new("a".repeat(63), 42).is_err());
        assert!(OllamaModelIdentity::new("g".repeat(64), 42).is_err());
        assert!(OllamaModelIdentity::new("a".repeat(64), 0).is_err());
    }

    #[test]
    fn validation_rejects_invalid_values_and_unknown_fields() {
        assert!(toml::from_str::<Settings>("[recognition]").is_err());
        assert!(toml::from_str::<Settings>("schema_version = 1\nsurprise = true").is_err());

        for minimum_rms in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            let mut settings = Settings::default();
            settings.recognition.minimum_rms = minimum_rms;
            assert!(matches!(
                settings.validate_and_normalize(),
                Err(SettingsError::InvalidMinimumRms(_))
            ));
        }

        for language in ["", "e", "português", "en_US"] {
            let mut settings = Settings::default();
            settings.recognition.language = language.to_owned();
            assert!(matches!(
                settings.validate_and_normalize(),
                Err(SettingsError::InvalidLanguage(_))
            ));
        }
    }

    #[test]
    fn custom_requires_bounded_nonblank_instructions() {
        let mut settings = Settings::default();
        settings.formatting.strength = FormattingStrength::Custom;
        assert!(matches!(
            settings.validate_and_normalize(),
            Err(SettingsError::MissingCustomInstructions)
        ));
        settings.formatting.custom_instructions = Some(" ".to_owned());
        assert!(matches!(
            settings.validate_and_normalize(),
            Err(SettingsError::MissingCustomInstructions)
        ));
        settings.formatting.custom_instructions = Some("x".repeat(4_097));
        assert!(matches!(
            settings.validate_and_normalize(),
            Err(SettingsError::CustomInstructionsTooLong)
        ));
    }

    #[test]
    fn save_is_atomic_and_reloadable() {
        let directory = TestDirectory::new("save");
        let store = SettingsStore::new(directory.0.join("settings.toml")).unwrap();
        let mut settings = Settings::default();
        settings.recognition.language = "PT-br".to_owned();
        settings.recognition.model_path = PathBuf::from("modelos/áudio.bin");
        settings.formatting.custom_instructions = Some("Preserve nomes.\nUse frases.".to_owned());

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.recognition.language, "pt-br");
        assert_eq!(
            loaded.recognition.model_path,
            settings.recognition.model_path
        );
        assert_eq!(
            loaded.formatting.custom_instructions,
            settings.formatting.custom_instructions
        );

        let mut replacement = loaded;
        replacement.formatting.strength = FormattingStrength::Raw;
        store.save(&replacement).unwrap();
        assert_eq!(store.load().unwrap(), replacement);
    }

    #[test]
    fn prepared_compare_and_save_rechecks_external_changes_at_commit_boundary() {
        let directory = TestDirectory::new("compare-external");
        let store = SettingsStore::new(directory.0.join("settings.toml")).unwrap();
        let original = Settings::default();
        store.save(&original).unwrap();

        let mut replacement = original.clone();
        replacement.recognition.minimum_rms = 0.1;
        let temporary = store.prepare_locked(&replacement).unwrap();

        let mut external = original.clone();
        external.recognition.minimum_rms = 0.2;
        let serialized = toml::to_string_pretty(&external).unwrap();
        fs::write(store.path(), serialized).unwrap();

        assert!(
            !store
                .commit_temporary_if_unchanged(temporary.clone(), &original)
                .unwrap()
        );
        assert_eq!(store.load().unwrap(), external);
        assert!(!temporary.exists());
    }

    #[test]
    fn relative_model_paths_are_based_on_settings_directory() {
        let directory = TestDirectory::new("relative");
        let store = SettingsStore::new(directory.0.join("settings.toml")).unwrap();
        assert_eq!(
            store.resolve_model_path(Path::new("models/base.bin")),
            directory.0.join("models/base.bin")
        );
    }
}

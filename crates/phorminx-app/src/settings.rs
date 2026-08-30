use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use phorminx_windows::atomic_replace_file;
use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            recognition: RecognitionSettings::default(),
            formatting: FormattingSettings::default(),
        }
    }
}

impl Settings {
    pub fn validate_and_normalize(&mut self) -> Result<(), SettingsError> {
        match self.schema_version {
            CURRENT_SCHEMA_VERSION => {}
            0 => return Err(SettingsError::MissingOrInvalidVersion),
            version if version > CURRENT_SCHEMA_VERSION => {
                return Err(SettingsError::FutureVersion {
                    found: version,
                    supported: CURRENT_SCHEMA_VERSION,
                });
            }
            version => return Err(SettingsError::UnsupportedOldVersion(version)),
        }

        if self.recognition.model_path.as_os_str().is_empty() {
            return Err(SettingsError::EmptyModelPath);
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

        Ok(())
    }

    pub fn ensure_runtime_supported(&self) -> Result<(), SettingsError> {
        match self.formatting.strength {
            FormattingStrength::Raw | FormattingStrength::Light => Ok(()),
            strength => Err(SettingsError::FormattingNotAvailable(strength)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecognitionSettings {
    pub model_path: PathBuf,
    pub language: String,
    pub minimum_rms: f32,
}

impl Default for RecognitionSettings {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("models/ggml-base.en.bin"),
            language: "en".to_owned(),
            minimum_rms: 0.003,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FormattingSettings {
    pub strength: FormattingStrength,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
}

impl Default for FormattingSettings {
    fn default() -> Self {
        Self {
            strength: FormattingStrength::Light,
            custom_instructions: None,
        }
    }
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
}

impl TryFrom<FormattingStrength> for RuntimeFormatting {
    type Error = SettingsError;

    fn try_from(strength: FormattingStrength) -> Result<Self, Self::Error> {
        match strength {
            FormattingStrength::Raw => Ok(Self::Raw),
            FormattingStrength::Light => Ok(Self::Light),
            other => Err(SettingsError::FormattingNotAvailable(other)),
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
        let mut settings =
            toml::from_str::<Settings>(text).map_err(|source| SettingsError::Parse {
                path: self.path.clone(),
                source,
            })?;
        settings.validate_and_normalize()?;
        Ok(settings)
    }

    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        let _save_guard = SAVE_LOCK
            .lock()
            .map_err(|_| SettingsError::SaveLockPoisoned)?;
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
            atomic_replace_file(&temporary_path, &self.path).map_err(SettingsError::AtomicReplace)
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        write_result
    }

    pub fn resolve_model_path(&self, model_path: &Path) -> PathBuf {
        if model_path.is_absolute() {
            return model_path.to_path_buf();
        }
        self.path
            .parent()
            .expect("validated settings path has a parent")
            .join(model_path)
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
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
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
    #[error("recognition.minimum_rms must be finite and between 0 and 1, got {0}")]
    InvalidMinimumRms(f32),
    #[error("recognition.language must contain 2-16 ASCII letters or hyphens, got {0:?}")]
    InvalidLanguage(String),
    #[error("formatting.custom_instructions must not exceed 4096 characters")]
    CustomInstructionsTooLong,
    #[error("custom formatting requires nonblank custom_instructions")]
    MissingCustomInstructions,
    #[error("{0} formatting requires the local AI phase and is not available yet")]
    FormattingNotAvailable(FormattingStrength),
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
        assert_eq!(settings.schema_version, 1);
        assert_eq!(
            settings.recognition.model_path,
            PathBuf::from("models/ggml-base.en.bin")
        );
        assert_eq!(settings.recognition.language, "en");
        assert_eq!(settings.recognition.minimum_rms, 0.003);
        assert_eq!(settings.formatting.strength, FormattingStrength::Light);
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
    fn relative_model_paths_are_based_on_settings_directory() {
        let directory = TestDirectory::new("relative");
        let store = SettingsStore::new(directory.0.join("settings.toml")).unwrap();
        assert_eq!(
            store.resolve_model_path(Path::new("models/base.bin")),
            directory.0.join("models/base.bin")
        );
    }
}

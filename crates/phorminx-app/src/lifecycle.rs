//! Process lifecycle helpers: the profile-scoped instance slot and a small
//! local log of exits and restarts.
//!
//! A windowed release build has no console, so `eprintln!` diagnostics about
//! why the app closed were lost. The lifecycle log keeps only event names,
//! process ids and error chains with the user's profile path redacted. It
//! never records transcripts, audio or settings values.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

const LOG_FILE: &str = "lifecycle.log";
const ROTATED_FILE: &str = "lifecycle.log.1";
const MAX_LOG_BYTES: u64 = 256 * 1024;

/// Returns `None` for the normal profile and a stable scope for any other
/// settings file, so an isolated `--config` profile owns its own instance slot.
pub fn profile_instance_scope(config: &Path, default: &Path) -> Option<String> {
    let config = comparable_path(config);
    if config == comparable_path(default) {
        return None;
    }
    let digest = Sha256::digest(config.as_bytes());
    Some(
        digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn comparable_path(path: &Path) -> String {
    // The settings file may not exist yet; its folder usually does.
    let resolved = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    };
    let text = resolved.to_string_lossy().replace('/', "\\");
    text.strip_prefix(r"\\?\")
        .unwrap_or(&text)
        .to_lowercase()
}

#[derive(Clone, Debug)]
pub struct LifecycleLog {
    path: PathBuf,
}

impl LifecycleLog {
    /// The log lives in `logs/` next to the settings file of the active profile.
    pub fn for_settings(settings_path: &Path) -> Option<Self> {
        Some(Self {
            path: settings_path.parent()?.join("logs").join(LOG_FILE),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one line. Logging is best effort and never fails the caller.
    pub fn record(&self, event: &str) {
        let _ = self.try_record(event);
    }

    fn try_record(&self, event: &str) -> std::io::Result<()> {
        let directory = self.path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
        fs::create_dir_all(directory)?;
        if fs::metadata(&self.path).is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES) {
            let _ = fs::rename(&self.path, directory.join(ROTATED_FILE));
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let line = redact_profile_paths(&event.replace(['\r', '\n'], " "));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "ts_ms={now_ms} pid={} {line}", std::process::id())
    }
}

/// Formats an error chain for the lifecycle log.
pub fn error_chain(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

fn redact_profile_paths(text: &str) -> String {
    let Some(profile) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) else {
        return text.to_owned();
    };
    redact(text, &profile.to_string_lossy(), "%USERPROFILE%")
}

fn redact(text: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return text.to_owned();
    }
    let lower_text = text.to_lowercase();
    let lower_needle = needle.to_lowercase();
    // Lowercasing can change byte lengths for some scripts; fall back to an
    // exact-case replacement rather than slicing at a wrong boundary.
    if lower_text.len() != text.len() || lower_needle.len() != needle.len() {
        return text.replace(needle, replacement);
    }
    let mut output = String::with_capacity(text.len());
    let mut rest = 0;
    while let Some(found) = lower_text[rest..].find(&lower_needle) {
        let start = rest + found;
        output.push_str(&text[rest..start]);
        output.push_str(replacement);
        rest = start + needle.len();
    }
    output.push_str(&text[rest..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_keeps_the_shared_slot_and_others_are_scoped() {
        let directory = tempfile::tempdir().unwrap();
        let default = directory.path().join("Phorminx").join("settings.toml");
        fs::create_dir_all(default.parent().unwrap()).unwrap();
        assert_eq!(profile_instance_scope(&default, &default), None);
        let spelled_differently =
            PathBuf::from(default.to_string_lossy().to_uppercase().replace('\\', "/"));
        assert_eq!(profile_instance_scope(&spelled_differently, &default), None);

        let qa = directory.path().join("qa").join("settings.toml");
        let scope = profile_instance_scope(&qa, &default).unwrap();
        assert_eq!(scope.len(), 16);
        assert_eq!(profile_instance_scope(&qa, &default), Some(scope.clone()));
        let other = directory.path().join("qa2").join("settings.toml");
        assert_ne!(profile_instance_scope(&other, &default), Some(scope));
    }

    #[test]
    fn log_appends_redacts_and_rotates() {
        let directory = tempfile::tempdir().unwrap();
        let log = LifecycleLog::for_settings(&directory.path().join("settings.toml")).unwrap();
        log.record("event=runtime_exit restart_requested=true\nsecond");
        let text = fs::read_to_string(log.path()).unwrap();
        assert!(text.contains("event=runtime_exit restart_requested=true second"));
        assert_eq!(text.lines().count(), 1);

        fs::write(log.path(), vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();
        log.record("event=after_rotation");
        assert!(log.path().with_file_name(ROTATED_FILE).is_file());
        assert!(fs::read_to_string(log.path()).unwrap().contains("event=after_rotation"));
    }

    #[test]
    fn profile_paths_are_redacted_case_insensitively() {
        assert_eq!(
            redact(r"failed at C:\Users\Mau\x and c:\users\mau\y", r"C:\Users\Mau", "%USERPROFILE%"),
            r"failed at %USERPROFILE%\x and %USERPROFILE%\y"
        );
        assert_eq!(redact("unchanged", "", "x"), "unchanged");
    }
}

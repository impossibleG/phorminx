use std::fmt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::w;

pub const OLLAMA_OFFICIAL_WINDOWS_DOWNLOAD: &str = "https://ollama.com/download/windows";
const OLLAMA_RELATIVE_PATH: &str = r"Programs\Ollama\ollama.exe";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomaticOllamaInstallAvailability {
    Unavailable(AutomaticOllamaInstallUnavailable),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomaticOllamaInstallUnavailable {
    MutableInstallerHasNoPinnedAuthority,
}

/// Automatic installation intentionally fails closed until an official stable
/// digest or signer contract can be compiled into Phorminx.
#[must_use]
pub const fn ollama_automatic_install_availability() -> AutomaticOllamaInstallAvailability {
    AutomaticOllamaInstallAvailability::Unavailable(
        AutomaticOllamaInstallUnavailable::MutableInstallerHasNoPinnedAuthority,
    )
}

/// Result of checking only Ollama's documented per-user installation location.
pub enum OllamaInstallation {
    Missing,
    Present,
    Unsafe,
}

impl fmt::Debug for OllamaInstallation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("Missing"),
            Self::Present => formatter.write_str("Present"),
            Self::Unsafe => formatter.write_str("Unsafe"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum OllamaInstallationError {
    #[error("Windows could not resolve the per-user application directory")]
    KnownFolderUnavailable,
    #[error("the per-user application directory is invalid")]
    InvalidKnownFolder,
    #[error("the Ollama installation could not be inspected")]
    InspectionFailed,
}

/// Resolve LocalAppData through the Windows Known Folder API. Environment
/// variables, PATH, the process current directory, and the registry are not used.
pub fn inspect_ollama_installation() -> Result<OllamaInstallation, OllamaInstallationError> {
    let local_app_data = known_local_app_data()?;
    inspect_at(&local_app_data)
}

fn known_local_app_data() -> Result<PathBuf, OllamaInstallationError> {
    // SAFETY: The folder id is a valid static GUID and no impersonation token is used.
    let raw = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }
        .map_err(|_| OllamaInstallationError::KnownFolderUnavailable)?;
    // SAFETY: SHGetKnownFolderPath returns a NUL-terminated allocation owned by CoTaskMem.
    let text = unsafe { raw.to_string() }.map_err(|_| OllamaInstallationError::InvalidKnownFolder);
    // SAFETY: `raw` was allocated by SHGetKnownFolderPath and is released exactly once.
    unsafe { CoTaskMemFree(Some(raw.0.cast())) };
    let path = PathBuf::from(text?);
    if !absolute_drive_path(&path) {
        return Err(OllamaInstallationError::InvalidKnownFolder);
    }
    Ok(path)
}

fn inspect_at(local_app_data: &Path) -> Result<OllamaInstallation, OllamaInstallationError> {
    if !absolute_drive_path(local_app_data) {
        return Err(OllamaInstallationError::InvalidKnownFolder);
    }
    let executable = local_app_data.join(OLLAMA_RELATIVE_PATH);
    let metadata = match std::fs::symlink_metadata(&executable) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OllamaInstallation::Missing);
        }
        Err(_) => return Err(OllamaInstallationError::InspectionFailed),
    };
    if !metadata.is_file() || is_reparse(&metadata) {
        return Ok(OllamaInstallation::Unsafe);
    }

    // Reject junctions/symlinks in every mutable descendant between the known
    // folder and executable. LocalAppData itself is the OS-provided trust root.
    let mut cursor = local_app_data.to_path_buf();
    for component in ["Programs", "Ollama"] {
        cursor.push(component);
        let metadata = std::fs::symlink_metadata(&cursor)
            .map_err(|_| OllamaInstallationError::InspectionFailed)?;
        if !metadata.is_dir() || is_reparse(&metadata) {
            return Ok(OllamaInstallation::Unsafe);
        }
    }
    let canonical_root = std::fs::canonicalize(local_app_data)
        .map_err(|_| OllamaInstallationError::InspectionFailed)?;
    let canonical_executable = std::fs::canonicalize(&executable)
        .map_err(|_| OllamaInstallationError::InspectionFailed)?;
    if !canonical_executable.starts_with(&canonical_root) {
        return Ok(OllamaInstallation::Unsafe);
    }
    // Do not return an executable path or spawn capability. LocalAppData is
    // user-writable and no signer identity is pinned, so a file could be
    // replaced after this observation. Phorminx requires the user to start
    // Ollama and interacts only with its loopback API.
    Ok(OllamaInstallation::Present)
}

fn absolute_drive_path(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Prefix(_)))
        && matches!(components.next(), Some(Component::RootDir))
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

/// An explicit review of the only installer flow Phorminx currently trusts.
///
/// Ollama publishes a mutable `OllamaSetup.exe` but no stable digest or signing
/// identity that Phorminx can pin. Consequently this action opens the official
/// page and never downloads, executes, elevates, or enables autostart itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OllamaInstallReview {
    confirmation: String,
}

impl OllamaInstallReview {
    #[must_use]
    pub fn manual() -> Self {
        Self {
            confirmation: "Open Ollama's official Windows download page".to_owned(),
        }
    }

    #[must_use]
    pub fn confirmation(&self) -> &str {
        &self.confirmation
    }

    pub fn authorize(
        self,
        exact_confirmation: &str,
    ) -> Result<OllamaManualInstallAction, OllamaInstallConsentError> {
        if exact_confirmation != self.confirmation {
            return Err(OllamaInstallConsentError);
        }
        Ok(OllamaManualInstallAction(()))
    }
}

#[derive(Debug)]
pub struct OllamaManualInstallAction(());

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("opening the Ollama download page was not explicitly confirmed")]
pub struct OllamaInstallConsentError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum OpenOllamaDownloadError {
    #[error("Windows could not open the official Ollama download page")]
    ShellOpenFailed,
}

pub fn open_official_ollama_download(
    _action: OllamaManualInstallAction,
) -> Result<(), OpenOllamaDownloadError> {
    // SAFETY: All strings are fixed, NUL-terminated constants. No command-line
    // arguments, file paths, or user-provided text reach ShellExecuteW.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("https://ollama.com/download/windows"),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize <= 32 {
        Err(OpenOllamaDownloadError::ShellOpenFailed)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OllamaUninstallGuidance {
    pub use_windows_installed_apps: bool,
    pub downloaded_models_may_remain: bool,
    pub custom_model_location_is_not_removed: bool,
}

impl Default for OllamaUninstallGuidance {
    fn default() -> Self {
        Self {
            use_windows_installed_apps: true,
            downloaded_models_may_remain: true,
            custom_model_location_is_not_removed: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_location_does_not_consult_path_or_current_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("ollama.exe"), b"spoof").unwrap();
        assert!(matches!(
            inspect_at(Path::new("relative")),
            Err(OllamaInstallationError::InvalidKnownFolder)
        ));
        // A process-local executable is irrelevant: only the absolute known
        // folder descendant can be inspected.
        assert!(matches!(
            inspect_at(root.path()),
            Ok(OllamaInstallation::Missing)
        ));
    }

    #[test]
    fn manual_installer_requires_exact_confirmation_and_cannot_hold_a_path() {
        let review = OllamaInstallReview::manual();
        assert!(review.clone().authorize("yes").is_err());
        assert!(
            review
                .clone()
                .authorize(&format!("{} ", review.confirmation()))
                .is_err()
        );
        let action = review.clone().authorize(review.confirmation()).unwrap();
        assert_eq!(format!("{action:?}"), "OllamaManualInstallAction(())");
        assert!(OLLAMA_OFFICIAL_WINDOWS_DOWNLOAD.starts_with("https://ollama.com/"));
        assert_eq!(
            ollama_automatic_install_availability(),
            AutomaticOllamaInstallAvailability::Unavailable(
                AutomaticOllamaInstallUnavailable::MutableInstallerHasNoPinnedAuthority
            )
        );
    }

    #[test]
    fn uninstall_disclosure_never_claims_model_cleanup() {
        let guidance = OllamaUninstallGuidance::default();
        assert!(guidance.use_windows_installed_apps);
        assert!(guidance.downloaded_models_may_remain);
        assert!(guidance.custom_model_location_is_not_removed);
    }

    #[test]
    fn inspection_never_grants_authority_to_execute_a_replaceable_file() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("Programs").join("Ollama");
        std::fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("ollama.exe");
        std::fs::write(&executable, b"first").unwrap();
        assert!(matches!(
            inspect_at(root.path()),
            Ok(OllamaInstallation::Present)
        ));
        std::fs::write(&executable, b"replacement").unwrap();
        // The earlier result contains neither a path nor an executable handle.
        assert_eq!(format!("{:?}", OllamaInstallation::Present), "Present");
    }
}

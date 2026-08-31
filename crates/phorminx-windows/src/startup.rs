use std::{ffi::OsStr, path::Path};

use thiserror::Error;
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, REG_VALUE_TYPE,
            RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
            RegSetValueExW,
        },
    },
    core::PCWSTR,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Phorminx";
const MAX_REGISTRY_VALUE_BYTES: u32 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchAtLoginState {
    Disabled,
    Enabled,
    DifferentCommand(String),
}

#[derive(Debug, Error)]
pub enum LaunchAtLoginError {
    #[error("the executable path cannot be empty")]
    EmptyExecutablePath,
    #[error("the executable path contains a NUL character")]
    NulInExecutablePath,
    #[error("the executable path contains a quotation mark")]
    QuoteInExecutablePath,
    #[error("the executable path must be absolute")]
    RelativeExecutablePath,
    #[error("the executable path is not valid Unicode")]
    NonUnicodeExecutablePath,
    #[error("the launch-at-login registry value is too large ({0} bytes)")]
    RegistryValueTooLarge(u32),
    #[error("the launch-at-login registry value has unsupported type {0}")]
    UnsupportedRegistryType(u32),
    #[error("the launch-at-login registry value contains invalid UTF-16")]
    InvalidRegistryText,
    #[error("Windows registry operation {operation} failed with code {code}")]
    Registry { operation: &'static str, code: u32 },
}

/// Returns the per-user launch-at-login state without modifying it.
pub fn launch_at_login_state(
    executable_path: &Path,
) -> Result<LaunchAtLoginState, LaunchAtLoginError> {
    let expected = command_for_executable(executable_path)?;
    let Some(key) = open_run_key(KEY_QUERY_VALUE)? else {
        return Ok(LaunchAtLoginState::Disabled);
    };
    match query_string_value(key.0) {
        Ok(Some(actual)) if actual == expected => Ok(LaunchAtLoginState::Enabled),
        Ok(Some(actual)) => Ok(LaunchAtLoginState::DifferentCommand(actual)),
        Ok(None) => Ok(LaunchAtLoginState::Disabled),
        Err(error) => Err(error),
    }
}

/// Enables or disables Phorminx launch-at-login for the current Windows user.
///
/// Enabling writes only `HKCU\...\Run\Phorminx`; it never requests elevation.
/// Disabling deletes only that named value and treats an absent value as success.
pub fn set_launch_at_login(
    executable_path: &Path,
    enabled: bool,
) -> Result<(), LaunchAtLoginError> {
    if enabled {
        let command = command_for_executable(executable_path)?;
        let key = match open_run_key(KEY_SET_VALUE)? {
            Some(key) => key,
            None => create_run_key()?,
        };
        set_string_value(key.0, &command)
    } else {
        match open_run_key(KEY_SET_VALUE)? {
            Some(key) => delete_value(key.0),
            None => Ok(()),
        }
    }
}

fn command_for_executable(executable_path: &Path) -> Result<String, LaunchAtLoginError> {
    if executable_path.as_os_str().is_empty() {
        return Err(LaunchAtLoginError::EmptyExecutablePath);
    }
    if !executable_path.is_absolute() {
        return Err(LaunchAtLoginError::RelativeExecutablePath);
    }
    let path = executable_path
        .to_str()
        .ok_or(LaunchAtLoginError::NonUnicodeExecutablePath)?;
    if path.contains('\0') {
        return Err(LaunchAtLoginError::NulInExecutablePath);
    }
    if path.contains('"') {
        return Err(LaunchAtLoginError::QuoteInExecutablePath);
    }
    Ok(format!("\"{path}\""))
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: The handle was returned by RegOpenKeyExW and is owned by this guard.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open_run_key(
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Result<Option<RegistryKey>, LaunchAtLoginError> {
    let key_name = wide_null(OsStr::new(RUN_KEY));
    let mut key = HKEY::default();
    // SAFETY: key_name is NUL-terminated and key points to valid writable storage.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_name.as_ptr()),
            None,
            access,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(status, "open HKCU Run key")?;
    Ok(Some(RegistryKey(key)))
}

fn create_run_key() -> Result<RegistryKey, LaunchAtLoginError> {
    let key_name = wide_null(OsStr::new(RUN_KEY));
    let mut key = HKEY::default();
    // SAFETY: key_name is NUL-terminated and key points to valid writable storage.
    let status = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(key_name.as_ptr()), &mut key) };
    check(status, "create HKCU Run key")?;
    Ok(RegistryKey(key))
}

fn query_string_value(key: HKEY) -> Result<Option<String>, LaunchAtLoginError> {
    let value_name = wide_null(OsStr::new(VALUE_NAME));
    let mut value_type = REG_VALUE_TYPE::default();
    let mut byte_count = 0_u32;
    // SAFETY: value_name is NUL-terminated; output pointers reference valid storage.
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut value_type),
            None,
            Some(&mut byte_count),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(status, "query Phorminx launch value size")?;
    if value_type != REG_SZ {
        return Err(LaunchAtLoginError::UnsupportedRegistryType(value_type.0));
    }
    if byte_count > MAX_REGISTRY_VALUE_BYTES {
        return Err(LaunchAtLoginError::RegistryValueTooLarge(byte_count));
    }
    if byte_count == 0 {
        return Ok(Some(String::new()));
    }

    let mut bytes = vec![0_u8; byte_count as usize];
    // SAFETY: bytes is writable for byte_count bytes; other pointers remain valid.
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut value_type),
            Some(bytes.as_mut_ptr()),
            Some(&mut byte_count),
        )
    };
    check(status, "read Phorminx launch value")?;
    if value_type != REG_SZ {
        return Err(LaunchAtLoginError::UnsupportedRegistryType(value_type.0));
    }
    bytes.truncate(byte_count as usize);
    decode_registry_string(&bytes).map(Some)
}

fn set_string_value(key: HKEY, value: &str) -> Result<(), LaunchAtLoginError> {
    let value_name = wide_null(OsStr::new(VALUE_NAME));
    let wide_value: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_count = wide_value.len().saturating_mul(size_of::<u16>());
    if byte_count > MAX_REGISTRY_VALUE_BYTES as usize {
        return Err(LaunchAtLoginError::RegistryValueTooLarge(
            byte_count.min(u32::MAX as usize) as u32,
        ));
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(
            wide_value.as_ptr().cast::<u8>(),
            wide_value.len() * size_of::<u16>(),
        )
    };
    // SAFETY: both the name and string data are NUL-terminated and valid for the call.
    let status =
        unsafe { RegSetValueExW(key, PCWSTR(value_name.as_ptr()), None, REG_SZ, Some(bytes)) };
    check(status, "write Phorminx launch value")
}

fn delete_value(key: HKEY) -> Result<(), LaunchAtLoginError> {
    let value_name = wide_null(OsStr::new(VALUE_NAME));
    // SAFETY: value_name is a valid NUL-terminated UTF-16 string.
    let status = unsafe { RegDeleteValueW(key, PCWSTR(value_name.as_ptr())) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(status, "delete Phorminx launch value")
}

fn check(status: WIN32_ERROR, operation: &'static str) -> Result<(), LaunchAtLoginError> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(LaunchAtLoginError::Registry {
            operation,
            code: status.0,
        })
    }
}

fn decode_registry_string(bytes: &[u8]) -> Result<String, LaunchAtLoginError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(LaunchAtLoginError::InvalidRegistryText);
    }
    let mut wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    while wide.last() == Some(&0) {
        wide.pop();
    }
    String::from_utf16(&wide).map_err(|_| LaunchAtLoginError::InvalidRegistryText)
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_command_quotes_paths_with_spaces() {
        let command =
            command_for_executable(Path::new(r"C:\Users\A User\Phorminx\phorminx-app.exe"))
                .expect("command should be valid");
        assert_eq!(command, r#""C:\Users\A User\Phorminx\phorminx-app.exe""#);
    }

    #[test]
    fn empty_executable_path_is_rejected() {
        assert!(matches!(
            command_for_executable(Path::new("")),
            Err(LaunchAtLoginError::EmptyExecutablePath)
        ));
    }

    #[test]
    fn relative_executable_path_is_rejected() {
        assert!(matches!(
            command_for_executable(Path::new("phorminx-app.exe")),
            Err(LaunchAtLoginError::RelativeExecutablePath)
        ));
    }

    #[test]
    fn quotation_mark_in_executable_path_is_rejected() {
        assert!(matches!(
            command_for_executable(Path::new(r#"C:\Phorminx\bad"name.exe"#)),
            Err(LaunchAtLoginError::QuoteInExecutablePath)
        ));
    }

    #[test]
    fn registry_string_decoding_trims_nul_terminators() {
        let bytes: Vec<u8> = "hello"
            .encode_utf16()
            .chain([0, 0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(decode_registry_string(&bytes).unwrap(), "hello");
    }

    #[test]
    fn malformed_registry_string_is_rejected() {
        assert!(matches!(
            decode_registry_string(&[0_u8]),
            Err(LaunchAtLoginError::InvalidRegistryText)
        ));
        let unpaired_surrogate = 0xD800_u16.to_le_bytes();
        assert!(matches!(
            decode_registry_string(&unpaired_surrogate),
            Err(LaunchAtLoginError::InvalidRegistryText)
        ));
    }
}

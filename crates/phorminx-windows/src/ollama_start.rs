//! Starting a locally installed daemon is separate from trusting its models.
//! Never search PATH or execute an arbitrary configured binary.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum OllamaStartError {
    #[error("Ollama is not installed in its official per-user location")]
    Missing,
    #[error("Ollama's executable could not be verified as signed by Ollama Inc.")]
    Unverified,
    #[error("Windows could not start the verified local Ollama service")]
    StartFailed,
    #[error("Ollama startup was cancelled")]
    Cancelled,
    #[error("Ollama executable verification timed out")]
    TimedOut,
}

// A fixed script, without interpolation of paths or other caller input. The
// official Windows installer also verifies the exact O=Ollama Inc. certificate
// organization. Hold a non-write/non-delete-shared handle through validation
// and process creation so the verified file cannot be replaced in between.
const START_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$guard = $null
try {
    $root = [Environment]::GetFolderPath('LocalApplicationData')
    if ($root -notmatch '^[A-Za-z]:\\') { exit 11 }
    $exe = [IO.Path]::Combine($root, 'Programs\Ollama\ollama.exe')
    if (-not [IO.File]::Exists($exe)) { exit 10 }
    foreach ($relative in @('Programs', 'Programs\Ollama', 'Programs\Ollama\ollama.exe')) {
        $attrs = [IO.File]::GetAttributes([IO.Path]::Combine($root, $relative))
        if (($attrs -band [IO.FileAttributes]::ReparsePoint) -ne 0) { exit 11 }
    }
    $guard = [IO.File]::Open($exe, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $signature = Microsoft.PowerShell.Security\Get-AuthenticodeSignature -LiteralPath $exe
    if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate -or
        $signature.SignerCertificate.Subject -notmatch '(^|, )O=Ollama Inc\.(,|$)') { exit 11 }
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $exe
    $info.Arguments = 'serve'
    $info.WorkingDirectory = [IO.Path]::GetDirectoryName($exe)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $info.EnvironmentVariables['OLLAMA_HOST'] = '127.0.0.1:11434'
    $process = [Diagnostics.Process]::Start($info)
    if ($null -eq $process) { exit 12 }
    $process.Dispose()
    exit 0
} catch { exit 12 } finally { if ($null -ne $guard) { $guard.Dispose() } }
"#;

/// Verify Authenticode and launch the official per-user Ollama binary, hidden
/// and bound to loopback. Success means process creation, not API/model readiness.
/// The caller must probe the API and verify its selected model identity.
pub fn start_verified_ollama(cancel: &AtomicBool) -> Result<(), OllamaStartError> {
    if cancel.load(Ordering::Acquire) {
        return Err(OllamaStartError::Cancelled);
    }
    let powershell = crate::windows_system_directory()
        .map_err(|_| OllamaStartError::StartFailed)?
        .join("WindowsPowerShell\\v1.0\\powershell.exe");
    let trusted_modules = powershell
        .parent()
        .ok_or(OllamaStartError::StartFailed)?
        .join("Modules");
    let mut child = Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            START_SCRIPT,
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .env("PSModulePath", trusted_modules)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| OllamaStartError::StartFailed)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return start_exit_result(status.code()),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OllamaStartError::StartFailed);
            }
            Ok(None) => {}
        }
        let failure = if cancel.load(Ordering::Acquire) {
            Some(OllamaStartError::Cancelled)
        } else if Instant::now() >= deadline {
            Some(OllamaStartError::TimedOut)
        } else {
            None
        };
        if let Some(error) = failure {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn start_exit_result(code: Option<i32>) -> Result<(), OllamaStartError> {
    match code {
        Some(0) => Ok(()),
        Some(10) => Err(OllamaStartError::Missing),
        Some(11) => Err(OllamaStartError::Unverified),
        _ => Err(OllamaStartError::StartFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_start_never_spawns_a_process() {
        assert_eq!(
            start_verified_ollama(&AtomicBool::new(true)),
            Err(OllamaStartError::Cancelled)
        );
    }

    #[test]
    fn only_verified_success_is_accepted() {
        assert!(start_exit_result(Some(0)).is_ok());
        assert_eq!(start_exit_result(Some(10)), Err(OllamaStartError::Missing));
        assert_eq!(
            start_exit_result(Some(11)),
            Err(OllamaStartError::Unverified)
        );
        for code in [None, Some(1), Some(12), Some(-1)] {
            assert_eq!(start_exit_result(code), Err(OllamaStartError::StartFailed));
        }
    }

    #[test]
    fn launch_contract_is_fixed_hidden_and_loopback_only() {
        assert!(START_SCRIPT.contains("[IO.FileShare]::Read"));
        assert!(START_SCRIPT.contains("Get-AuthenticodeSignature -LiteralPath $exe"));
        assert!(START_SCRIPT.contains("O=Ollama Inc\\."));
        assert!(START_SCRIPT.contains("'127.0.0.1:11434'"));
        assert!(START_SCRIPT.contains("$info.CreateNoWindow = $true"));
        assert!(!START_SCRIPT.contains("$env:PATH"));
    }
}

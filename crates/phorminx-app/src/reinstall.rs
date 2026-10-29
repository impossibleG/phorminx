//! "Reinstall Phorminx": put a fresh copy of the signed release in place and
//! reset app state, while keeping everything the user made. Nothing is
//! deleted: reset state is moved into `backups\reinstall-<UTC>\state` so the
//! previous settings can be restored.
use crate::updates::{self, Release};
use phorminx_assistant::ConfigStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Data-directory entries a reinstall moves into the backup. Names are
/// matched case-insensitively at the top level of the data directory.
pub const RESET: &[&str] = &[
    "settings.toml",
    "assistant.json",
    "settings.schema-3.backup.toml",
    "performance-recommendation-rollback.toml",
    "setup-status.json",
    "setup-downloads",
    "managed-setup-v1",
    "runtime",
    "window-geometry.json",
    "eframe-v2.ron",
    "temp",
];

/// Inside `temp`, the encrypted dictation audio spool is left in place.
pub const TEMP_KEEP: &[&str] = &["audio-spool"];

/// Entries that are user data or must survive a reinstall. Anything not in
/// [`RESET`] is kept as well; this list documents the deliberate cases.
pub const KEEP: &[&str] = &[
    "phorminx.db",
    "phorminx.db-wal",
    "phorminx.db-shm",
    "phorminx.db-journal",
    "investigations",
    "tor",
    "models",
    "ocr",
    "backups",
    "updates",
    "logs",
    "meeting-scratch",
    "feedback-scratch",
    "performance-evidence.json",
];

/// The WebView2 profile lives beside, not inside, the data directory.
pub const WEBVIEW_DIRECTORY: &str = "net.phorminx.app";

const BACKUPS: &str = "backups";
const PREFIX: &str = "reinstall-";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    Reset,
    Keep,
}

/// What a reinstall does with one top-level entry of the data directory.
/// Unknown entries are kept.
pub fn classify(name: &str) -> Disposition {
    if RESET.iter().any(|reset| reset.eq_ignore_ascii_case(name)) {
        Disposition::Reset
    } else {
        Disposition::Keep
    }
}

static AFTER_REINSTALL: AtomicBool = AtomicBool::new(false);
static AUTO_SETUP: AtomicBool = AtomicBool::new(false);

/// Called once at startup when the reinstall helper relaunched Phorminx
/// after a completed reinstall: setup then starts on its own.
pub fn mark_after_reinstall() {
    AFTER_REINSTALL.store(true, Ordering::Release);
    AUTO_SETUP.store(true, Ordering::Release);
}

pub fn after_reinstall() -> bool {
    AFTER_REINSTALL.load(Ordering::Acquire)
}

/// True exactly once after a completed reinstall.
pub fn take_auto_setup() -> bool {
    AUTO_SETUP.swap(false, Ordering::AcqRel)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Manifest {
    pub schema: u8,
    pub created_at: u64,
    pub from_version: String,
    pub to_version: String,
    pub program_dir: String,
    pub reset: Vec<String>,
    pub keep: Vec<String>,
    pub restored_at: Option<u64>,
    pub dismissed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Receipt {
    pub status: String,
    pub detail: String,
    pub version: String,
    pub moved: Vec<String>,
    pub not_moved: Vec<String>,
    pub finished_at: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackupView {
    pub id: String,
    pub created_at: u64,
    pub from_version: String,
    pub to_version: String,
    /// `pending`, `installed`, `installation_failed`, …
    pub status: String,
    pub message: String,
    pub has_settings: bool,
    pub has_assistant: bool,
    pub restored: bool,
    pub dismissed: bool,
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// `20261028T203000Z` for a Unix time.
pub fn utc_stamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Howard Hinnant's days-to-civil conversion.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn valid_backup_id(id: &str) -> bool {
    let Some(stamp) = id.strip_prefix(PREFIX) else {
        return false;
    };
    let bytes = stamp.as_bytes();
    bytes.len() == 16
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
}

pub fn backups_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(BACKUPS)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| "Could not write the backup record.")?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes).map_err(|_| "Could not write the backup record.")?;
    if path.exists() {
        phorminx_windows::atomic_replace_file(&temporary, path)
            .map_err(|_| "Could not write the backup record.")?;
    } else {
        std::fs::rename(&temporary, path).map_err(|_| "Could not write the backup record.")?;
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() <= 64 * 1024)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Create the empty backup folder and its manifest before handing off.
pub fn create_backup(
    data_dir: &Path,
    now: u64,
    from_version: &str,
    to_version: &str,
    program_dir: &Path,
) -> Result<PathBuf, String> {
    let backups = backups_dir(data_dir);
    updates::reject_links(&backups).map_err(|_| "The backup folder is not safe to use.")?;
    std::fs::create_dir_all(&backups).map_err(|_| "Could not create the backup folder.")?;
    // Two reinstalls in one second still get separate folders.
    let mut time = now;
    let directory = loop {
        let candidate = backups.join(format!("{PREFIX}{}", utc_stamp(time)));
        match std::fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && time < now + 60 => {
                time += 1;
            }
            Err(_) => return Err("Could not create the backup folder.".into()),
        }
    };
    std::fs::create_dir(directory.join("state")).map_err(|_| "Could not create the backup folder.")?;
    let manifest = Manifest {
        schema: 1,
        created_at: now,
        from_version: from_version.to_owned(),
        to_version: to_version.to_owned(),
        program_dir: program_dir.display().to_string(),
        reset: RESET.iter().map(|name| (*name).to_owned()).collect(),
        keep: KEEP.iter().map(|name| (*name).to_owned()).collect(),
        restored_at: None,
        dismissed: false,
    };
    write_json(&directory.join("manifest.json"), &manifest)?;
    Ok(directory)
}

fn receipt_message(status: &str) -> &'static str {
    match status {
        "installed" => "Phorminx was reinstalled. Settings were reset to defaults.",
        "pending" | "started" => "The reinstall did not report back. Your data was not deleted.",
        "verification_failed" => "The downloaded installer failed its check, so nothing was changed.",
        "program_busy" => "The Phorminx program folder was in use, so nothing was changed.",
        _ => "The installer did not finish. The previous Phorminx was put back and your settings were not changed.",
    }
}

/// Reinstall backups, newest first.
pub fn list_backups(data_dir: &Path) -> Vec<BackupView> {
    let Ok(entries) = std::fs::read_dir(backups_dir(data_dir)) else {
        return Vec::new();
    };
    let mut views: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_backup_id(&id) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                return None;
            }
            let path = entry.path();
            let manifest: Manifest = read_json(&path.join("manifest.json"))?;
            let receipt: Option<Receipt> = read_json(&path.join("receipt.json"));
            let status = receipt.map_or_else(|| "pending".to_owned(), |receipt| receipt.status);
            Some(BackupView {
                message: receipt_message(&status).into(),
                status,
                created_at: manifest.created_at,
                from_version: manifest.from_version,
                to_version: manifest.to_version,
                has_settings: path.join("state").join("settings.toml").is_file(),
                has_assistant: path.join("state").join("assistant.json").is_file(),
                restored: manifest.restored_at.is_some(),
                dismissed: manifest.dismissed,
                id,
            })
        })
        .collect();
    views.sort_by(|a, b| b.id.cmp(&a.id));
    views
}

/// The newest reinstall's receipt status, if one exists.
pub fn latest_status(data_dir: &Path) -> Option<String> {
    list_backups(data_dir).into_iter().next().map(|backup| backup.status)
}

fn update_manifest(data_dir: &Path, id: &str, change: impl FnOnce(&mut Manifest)) -> Result<(), String> {
    if !valid_backup_id(id) {
        return Err("That backup does not exist.".into());
    }
    let path = backups_dir(data_dir).join(id).join("manifest.json");
    let mut manifest: Manifest = read_json(&path).ok_or("That backup does not exist.")?;
    change(&mut manifest);
    write_json(&path, &manifest)
}

pub fn dismiss(data_dir: &Path, id: &str) -> Result<(), String> {
    update_manifest(data_dir, id, |manifest| manifest.dismissed = true)
}

/// Remove `<program>.old-*` folders a finished reinstall could not delete.
pub fn remove_leftover_program_copies(program_dir: &Path) {
    let (Some(parent), Some(name)) = (program_dir.parent(), program_dir.file_name()) else {
        return;
    };
    let prefix = format!("{}.old-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        if entry_name.starts_with(&prefix)
            && entry.file_type().is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// Ollama models the restored settings use.
    pub models: Vec<String>,
}

/// Put the backed-up settings back. Both files are validated first; the
/// current files are moved into the backup, never deleted.
pub fn restore(data_dir: &Path, id: &str) -> Result<RestoreOutcome, String> {
    if !valid_backup_id(id) {
        return Err("That backup does not exist.".into());
    }
    let backup = backups_dir(data_dir).join(id);
    updates::reject_links(&backup).map_err(|_| "That backup is not safe to read.")?;
    let state = backup.join("state");
    let settings_source = state.join("settings.toml");
    if !settings_source.is_file() {
        return Err("This backup has no settings to restore.".into());
    }
    let assistant_source = state.join("assistant.json");
    let has_assistant = assistant_source.is_file();

    // Validate copies, so loading (which may migrate) never touches the backup.
    let check = backup.join(format!("restore-check-{}", unix_now()));
    std::fs::create_dir_all(&check).map_err(|_| "Could not check the saved settings.")?;
    let validated = (|| {
        let settings_copy = check.join("settings.toml");
        std::fs::copy(&settings_source, &settings_copy)
            .map_err(|_| "Could not read the saved settings.")?;
        let settings = crate::settings::SettingsStore::new(settings_copy)
            .and_then(|store| store.load())
            .map_err(|_| "The saved settings are damaged, so they were not restored.")?;
        let assistant = if has_assistant {
            let copy = check.join("assistant.json");
            std::fs::copy(&assistant_source, &copy)
                .map_err(|_| "Could not read the saved meeting settings.")?;
            ConfigStore::new(copy)
                .load()
                .map_err(|_| "The saved meeting settings are damaged, so they were not restored.")?
        } else {
            Default::default()
        };
        Ok::<_, String>(ollama_models(&settings, &assistant))
    })();
    let _ = std::fs::remove_dir_all(&check);
    let models = validated?;

    // Move whatever is there now aside, then copy the saved files in.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos());
    let replaced = backup.join(format!("replaced-{}-{nonce}", utc_stamp(unix_now())));
    std::fs::create_dir(&replaced).map_err(|_| "Could not prepare the restore.")?;
    let names: &[&str] = if has_assistant {
        &["settings.toml", "assistant.json"]
    } else {
        &["settings.toml"]
    };
    let mut moved = Vec::new();
    let mut result = Ok(());
    for name in names {
        let current = data_dir.join(name);
        if current.exists() {
            if std::fs::rename(&current, replaced.join(name)).is_err() {
                result = Err("A settings file is in use, so nothing was restored.".to_owned());
                break;
            }
            moved.push(*name);
        }
    }
    if result.is_ok() {
        for name in names {
            let temporary = data_dir.join(format!(".{name}.restore.tmp"));
            let copied = std::fs::copy(state.join(name), &temporary)
                .and_then(|_| std::fs::rename(&temporary, data_dir.join(name)));
            if copied.is_err() {
                let _ = std::fs::remove_file(&temporary);
                result = Err("Could not copy the saved settings back.".to_owned());
                break;
            }
        }
    }
    if result.is_err() {
        // Put the files that were there before back in place.
        for name in names {
            if moved.contains(name) {
                let target = data_dir.join(name);
                if target.exists() {
                    let _ = std::fs::rename(&target, replaced.join(format!("{name}.partial")));
                }
                let _ = std::fs::rename(replaced.join(name), &target);
            }
        }
        result?;
    }
    update_manifest(data_dir, id, |manifest| manifest.restored_at = Some(unix_now()))?;
    Ok(RestoreOutcome { models })
}

/// The Ollama models a settings pair relies on, without duplicates.
pub fn ollama_models(
    settings: &crate::settings::Settings,
    assistant: &phorminx_assistant::AssistantConfig,
) -> Vec<String> {
    let mut models: Vec<String> = Vec::new();
    for (model, feature) in crate::model_registry::uses(settings, assistant) {
        if !feature.starts_with("Dictation") && !models.contains(&model) {
            models.push(model);
        }
    }
    models
}

/// A verified installer ready to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedInstaller {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub version: String,
}

/// Everything the helper needs once Phorminx has exited.
#[derive(Clone, Debug)]
pub struct HelperPlan {
    pub pid: u32,
    pub installer: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub version: String,
    pub installer_args: String,
    pub program_dir: PathBuf,
    pub data_dir: PathBuf,
    /// `%LOCALAPPDATA%\net.phorminx.app`, the WebView2 profile.
    pub webview_dir: Option<PathBuf>,
    pub backup_dir: PathBuf,
    pub stamp: String,
    /// The executable to start afterwards with `--after-reinstall`.
    pub relaunch: Option<PathBuf>,
}

impl HelperPlan {
    pub fn installer_args_for(program_dir: &Path) -> String {
        format!(
            "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /NOCLOSEAPPLICATIONS /NORESTARTAPPLICATIONS /DIR=\"{}\"",
            program_dir.display()
        )
    }
}

fn ps(text: &str) -> String {
    text.replace('\'', "''")
}

fn ps_path(path: &Path) -> String {
    ps(&path.display().to_string())
}

/// The PowerShell the helper runs. It waits for Phorminx to exit, re-checks
/// the installer, moves the program folder aside, installs, and only after a
/// successful install moves app state into the backup. A failed install puts
/// the old program folder back and touches no data.
pub fn helper_script(plan: &HelperPlan) -> String {
    let program_name = plan
        .program_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Phorminx".into());
    let sibling = |suffix: &str| plan.program_dir.with_file_name(format!("{program_name}.{suffix}-{}", plan.stamp));
    let reset_list = RESET
        .iter()
        .filter(|name| !name.eq_ignore_ascii_case("temp"))
        .map(|name| format!("'{}'", ps(name)))
        .collect::<Vec<_>>()
        .join(",");
    let temp_keep = TEMP_KEEP
        .iter()
        .map(|name| format!("'{}'", ps(name)))
        .collect::<Vec<_>>()
        .join(",");
    let webview = plan
        .webview_dir
        .as_ref()
        .map_or_else(String::new, |path| ps_path(path));
    let relaunch = plan
        .relaunch
        .as_ref()
        .map_or_else(String::new, |path| ps_path(path));
    format!(
        r#"
$ErrorActionPreference = 'Stop'
$moved = New-Object System.Collections.Generic.List[string]
$notMoved = New-Object System.Collections.Generic.List[string]
$backup = '{backup}'
$state = Join-Path $backup 'state'
function Report([string] $status, [string] $detail) {{
  $receipt = [ordered]@{{status=$status; detail=$detail; version='{version}'; moved=@($moved); not_moved=@($notMoved); finished_at=[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()}} | ConvertTo-Json -Compress
  [IO.File]::WriteAllText((Join-Path $backup 'receipt.json'), $receipt)
}}
# Files can stay locked for a moment after Phorminx and WebView2 exit.
function Move-Patiently([string] $from, [string] $to) {{
  for ($i = 0; $i -lt 60; $i++) {{
    try {{ Move-Item -LiteralPath $from -Destination $to; return $true }} catch {{ Start-Sleep -Milliseconds 500 }}
  }}
  return $false
}}
function Move-State([string] $from, [string] $name) {{
  if (-not (Test-Path -LiteralPath $from)) {{ return }}
  if (Move-Patiently $from (Join-Path $state $name)) {{ $moved.Add($name) }} else {{ $notMoved.Add($name) }}
}}
Report 'started' ''
Wait-Process -Id {pid} -ErrorAction SilentlyContinue
$code = & {{
  $file = $null
  try {{
    $file = [IO.File]::Open('{installer}', [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    if ($file.Length -ne {bytes}) {{ Report 'verification_failed' 'size'; return 3 }}
    $sha = [Security.Cryptography.SHA256]::Create()
    try {{ $actual = [BitConverter]::ToString($sha.ComputeHash($file)).Replace('-', '').ToLowerInvariant() }} finally {{ $sha.Dispose() }}
    if ($actual -ne '{hash}') {{ Report 'verification_failed' 'sha256'; return 4 }}
    $program = '{program}'
    $old = '{old}'
    $hadProgram = Test-Path -LiteralPath $program
    if ($hadProgram -and -not (Move-Patiently $program $old)) {{ Report 'program_busy' ''; return 6 }}
    $exit = -1
    try {{
      $process = Start-Process -FilePath '{installer}' -ArgumentList '{args}' -WindowStyle Hidden -PassThru
      $null = $process.Handle
      $process.WaitForExit()
      $exit = $process.ExitCode
    }} catch {{ $exit = -2 }}
    if ($exit -ne 0 -or -not (Test-Path -LiteralPath (Join-Path $program 'phorminx-app.exe'))) {{
      if ($hadProgram) {{
        if (Test-Path -LiteralPath $program) {{ $null = Move-Patiently $program '{failed}' }}
        $null = Move-Patiently $old $program
      }}
      Report 'installation_failed' "installer exit $exit"
      return 7
    }}
    if ($hadProgram) {{ try {{ Remove-Item -LiteralPath $old -Recurse -Force }} catch {{ }} }}
    $data = '{data}'
    foreach ($name in @({reset_list})) {{ Move-State (Join-Path $data $name) $name }}
    $temp = Join-Path $data 'temp'
    if (Test-Path -LiteralPath $temp) {{
      $tempState = Join-Path $state 'temp'
      $null = New-Item -ItemType Directory -Force -Path $tempState
      foreach ($child in @(Get-ChildItem -LiteralPath $temp -Force)) {{
        if (@({temp_keep}) -contains $child.Name) {{ continue }}
        if (Move-Patiently $child.FullName (Join-Path $tempState $child.Name)) {{ $moved.Add('temp\' + $child.Name) }} else {{ $notMoved.Add('temp\' + $child.Name) }}
      }}
    }}
    $webview = '{webview}'
    if ($webview -ne '') {{ Move-State $webview 'webview2' }}
    Report 'installed' ''
    return 0
  }} catch {{ Report 'installation_failed' $_.Exception.Message; return 5 }} finally {{ if ($null -ne $file) {{ $file.Dispose() }} }}
}}
try {{
  $relaunch = '{relaunch}'
  if ($relaunch -ne '' -and (Test-Path -LiteralPath $relaunch)) {{ Start-Process -FilePath $relaunch -ArgumentList '--after-reinstall' }}
}} catch {{ }}
exit $code
"#,
        backup = ps_path(&plan.backup_dir),
        version = ps(&plan.version),
        pid = plan.pid,
        installer = ps_path(&plan.installer),
        bytes = plan.bytes,
        hash = ps(&plan.sha256),
        program = ps_path(&plan.program_dir),
        old = ps_path(&sibling("old")),
        failed = ps_path(&sibling("failed")),
        args = ps(&plan.installer_args),
        data = ps_path(&plan.data_dir),
    )
}

/// The hidden PowerShell process that runs [`helper_script`].
pub fn helper_command(plan: &HelperPlan) -> Result<std::process::Command, String> {
    use base64::Engine as _;
    if plan.sha256.len() != 64 || !plan.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("The installer check is missing.".into());
    }
    let powershell = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or("Windows PowerShell is unavailable.")?
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let encoded: Vec<u8> = helper_script(plan)
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut command = std::process::Command::new(powershell);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-EncodedCommand",
            &base64::engine::general_purpose::STANDARD.encode(encoded),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    Ok(command)
}

fn sha256_file(path: &Path) -> Result<(String, u64), String> {
    let mut file = std::fs::File::open(path).map_err(|_| "Could not read the installer.")?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buffer).map_err(|_| "Could not read the installer.")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hash.update(&buffer[..n]);
    }
    Ok((updates::hex(&hash.finalize()), total))
}

/// The installed program folder, if this copy came from the installer.
pub fn installed_program_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|_| "Could not locate Phorminx.")?;
    let directory = exe.parent().ok_or("Could not locate Phorminx.")?.to_path_buf();
    if !directory.join("unins000.exe").is_file() {
        return Err("This copy of Phorminx was not installed with the Phorminx installer, so it cannot reinstall itself. Download the installer from the Phorminx releases page instead.".into());
    }
    Ok(directory)
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FlowView {
    /// `idle`, `checking`, `downloading`, `ready`, `handing-off`, `failed`.
    pub state: String,
    pub message: String,
    pub received: u64,
    pub total: u64,
    pub version: String,
    /// This is a QA build that installs a local installer.
    pub qa: bool,
}

/// Download and verify the installer in the background.
#[derive(Default)]
pub struct ReinstallFlow {
    view: Arc<Mutex<FlowView>>,
    staged: Arc<Mutex<Option<StagedInstaller>>>,
}

fn qa_installer() -> Option<PathBuf> {
    #[cfg(feature = "reinstall-qa")]
    {
        // A QA build must never also carry the release-signing key.
        const _: () = assert!(option_env!("PHORMINX_UPDATE_PUBLIC_KEY").is_none());
        std::env::var_os("PHORMINX_QA_INSTALLER").map(PathBuf::from)
    }
    #[cfg(not(feature = "reinstall-qa"))]
    {
        None
    }
}

/// A QA reinstall runs only against a throwaway profile, never the user's
/// real `%USERPROFILE%\AppData\Local\Phorminx`.
fn qa_guard(data_dir: &Path) -> Result<(), String> {
    if !cfg!(feature = "reinstall-qa") {
        return Ok(());
    }
    if qa_installer().is_none() {
        return Err("QA build: set PHORMINX_QA_INSTALLER to the local test installer.".into());
    }
    let real = std::env::var_os("USERPROFILE")
        .map(|profile| PathBuf::from(profile).join("AppData").join("Local").join("Phorminx"));
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    };
    if real.is_none_or(|real| same(&real, data_dir)) {
        return Err("QA build: start it with LOCALAPPDATA set to a throwaway folder. It never reinstalls your real Phorminx profile.".into());
    }
    Ok(())
}

/// The rollback floor the updater already keeps; a reinstall never lowers it.
fn rollback_floor(data_dir: &Path) -> u64 {
    read_json::<serde_json::Value>(&data_dir.join("updates").join("preferences.json"))
        .and_then(|value| value.get("highest_sequence").and_then(serde_json::Value::as_u64))
        .unwrap_or(0)
}

impl ReinstallFlow {
    pub fn snapshot(&self) -> FlowView {
        let mut view = self.view.lock().map(|view| view.clone()).unwrap_or_default();
        if view.state.is_empty() {
            view.state = "idle".into();
        }
        view.qa = cfg!(feature = "reinstall-qa");
        view
    }

    pub fn start(&self, data_dir: &Path) -> Result<(), String> {
        qa_guard(data_dir)?;
        {
            let mut view = self.view.lock().map_err(|_| "Reinstall is unavailable.")?;
            if matches!(view.state.as_str(), "checking" | "downloading" | "handing-off") {
                return Ok(());
            }
            *view = FlowView {
                state: "checking".into(),
                message: "Looking up the current Phorminx release…".into(),
                ..FlowView::default()
            };
        }
        let view = self.view.clone();
        let staged = self.staged.clone();
        let packages = data_dir.join("updates").join("packages");
        let floor = rollback_floor(data_dir);
        let spawned = std::thread::Builder::new()
            .name("phorminx-reinstall-download".into())
            .spawn(move || {
                let set = |update: &dyn Fn(&mut FlowView)| {
                    if let Ok(mut view) = view.lock() {
                        update(&mut view);
                    }
                };
                let result = if let Some(path) = qa_installer() {
                    sha256_file(&path).map(|(sha256, bytes)| StagedInstaller {
                        path,
                        sha256,
                        bytes,
                        version: updates::current_version().into(),
                    })
                } else {
                    match updates::check_for_reinstall(floor) {
                        Err(error) => Err(plain_error(&error)),
                        Ok(release) => {
                            let total = release.bytes;
                            let version = release.version.clone();
                            set(&|view| {
                                view.state = "downloading".into();
                                view.message = format!("Downloading Phorminx {version}…");
                                view.total = total;
                                view.version = version.clone();
                            });
                            let progress = |received: u64| set(&|view| view.received = received);
                            stage(&release, &packages, &progress)
                        }
                    }
                };
                match result {
                    Ok(installer) => {
                        set(&|view| {
                            view.state = "ready".into();
                            view.message = "The installer is downloaded and verified.".into();
                            view.received = installer.bytes;
                            view.total = installer.bytes;
                            view.version = installer.version.clone();
                        });
                        if let Ok(mut slot) = staged.lock() {
                            *slot = Some(installer);
                        }
                    }
                    Err(message) => set(&|view| {
                        view.state = "failed".into();
                        view.message = message.clone();
                    }),
                }
            });
        if spawned.is_err() {
            let mut view = self.view.lock().map_err(|_| "Reinstall is unavailable.")?;
            view.state = "failed".into();
            view.message = "Could not start the download.".into();
        }
        Ok(())
    }

    /// Create the backup and start the helper. The caller must then exit
    /// Phorminx; the helper waits for that.
    pub fn hand_off(&self, data_dir: &Path, program_dir: &Path) -> Result<(), String> {
        qa_guard(data_dir)?;
        let installer = self
            .staged
            .lock()
            .map_err(|_| "Reinstall is unavailable.")?
            .clone()
            .ok_or("Download the installer first.")?;
        // The bytes are checked again here and again by the helper.
        let (sha256, bytes) = sha256_file(&installer.path)?;
        if sha256 != installer.sha256 || bytes != installer.bytes {
            return Err("The downloaded installer changed after it was checked. Try again.".into());
        }
        let now = unix_now();
        let backup = create_backup(
            data_dir,
            now,
            updates::current_version(),
            &installer.version,
            program_dir,
        )?;
        let plan = HelperPlan {
            pid: std::process::id(),
            installer_args: HelperPlan::installer_args_for(program_dir),
            installer: installer.path,
            sha256: installer.sha256,
            bytes: installer.bytes,
            version: installer.version,
            program_dir: program_dir.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            webview_dir: data_dir.parent().map(|parent| parent.join(WEBVIEW_DIRECTORY)),
            backup_dir: backup,
            stamp: utc_stamp(now),
            relaunch: Some(program_dir.join("phorminx-app.exe")),
        };
        helper_command(&plan)?
            .spawn()
            .map_err(|_| "Could not start the reinstall helper.")?;
        if let Ok(mut view) = self.view.lock() {
            view.state = "handing-off".into();
            view.message = "Closing Phorminx to reinstall it. It reopens by itself in a minute or two.".into();
        }
        Ok(())
    }

    pub fn ready(&self) -> bool {
        self.staged.lock().is_ok_and(|staged| staged.is_some())
    }
}

fn stage(release: &Release, packages: &Path, progress: &dyn Fn(u64)) -> Result<StagedInstaller, String> {
    updates::stage_with_progress(release, packages, progress)
        .map(|path| StagedInstaller {
            path,
            sha256: release.sha256.clone(),
            bytes: release.bytes,
            version: release.version.clone(),
        })
        .map_err(|error| plain_error(&error))
}

fn plain_error(error: &updates::UpdateError) -> String {
    match error {
        updates::UpdateError::Unconfigured => "This copy of Phorminx cannot check for signed releases, so it cannot reinstall itself. Download the installer from the Phorminx releases page instead.".into(),
        updates::UpdateError::Network => "Phorminx could not reach the internet to download the installer. Check your connection and try again. Nothing was changed.".into(),
        updates::UpdateError::InvalidMetadata => "The release information did not pass its security check, so nothing was downloaded or changed.".into(),
        updates::UpdateError::Package => "The downloaded installer did not pass its security check, so nothing was changed. Try again.".into(),
        updates::UpdateError::Storage => "Phorminx could not save the installer on this computer. Check that the disk has free space. Nothing was changed.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_resets_app_state_and_keeps_user_data() {
        for name in RESET {
            assert_eq!(classify(name), Disposition::Reset, "{name}");
        }
        assert_eq!(classify("Settings.TOML"), Disposition::Reset);
        for name in KEEP {
            assert_eq!(classify(name), Disposition::Keep, "{name}");
        }
        // User data, identity and anything unknown is never reset.
        for name in [
            "phorminx.db",
            "phorminx.db-wal",
            "phorminx.db-shm",
            "tor",
            "investigations",
            "models",
            "backups",
            "updates",
            "vosk-assets-1700000000000",
            "something-new.sqlite3",
            "audio-spool",
        ] {
            assert_eq!(classify(name), Disposition::Keep, "{name}");
        }
        assert!(TEMP_KEEP.contains(&"audio-spool"));
        assert!(KEEP.iter().all(|keep| !RESET.contains(keep)));
    }

    #[test]
    fn stamps_are_utc_and_sortable() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_793_219_312), "20261028T202832Z");
        assert!(valid_backup_id("reinstall-20261028T202832Z"));
        for bad in ["reinstall-..", "reinstall-20261028T203512", "x-20261028T202832Z", "reinstall-2026102xT203512Z"] {
            assert!(!valid_backup_id(bad), "{bad}");
        }
    }

    fn data_with_settings(text: &str) -> tempfile::TempDir {
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("settings.toml"), text).unwrap();
        data
    }

    fn valid_settings() -> String {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::settings::SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let mut settings = crate::settings::Settings::default();
        settings.formatting.ollama_model = Some("qwen2.5:7b".into());
        store.save(&settings).unwrap();
        std::fs::read_to_string(store.path()).unwrap()
    }

    /// Simulates what the helper does after a successful install.
    fn move_into_backup(data: &Path, backup: &Path, name: &str) {
        std::fs::rename(data.join(name), backup.join("state").join(name)).unwrap();
    }

    #[test]
    fn backup_lists_and_restores_previous_settings_without_deleting_anything() {
        let saved = valid_settings();
        let data = data_with_settings(&saved);
        std::fs::write(data.path().join("phorminx.db"), b"user history").unwrap();
        let backup = create_backup(data.path(), 1_793_219_312, "0.2.4", "0.2.4", Path::new("C:/P")).unwrap();
        assert!(backup.ends_with("backups/reinstall-20261028T202832Z"));
        move_into_backup(data.path(), &backup, "settings.toml");
        std::fs::write(backup.join("receipt.json"), br#"{"status":"installed"}"#).unwrap();
        // First-run setup then writes starter defaults.
        std::fs::write(data.path().join("settings.toml"), "# starter defaults\n").unwrap();

        let listed = list_backups(data.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "installed");
        assert!(listed[0].has_settings && !listed[0].restored);
        assert_eq!(latest_status(data.path()).as_deref(), Some("installed"));

        let outcome = restore(data.path(), &listed[0].id).unwrap();
        // The formatting model, plus the default semantic-search embedder.
        assert!(outcome.models.contains(&"qwen2.5:7b".to_owned()), "{:?}", outcome.models);
        assert!(outcome.models.iter().all(|model| !model.is_empty()));
        assert_eq!(std::fs::read_to_string(data.path().join("settings.toml")).unwrap(), saved);
        // The defaults it replaced are kept in the backup, and so is the original.
        let replaced = std::fs::read_dir(&backup)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("replaced-"))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(replaced.path().join("settings.toml")).unwrap(),
            "# starter defaults\n"
        );
        assert!(backup.join("state/settings.toml").is_file());
        assert_eq!(std::fs::read(data.path().join("phorminx.db")).unwrap(), b"user history");
        assert!(list_backups(data.path())[0].restored);
        dismiss(data.path(), &listed[0].id).unwrap();
        assert!(list_backups(data.path())[0].dismissed);
    }

    #[test]
    fn damaged_backup_settings_are_refused_and_current_files_untouched() {
        let data = data_with_settings("current = true\n");
        let backup = create_backup(data.path(), 1_793_219_312, "0.2.4", "0.2.4", Path::new("C:/P")).unwrap();
        std::fs::write(backup.join("state/settings.toml"), "this is [not valid toml").unwrap();
        let id = backup.file_name().unwrap().to_string_lossy().into_owned();
        assert!(restore(data.path(), &id).is_err());
        assert_eq!(std::fs::read_to_string(data.path().join("settings.toml")).unwrap(), "current = true\n");

        std::fs::write(backup.join("state/settings.toml"), valid_settings()).unwrap();
        std::fs::write(backup.join("state/assistant.json"), b"{not json").unwrap();
        assert!(restore(data.path(), &id).is_err());
        assert_eq!(std::fs::read_to_string(data.path().join("settings.toml")).unwrap(), "current = true\n");

        assert!(restore(data.path(), "reinstall-../../x").is_err());
    }

    #[test]
    fn same_second_backups_get_distinct_folders() {
        let data = tempfile::tempdir().unwrap();
        let a = create_backup(data.path(), 100, "1", "1", Path::new("C:/P")).unwrap();
        let b = create_backup(data.path(), 100, "1", "1", Path::new("C:/P")).unwrap();
        assert_ne!(a, b);
        assert_eq!(list_backups(data.path()).len(), 2);
    }

    fn plan(root: &Path, installer: &Path, args: &str) -> HelperPlan {
        let (sha256, bytes) = sha256_file(installer).unwrap();
        HelperPlan {
            pid: 2_147_483_644,
            installer: installer.to_owned(),
            sha256,
            bytes,
            version: "9.9.9".into(),
            installer_args: args.into(),
            program_dir: root.join("Programs").join("Phorminx"),
            data_dir: root.join("Phorminx"),
            webview_dir: Some(root.join(WEBVIEW_DIRECTORY)),
            backup_dir: root.join("Phorminx").join("backups").join("reinstall-20261028T202832Z"),
            stamp: "20261028T202832Z".into(),
            relaunch: None,
        }
    }

    #[test]
    fn helper_script_quotes_paths_and_moves_only_reset_entries() {
        let root = Path::new("C:/Users/O'Brien/AppData/Local");
        let mut plan = HelperPlan {
            pid: 42,
            installer: root.join("installer.exe"),
            sha256: "a".repeat(64),
            bytes: 7,
            version: "1.0.0".into(),
            installer_args: HelperPlan::installer_args_for(&root.join("Programs/Phorminx")),
            program_dir: root.join("Programs/Phorminx"),
            data_dir: root.join("Phorminx"),
            webview_dir: None,
            backup_dir: root.join("Phorminx/backups/reinstall-20261028T202832Z"),
            stamp: "20261028T202832Z".into(),
            relaunch: Some(root.join("Programs/Phorminx/phorminx-app.exe")),
        };
        let script = helper_script(&plan);
        assert!(script.contains("O''Brien"));
        assert!(!script.contains("O'Brien"));
        assert!(script.contains("Wait-Process -Id 42"));
        assert!(script.contains("/VERYSILENT"));
        assert!(script.contains("Phorminx.old-20261028T202832Z"));
        assert!(script.contains("--after-reinstall"));
        for name in RESET.iter().filter(|name| **name != "temp") {
            assert!(script.contains(&format!("'{name}'")), "{name}");
        }
        for name in ["phorminx.db", "'tor'", "'models'", "'investigations'", "Remove-Item -LiteralPath $data"] {
            assert!(!script.contains(name), "{name}");
        }
        // The only deletion is the moved-aside program folder.
        assert_eq!(script.matches("Remove-Item").count(), 1);
        plan.sha256 = "not-a-hash".into();
        assert!(helper_command(&plan).is_err());
    }

    /// Runs the real helper against a throwaway folder tree, with a copy of
    /// cmd.exe standing in for the installer.
    #[cfg(windows)]
    fn run_helper(root: &Path, succeed: bool) -> HelperPlan {
        let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap();
        let installer = root.join("fake-installer.exe");
        std::fs::copy(system.join("System32/cmd.exe"), &installer).unwrap();
        let program = root.join("Programs").join("Phorminx");
        let args = if succeed {
            format!(
                "/c mkdir \"{p}\" && echo new> \"{p}\\phorminx-app.exe\"",
                p = program.display()
            )
        } else {
            "/c exit 3".to_owned()
        };
        let plan = plan(root, &installer, &args);
        std::fs::create_dir_all(plan.backup_dir.join("state")).unwrap();
        let status = helper_command(&plan).unwrap().status().unwrap();
        assert_eq!(status.success(), succeed);
        plan
    }

    #[cfg(windows)]
    fn fake_install(root: &Path) {
        let program = root.join("Programs").join("Phorminx");
        std::fs::create_dir_all(&program).unwrap();
        std::fs::write(program.join("phorminx-app.exe"), "old").unwrap();
        let data = root.join("Phorminx");
        for directory in ["models", "tor/state/keystore", "investigations/1", "runtime/vosk", "temp/audio-spool", "temp/other", "managed-setup-v1"] {
            std::fs::create_dir_all(data.join(directory)).unwrap();
        }
        for (file, text) in [
            ("settings.toml", "s"),
            ("assistant.json", "{}"),
            ("phorminx.db", "db"),
            ("phorminx.db-wal", "wal"),
            ("models/ggml-base.en.bin", "model"),
            ("tor/state/keystore/key", "identity"),
            ("investigations/1/a.png", "png"),
            ("temp/audio-spool/chunk", "audio"),
            ("unknown-file.bin", "?"),
        ] {
            std::fs::write(data.join(file), text).unwrap();
        }
        std::fs::create_dir_all(root.join(WEBVIEW_DIRECTORY).join("EBWebView")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn helper_installs_then_moves_state_and_keeps_user_data() {
        let root = tempfile::tempdir().unwrap();
        fake_install(root.path());
        let plan = run_helper(root.path(), true);
        let data = &plan.data_dir;
        let state = plan.backup_dir.join("state");
        assert_eq!(std::fs::read_to_string(plan.program_dir.join("phorminx-app.exe")).unwrap().trim(), "new");
        assert!(!plan.program_dir.with_file_name("Phorminx.old-20261028T202832Z").exists());
        for moved in ["settings.toml", "assistant.json", "runtime", "managed-setup-v1", "temp/other", "webview2"] {
            assert!(state.join(moved).exists(), "{moved} moved");
        }
        assert!(!data.join("settings.toml").exists());
        assert!(!root.path().join(WEBVIEW_DIRECTORY).exists());
        for kept in ["phorminx.db", "phorminx.db-wal", "models/ggml-base.en.bin", "tor/state/keystore/key", "investigations/1/a.png", "temp/audio-spool/chunk", "unknown-file.bin"] {
            assert!(data.join(kept).exists(), "{kept} kept");
        }
        let receipt: Receipt = read_json(&plan.backup_dir.join("receipt.json")).unwrap();
        assert_eq!(receipt.status, "installed");
        assert!(receipt.moved.contains(&"settings.toml".to_owned()));
        assert!(receipt.not_moved.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn failed_install_puts_the_old_program_back_and_touches_no_data() {
        let root = tempfile::tempdir().unwrap();
        fake_install(root.path());
        let plan = run_helper(root.path(), false);
        assert_eq!(std::fs::read_to_string(plan.program_dir.join("phorminx-app.exe")).unwrap(), "old");
        assert!(plan.data_dir.join("settings.toml").is_file());
        assert!(plan.data_dir.join("runtime/vosk").is_dir());
        assert!(root.path().join(WEBVIEW_DIRECTORY).is_dir());
        let receipt: Receipt = read_json(&plan.backup_dir.join("receipt.json")).unwrap();
        assert_eq!(receipt.status, "installation_failed");
        assert!(receipt.moved.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn tampered_installer_is_refused_before_anything_moves() {
        let root = tempfile::tempdir().unwrap();
        fake_install(root.path());
        let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap();
        let installer = root.path().join("fake-installer.exe");
        std::fs::copy(system.join("System32/cmd.exe"), &installer).unwrap();
        let mut plan = plan(root.path(), &installer, "/c exit 0");
        plan.sha256 = "0".repeat(64);
        std::fs::create_dir_all(plan.backup_dir.join("state")).unwrap();
        assert!(!helper_command(&plan).unwrap().status().unwrap().success());
        assert_eq!(std::fs::read_to_string(plan.program_dir.join("phorminx-app.exe")).unwrap(), "old");
        assert!(plan.data_dir.join("settings.toml").is_file());
        let receipt: Receipt = read_json(&plan.backup_dir.join("receipt.json")).unwrap();
        assert_eq!(receipt.status, "verification_failed");
    }
}

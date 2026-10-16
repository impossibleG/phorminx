//! Signed release verification. Trust is compiled in, never supplied by a server.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

// The latest non-prerelease GitHub Release carries a signed envelope as an
// asset. GitHub is distribution, not the trust root: the embedded key is.
pub const UPDATE_ENDPOINT: &str =
    "https://github.com/impossibleG/phorminx/releases/latest/download/phorminx-update.json";
const MAX_ENVELOPE: usize = 32 * 1024;
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;
static STAGE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn package_matches(path: &Path, release: &Release) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != release.bytes || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let Ok(n) = file.read(&mut buffer) else {
            return false;
        };
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    hex(&hash.finalize()) == release.sha256
}

fn reject_links(path: &Path) -> Result<(), UpdateError> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(UpdateError::Storage);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(UpdateError::Storage);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(UpdateError::Storage),
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub schema_version: u8,
    pub version: String,
    pub sequence: u64,
    pub channel: String,
    pub platform: String,
    pub published_at: u64,
    pub expires_at: u64,
    pub download_url: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedCheck {
    pub release: Option<Release>,
    pub highest_sequence: u64,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum UpdateError {
    #[error(
        "Automatic updates are not configured in this build. A trusted release-signing key is required."
    )]
    Unconfigured,
    #[error("The update metadata failed signature, identity, expiry or rollback validation.")]
    InvalidMetadata,
    #[error("The update service is unavailable. The installed app is unchanged.")]
    Network,
    #[error("The update package failed verification. Nothing was installed.")]
    Package,
    #[error("Could not safely stage this update. The installed app is unchanged.")]
    Storage,
}

pub fn current_version() -> &'static str {
    option_env!("PHORMINX_RELEASE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

pub fn configured_key() -> Result<Vec<u8>, UpdateError> {
    let key = option_env!("PHORMINX_UPDATE_PUBLIC_KEY").ok_or(UpdateError::Unconfigured)?;
    let key = STANDARD
        .decode(key)
        .map_err(|_| UpdateError::Unconfigured)?;
    if key.len() != 32 {
        return Err(UpdateError::Unconfigured);
    }
    Ok(key)
}

fn download_url_allowed(url: &str) -> bool {
    let Some(path) = url.strip_prefix("https://github.com/impossibleG/phorminx/releases/download/")
    else {
        return false;
    };
    let parts: Vec<_> = path.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            part.as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
        && parts[1].ends_with(".exe")
}

/// Validate the signature on the exact decoded bytes before interpreting fields.
pub fn verify(
    envelope: &[u8],
    key: &[u8],
    now: u64,
    current: &str,
    highest_sequence: u64,
) -> Result<VerifiedCheck, UpdateError> {
    if envelope.len() > MAX_ENVELOPE || key.len() != 32 {
        return Err(UpdateError::InvalidMetadata);
    }
    let envelope: Envelope =
        serde_json::from_slice(envelope).map_err(|_| UpdateError::InvalidMetadata)?;
    let payload = STANDARD
        .decode(envelope.payload)
        .map_err(|_| UpdateError::InvalidMetadata)?;
    let signature = STANDARD
        .decode(envelope.signature)
        .map_err(|_| UpdateError::InvalidMetadata)?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&payload, &signature)
        .map_err(|_| UpdateError::InvalidMetadata)?;
    let release: Release =
        serde_json::from_slice(&payload).map_err(|_| UpdateError::InvalidMetadata)?;
    let version =
        semver::Version::parse(&release.version).map_err(|_| UpdateError::InvalidMetadata)?;
    let current = semver::Version::parse(current).map_err(|_| UpdateError::InvalidMetadata)?;
    if release.schema_version != 1
        || release.channel != "stable"
        || release.platform != "windows-x64"
        || !version.pre.is_empty()
        || !version.build.is_empty()
        || release.sequence == 0
        || release.sequence > MAX_SEQUENCE
        || release.sequence < highest_sequence
        || release.published_at > now.saturating_add(300)
        || release.expires_at <= now
        || release.expires_at <= release.published_at
        || release.expires_at - release.published_at > 5 * 365 * 86400
        || release.bytes == 0
        || release.bytes > MAX_PACKAGE
        || release.sha256.len() != 64
        || !release
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !download_url_allowed(&release.download_url)
    {
        return Err(UpdateError::InvalidMetadata);
    }
    Ok(VerifiedCheck {
        highest_sequence: release.sequence,
        release: (version > current).then_some(release),
    })
}

pub fn check(highest_sequence: u64) -> Result<VerifiedCheck, UpdateError> {
    let key = configured_key()?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .max_redirects(5)
        .https_only(true)
        .build()
        .new_agent();
    let bytes = agent
        .get(UPDATE_ENDPOINT)
        .call()
        .map_err(|_| UpdateError::Network)?
        .body_mut()
        .with_config()
        .limit(MAX_ENVELOPE as u64)
        .read_to_vec()
        .map_err(|_| UpdateError::Network)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| UpdateError::InvalidMetadata)?
        .as_secs();
    verify(&bytes, &key, now, current_version(), highest_sequence)
}

/// The caller passes only a release returned by verify/check and a private app
/// staging directory. Download and hashing are background work, never capture work.
pub fn stage(release: &Release, directory: &Path) -> Result<PathBuf, UpdateError> {
    if !download_url_allowed(&release.download_url)
        || release.bytes == 0
        || release.bytes > MAX_PACKAGE
        || semver::Version::parse(&release.version).is_err()
    {
        return Err(UpdateError::InvalidMetadata);
    }
    reject_links(directory)?;
    std::fs::create_dir_all(directory).map_err(|_| UpdateError::Storage)?;
    let target = directory.join(format!(
        "Phorminx-{}-{}.exe",
        release.version, release.sequence
    ));
    reject_links(&target)?;
    if package_matches(&target, release) {
        return Ok(target);
    }
    if target.exists() {
        return Err(UpdateError::Package);
    }
    let sequence = STAGE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| UpdateError::Storage)?
        .as_nanos();
    let partial = directory.join(format!(
        "download-{}-{nonce}-{sequence}.partial",
        std::process::id()
    ));
    // No following pre-existing partial files, symlinks, or arbitrary server filenames.
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&partial)
        .map_err(|_| UpdateError::Storage)?;
    let result = (|| {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(600)))
            .max_redirects(5)
            .https_only(true)
            .build()
            .new_agent();
        let mut response = agent
            .get(&release.download_url)
            .call()
            .map_err(|_| UpdateError::Network)?;
        let mut reader = response.body_mut().as_reader().take(release.bytes + 1);
        let mut writer = file;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            let n = reader.read(&mut buffer).map_err(|_| UpdateError::Network)?;
            if n == 0 {
                break;
            }
            received += n as u64;
            if received > release.bytes {
                return Err(UpdateError::Package);
            }
            writer
                .write_all(&buffer[..n])
                .map_err(|_| UpdateError::Storage)?;
            hash.update(&buffer[..n]);
        }
        if received != release.bytes || hex(&hash.finalize()) != release.sha256 {
            return Err(UpdateError::Package);
        }
        writer.sync_all().map_err(|_| UpdateError::Storage)?;
        drop(writer);
        if target.exists() {
            return Err(UpdateError::Storage);
        }
        std::fs::rename(&partial, &target).map_err(|_| UpdateError::Storage)?;
        Ok(target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

/// Start a narrowly scoped helper that waits for this process to exit naturally.
/// It never terminates Phorminx or closes applications; package bytes are hashed
/// again under a no-write/no-delete file handle immediately before execution.
pub fn install_after_exit(path: &Path, release: &Release) -> Result<(), UpdateError> {
    let _ = configured_key()?;
    reject_links(path)?;
    if !download_url_allowed(&release.download_url)
        || release.sha256.len() != 64
        || !release.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || release.bytes == 0
        || release.bytes > MAX_PACKAGE
    {
        return Err(UpdateError::InvalidMetadata);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let canonical = path.canonicalize().map_err(|_| UpdateError::Storage)?;
        let path = canonical
            .to_str()
            .ok_or(UpdateError::Storage)?
            .replace('\'', "''");
        let receipt = canonical
            .parent()
            .ok_or(UpdateError::Storage)?
            .join("last-install.json");
        reject_links(&receipt)?;
        let receipt = receipt
            .to_str()
            .ok_or(UpdateError::Storage)?
            .replace('\'', "''");
        let powershell = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .ok_or(UpdateError::Storage)?
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let script = format!(
            r#"
$ErrorActionPreference = 'Stop'
function Report([string] $status) {{
  $receipt = @{{status=$status; version='{version}'}} | ConvertTo-Json -Compress
  [IO.File]::WriteAllText('{receipt}', $receipt)
}}
Wait-Process -Id {pid} -ErrorAction SilentlyContinue
if ([DateTimeOffset]::UtcNow.ToUnixTimeSeconds() -ge {expires}) {{ Report 'expired'; exit 2 }}
$file = $null
try {{
  $file = [IO.File]::Open('{path}', [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
  if ($file.Length -ne {bytes}) {{ Report 'verification_failed'; exit 3 }}
  $sha = [Security.Cryptography.SHA256]::Create()
  try {{ $actual = [BitConverter]::ToString($sha.ComputeHash($file)).Replace('-', '').ToLowerInvariant() }} finally {{ $sha.Dispose() }}
  if ($actual -ne '{hash}') {{ Report 'verification_failed'; exit 4 }}
  $process = Start-Process -FilePath '{path}' -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /NOCLOSEAPPLICATIONS /NORESTARTAPPLICATIONS' -WindowStyle Hidden -PassThru
  $process.WaitForExit()
  if ($process.ExitCode -eq 0) {{ Report 'installed' }} else {{ Report 'installation_failed' }}
  exit $process.ExitCode
}} catch {{ Report 'installation_failed'; exit 5 }} finally {{ if ($null -ne $file) {{ $file.Dispose() }} }}
"#,
            pid = std::process::id(),
            expires = release.expires_at,
            bytes = release.bytes,
            hash = release.sha256,
            version = release.version
        );
        let encoded: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        std::process::Command::new(powershell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-EncodedCommand",
                &STANDARD.encode(encoded),
            ])
            .creation_flags(0x08000000)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| UpdateError::Storage)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(UpdateError::Unconfigured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    fn signed(release: &Release) -> (Vec<u8>, Vec<u8>) {
        let key = Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap(); // synthetic test key, never production authority
        let bytes = serde_json::to_vec(release).unwrap();
        (serde_json::to_vec(&serde_json::json!({"payload":STANDARD.encode(&bytes),"signature":STANDARD.encode(key.sign(&bytes).as_ref())})).unwrap(), key.public_key().as_ref().to_vec())
    }
    fn release() -> Release {
        Release {schema_version:1,version:"1.2.0".into(),sequence:9,channel:"stable".into(),platform:"windows-x64".into(),published_at:1000,expires_at:2000,download_url:"https://github.com/impossibleG/phorminx/releases/download/v1.2.0/Phorminx-1.2.0.exe".into(),sha256:"a".repeat(64),bytes:100}
    }
    #[test]
    fn authentic_new_release_only() {
        let (bytes, key) = signed(&release());
        assert_eq!(
            verify(&bytes, &key, 1500, "1.1.0", 8).unwrap().release,
            Some(release())
        );
        let already_current = verify(&bytes, &key, 1500, "1.2.0", 8).unwrap();
        assert_eq!(already_current.release, None);
        assert_eq!(already_current.highest_sequence, 9);
        assert_eq!(
            verify(&bytes, &key, 1500, "2.0.0", 9).unwrap().release,
            None
        );
    }
    #[test]
    fn qa_stage_reuses_verified_package_without_network_and_rejects_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let mut release = release();
        let content = b"synthetic inert package, not executable";
        release.bytes = content.len() as u64;
        release.sha256 = hex(&Sha256::digest(content));
        let path = directory.path().join("Phorminx-1.2.0-9.exe");
        std::fs::write(&path, content).unwrap();
        assert_eq!(stage(&release, directory.path()).unwrap(), path);
        std::fs::write(&path, b"corrupt").unwrap();
        assert_eq!(stage(&release, directory.path()), Err(UpdateError::Package));
        assert_eq!(std::fs::read(path).unwrap(), b"corrupt");
    }
    #[test]
    fn rejects_tampering_wrong_key_expiry_rollback_and_unsafe_urls() {
        let (bytes, key) = signed(&release());
        assert!(verify(&bytes, &[0; 32], 1500, "1.0.0", 0).is_err());
        assert!(verify(&bytes, &key, 2000, "1.0.0", 0).is_err());
        assert!(verify(&bytes, &key, 1500, "1.0.0", 10).is_err());
        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        envelope["payload"] = STANDARD.encode(b"{}").into();
        assert!(
            verify(
                &serde_json::to_vec(&envelope).unwrap(),
                &key,
                1500,
                "1.0.0",
                0
            )
            .is_err()
        );
        for url in [
            "http://github.com/impossibleG/phorminx/releases/download/v1/a.exe",
            "https://evil.example/a.exe",
            "https://github.com/impossibleG/phorminx/releases/download/../a.exe",
            "https://github.com/impossibleG/phorminx/releases/download/v1/a.exe?secret=1",
        ] {
            let mut r = release();
            r.download_url = url.into();
            let (b, k) = signed(&r);
            assert!(verify(&b, &k, 1500, "1.0.0", 0).is_err());
        }
    }
    #[test]
    fn rejects_signed_but_invalid_platform_channel_size_and_dates() {
        for field in [
            "platform",
            "channel",
            "bytes",
            "expires_at",
            "published_at",
            "sequence",
            "version",
        ] {
            let mut r = release();
            match field {
                "platform" => r.platform = "linux".into(),
                "channel" => r.channel = "preview".into(),
                "bytes" => r.bytes = MAX_PACKAGE + 1,
                "expires_at" => r.expires_at = 6 * 365 * 86400,
                "published_at" => r.published_at = 1900,
                "sequence" => r.sequence = 0,
                _ => r.version = "1.3.0-alpha.1".into(),
            };
            let (b, k) = signed(&r);
            assert!(verify(&b, &k, 1500, "1.0.0", 0).is_err(), "{field}");
        }
    }
}

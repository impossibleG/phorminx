//! Offline release-key and signed-manifest utility. Never upload the private key.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use phorminx_app::updates::{self, Release};
use ring::{rand::SystemRandom, signature::{Ed25519KeyPair, KeyPair}};
use sha2::{Digest, Sha256};
use std::{fs::{File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};

fn usage() -> ! {
    eprintln!("Usage:\n  phorminx_release keygen <private-key.pk8> <public-key.txt>\n  phorminx_release sign <private-key.pk8> <version> <sequence> <installer.exe> <phorminx-update.json>");
    std::process::exit(2)
}

fn new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)
        .map_err(|_| format!("Cannot create {}. Existing files are never replaced.", path.display()))?;
    file.write_all(bytes).and_then(|_| file.sync_all())
        .map_err(|_| format!("Could not finish writing {}.", path.display()))
}

fn private_path_allowed(path: &Path) -> Result<(), String> {
    if !path.is_absolute() { return Err("The private-key path must be absolute and outside this repository.".into()); }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()
        .map_err(|_| "Could not locate the source repository.")?;
    let parent = path.parent().ok_or("Choose a private-key file path.")?.canonicalize()
        .map_err(|_| "Create the private-key directory first.")?;
    if parent.starts_with(repo) { return Err("Never place the private signing key inside the source repository.".into()); }
    Ok(())
}

fn keygen(private: &Path, public: &Path) -> Result<(), String> {
    private_path_allowed(private)?;
    if public.exists() { return Err("The public-key file already exists.".into()); }
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "Could not generate a signing key.")?;
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| "Could not read the generated signing key.")?;
    new_file(private, pkcs8.as_ref())?;
    new_file(public, STANDARD.encode(key.public_key().as_ref()).as_bytes())?;
    println!("Signing key created. Keep {} offline and private. Put the public-key text in PHORMINX_UPDATE_PUBLIC_KEY when building releases.", private.display());
    Ok(())
}

fn sign(private: &Path, version: &str, sequence: u64, installer: &Path, output: &Path) -> Result<(), String> {
    private_path_allowed(private)?;
    let parsed = semver::Version::parse(version).map_err(|_| "Use a stable SemVer version.")?;
    if !parsed.pre.is_empty() || !parsed.build.is_empty() || sequence == 0 || sequence > 9_007_199_254_740_991 {
        return Err("Use a stable version and a positive, monotonically increasing sequence.".into());
    }
    let expected = format!("Phorminx-{version}-x64-setup.exe");
    if installer.file_name().and_then(|name| name.to_str()) != Some(expected.as_str()) {
        return Err(format!("Installer must be named {expected}."));
    }
    let bytes = std::fs::metadata(installer).map_err(|_| "Installer is unavailable.")?.len();
    if bytes == 0 || bytes > 512 * 1024 * 1024 { return Err("Installer size is outside the updater limit.".into()); }
    let mut input = File::open(installer).map_err(|_| "Cannot open the installer.")?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(|_| "Cannot hash the installer.")?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    let sha256: String = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "System clock is unavailable.")?.as_secs();
    let release = Release {
        schema_version: 1, version: version.into(), sequence,
        channel: "stable".into(), platform: "windows-x64".into(),
        published_at: now, expires_at: now + 5 * 365 * 86400,
        download_url: format!("https://github.com/impossibleG/phorminx/releases/download/v{version}/{expected}"),
        sha256, bytes,
    };
    let private_bytes = std::fs::read(private).map_err(|_| "Cannot read the private signing key.")?;
    let key = Ed25519KeyPair::from_pkcs8(&private_bytes).map_err(|_| "Private signing key is invalid.")?;
    let payload = serde_json::to_vec(&release).map_err(|_| "Cannot encode release metadata.")?;
    let signature = key.sign(&payload);
    let envelope = serde_json::json!({
        "payload": STANDARD.encode(&payload),
        "signature": STANDARD.encode(signature.as_ref()),
    });
    let envelope = serde_json::to_vec(&envelope).map_err(|_| "Cannot encode signed envelope.")?;
    updates::verify(&envelope, key.public_key().as_ref(), now, "0.0.0", 0)
        .map_err(|_| "Self-verification of the signed release failed.")?;
    new_file(output, &envelope)?;
    println!("Signed manifest created at {}. Upload it beside the exact installer in the v{version} GitHub Release.", output.display());
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let result = match args.as_slice() {
        [_, command, private, public] if command == "keygen" => keygen(Path::new(private), Path::new(public)),
        [_, command, private, version, sequence, installer, output] if command == "sign" =>
            sequence.parse::<u64>().map_err(|_| "Sequence must be a positive integer.".to_owned())
                .and_then(|sequence| sign(Path::new(private), version, sequence, Path::new(installer), Path::new(output))),
        _ => usage(),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_release_round_trip_rejects_tampering_and_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("offline.pk8");
        let public = root.path().join("public.txt");
        let installer = root.path().join("Phorminx-1.2.3-x64-setup.exe");
        let envelope = root.path().join("phorminx-update.json");
        std::fs::write(&installer, b"synthetic installer; never execute").unwrap();
        keygen(&private, &public).unwrap();
        assert!(keygen(&private, &public).is_err());
        sign(&private, "1.2.3", 12, &installer, &envelope).unwrap();
        assert!(sign(&private, "1.2.3", 12, &installer, &envelope).is_err());
        let key = STANDARD.decode(std::fs::read_to_string(public).unwrap()).unwrap();
        let bytes = std::fs::read(envelope).unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(updates::verify(&bytes, &key, now, "1.0.0", 11).unwrap().release.is_some());
        let mut tampered = bytes;
        tampered[20] ^= 1;
        assert!(updates::verify(&tampered, &key, now, "1.0.0", 11).is_err());
    }
}

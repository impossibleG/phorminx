#[cfg(feature = "tauri-preview")]
fn main() {
    // Tauri embeds its own version and icon resource. Embedding our legacy
    // winresource a second time makes the Windows linker reject the binary.
    println!("cargo:rerun-if-env-changed=PHORMINX_RELEASE_VERSION");
    bundle_benchmarks();
    tauri_build::build();
}

#[cfg(not(feature = "tauri-preview"))]
fn main() {
    println!("cargo:rerun-if-env-changed=PHORMINX_RELEASE_VERSION");
    println!("cargo:rerun-if-env-changed=PHORMINX_UPDATE_PUBLIC_KEY");
    println!("cargo:rerun-if-changed=../../design/brand/phorminx.ico");
    bundle_benchmarks();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    let release_version = std::env::var("PHORMINX_RELEASE_VERSION").unwrap_or_else(|_| {
        std::env::var("CARGO_PKG_VERSION").expect("package version is present")
    });
    resource
        .set("ProductVersion", &release_version)
        .set("FileVersion", &release_version)
        .set_icon("../../design/brand/phorminx.ico")
        .set("ProductName", "Phorminx")
        .set("FileDescription", "Phorminx local speech-to-text")
        .set("InternalName", "phorminx-app.exe")
        .set("OriginalFilename", "phorminx-app.exe");
    resource
        .compile()
        .expect("failed to embed the Phorminx Windows icon and version resource");
}

/// Embeds `docs/benchmarks/benchmarks.json` and the archived reports in
/// `docs/benchmarks/reports/` for the read-only Benchmarks page. A checkout
/// without them embeds nothing and the page says so.
fn bundle_benchmarks() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/benchmarks");
    let current = directory.join("benchmarks.json");
    let reports = directory.join("reports");
    println!("cargo:rerun-if-changed={}", current.display());
    println!("cargo:rerun-if-changed={}", reports.display());
    let embed = |path: &std::path::Path| format!("include_str!({:?})", path.display().to_string());
    let current_source = if current.is_file() {
        embed(&current)
    } else {
        "\"\"".to_owned()
    };
    let mut archived = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&reports) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if safe_report_name(name) {
                println!("cargo:rerun-if-changed={}", path.display());
                archived.push(format!(
                    "    ({name:?}, {}),
",
                    embed(&path)
                ));
            }
        }
    }
    archived.sort();
    let generated = format!(
        "pub const BUNDLED_CURRENT: &str = {current_source};
pub const BUNDLED_REPORTS: &[(&str, &str)] = &[
{}];
",
        archived.concat()
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set"));
    std::fs::write(out.join("bundled_benchmarks.rs"), generated)
        .expect("failed to write the bundled benchmark list");
}

/// `2026-10-19.json`-style names only.
fn safe_report_name(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(|stem| {
        !stem.is_empty()
            && stem.len() <= 64
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

#[cfg(feature = "tauri-preview")]
fn main() {
    // Tauri embeds its own version and icon resource. Embedding our legacy
    // winresource a second time makes the Windows linker reject the binary.
    println!("cargo:rerun-if-env-changed=PHORMINX_RELEASE_VERSION");
    tauri_build::build();
}

#[cfg(not(feature = "tauri-preview"))]
fn main() {
    println!("cargo:rerun-if-env-changed=PHORMINX_RELEASE_VERSION");
    println!("cargo:rerun-if-env-changed=PHORMINX_UPDATE_PUBLIC_KEY");
    println!("cargo:rerun-if-changed=../../design/brand/phorminx.ico");

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

fn main() {
    println!("cargo:rerun-if-changed=../../design/brand/phorminx.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../design/brand/phorminx.ico")
        .set("ProductName", "Phorminx")
        .set("FileDescription", "Phorminx local speech-to-text")
        .set("InternalName", "phorminx-app.exe")
        .set("OriginalFilename", "phorminx-app.exe");
    resource
        .compile()
        .expect("failed to embed the Phorminx Windows icon and version resource");
}

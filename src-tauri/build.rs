fn main() {
    // Embed Windows UAC manifest for netsh NIC configuration
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winres::WindowsResource::new();
        res.set_manifest_file("windows-manifest.xml");
        if let Err(e) = res.compile() {
            eprintln!("cargo:warning=failed to embed Windows manifest: {e}");
        }
    }
    tauri_build::build();
}

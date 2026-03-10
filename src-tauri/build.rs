fn main() {
    // Let Tauri keep ownership of the Windows resource file.
    // We only override the UAC execution level at link time to avoid manifest
    // merge conflicts and duplicate VERSION resources.
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:level='requireAdministrator'");
    }

    tauri_build::build();
}

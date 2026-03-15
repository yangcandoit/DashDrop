fn main() {
    // CARGO_BIN_NAME is NOT available in build scripts (only in the compiled binary
    // environment), so we use a custom env var to skip Tauri's sidecar existence
    // check while prepare_sidecar.mjs is building the sidecar binaries themselves.
    if std::env::var("DASHDROP_BUILDING_SIDECAR").is_ok() {
        println!("cargo:rerun-if-env-changed=DASHDROP_BUILDING_SIDECAR");
        return;
    }

    // On Windows targets, embed a manifest that activates Common Controls v6
    // using the MSVC linker's native /MANIFESTINPUT flag. This is more reliable
    // than the winresource approach because it bypasses any winres/rc.exe tooling
    // issues and goes directly through the linker.
    // Without comctl32 v6, TaskDialogIndirect (statically imported by tauri-plugin-dialog
    // via the windows crate) cannot be resolved and the app fails to start.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest_path = std::path::Path::new("windows/dashdrop.exe.manifest")
            .canonicalize()
            .unwrap_or_else(|_| std::path::Path::new("windows/dashdrop.exe.manifest").to_path_buf());
        if manifest_path.exists() {
            println!(
                "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
                manifest_path.display()
            );
            println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
            println!("cargo:rustc-link-arg=/MANIFESTUAC:NO");
        }
    }

    tauri_build::try_build(tauri_build::Attributes::new()).expect("failed to run tauri_build");
}

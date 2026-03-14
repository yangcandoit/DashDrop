fn main() {
    // CARGO_BIN_NAME is NOT available in build scripts (only in the compiled binary
    // environment), so we use a custom env var to skip Tauri's sidecar existence
    // check while prepare_sidecar.mjs is building the sidecar binaries themselves.
    if std::env::var("DASHDROP_BUILDING_SIDECAR").is_ok() {
        println!("cargo:rerun-if-env-changed=DASHDROP_BUILDING_SIDECAR");
        return;
    }

    let mut attributes = tauri_build::Attributes::new();

    // On Windows targets, embed a manifest that activates Common Controls v6.
    // Without this, TaskDialogIndirect (used by tauri-plugin-dialog) fails to resolve.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest_path = std::path::Path::new("windows/dashdrop.exe.manifest");
        if manifest_path.exists() {
            let manifest = std::fs::read_to_string(manifest_path)
                .expect("failed to read windows manifest");
            attributes = attributes.windows_attributes(
                tauri_build::WindowsAttributes::new().app_manifest(manifest),
            );
        }
    }

    tauri_build::try_build(attributes).expect("failed to run tauri_build");
}

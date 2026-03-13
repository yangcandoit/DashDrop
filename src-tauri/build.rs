fn main() {
    // CARGO_BIN_NAME is NOT available in build scripts (only in the compiled binary
    // environment), so we use a custom env var to skip Tauri's sidecar existence
    // check while prepare_sidecar.mjs is building the sidecar binaries themselves.
    if std::env::var("DASHDROP_BUILDING_SIDECAR").is_ok() {
        println!("cargo:rerun-if-env-changed=DASHDROP_BUILDING_SIDECAR");
        return;
    }
    tauri_build::build()
}

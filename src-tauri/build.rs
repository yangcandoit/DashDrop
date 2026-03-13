fn main() {
    if let Ok(bin_name) = std::env::var("CARGO_BIN_NAME") {
        if bin_name != "dashdrop" {
            println!("cargo:rerun-if-env-changed=CARGO_BIN_NAME");
            return;
        }
    }
    tauri_build::build()
}

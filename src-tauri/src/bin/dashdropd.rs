#[tokio::main]
async fn main() {
    if let Err(err) = dashdrop_lib::run_headless_daemon().await {
        eprintln!("dashdropd failed: {err:#}");
        std::process::exit(1);
    }
}

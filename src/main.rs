#[tokio::main]
async fn main() {
    if let Err(error) = forge_harness::cli::run().await {
        eprintln!("forge: {error:#}");
        std::process::exit(1);
    }
}

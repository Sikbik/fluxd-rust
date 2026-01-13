#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(err) = fluxd::run_entry(true).await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

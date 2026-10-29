/// Persistent models registered with the migration CLI.
#[path = "../storage.rs"]
mod storage;

/// Runs Toasty's migration CLI against the configured database.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = toasty_cli::Config::load()?;
    let path = std::env::var("ZERO_G_LOG_DB").unwrap_or_else(|_| "zero-g-logs.db".into());
    let db = toasty::Db::builder()
        .models(toasty::models!(
            storage::RequestLog,
            storage::Setting,
            storage::Secret,
            storage::VirtualKey,
            storage::ProviderRecord,
            storage::ProviderCredential
        ))
        .connect(&format!("turso:{path}"))
        .await?;
    toasty_cli::ToastyCli::with_config(db, config)
        .parse_and_run()
        .await?;
    Ok(())
}

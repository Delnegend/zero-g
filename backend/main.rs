/// Settings API contract tests.
#[cfg(test)]
mod contract;
/// Database connection and transaction retry support.
mod database;
/// OpenAI-compatible chat wire types.
mod openai;
/// Upstream provider implementations.
mod provider;
/// Responses API wire types and chat conversion.
mod responses;
/// HTTP server routes and state.
mod server;
/// Persistent database models.
mod storage;
/// Shared encoding helpers.
mod util;

use std::net::SocketAddr;
use std::process::ExitCode;

use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;

use crate::database::Database;
use crate::provider::Providers;
use crate::server::observability::LogStore;
use crate::server::settings::SettingsStore;
use crate::server::{AppState, create_router};

/// Failures while configuring or serving the proxy.
#[derive(Debug, thiserror::Error)]
enum ServerError {
    /// The retention setting is not an integer.
    #[error("log_retention_days must be a positive integer")]
    InvalidLogRetention(#[source] std::num::ParseIntError),
    /// The retention setting falls outside the supported range.
    #[error("log_retention_days must be between 1 and 36500")]
    LogRetentionOutOfRange,
    /// Dashboard authentication could not be configured.
    #[error("{0}")]
    DashboardAuth(String),
    /// The configured bind address could not be parsed.
    #[error("Invalid bind address: {0}")]
    InvalidBindAddress(#[source] std::net::AddrParseError),
    /// The listening socket could not be bound.
    #[error("Failed to bind to {addr}: {source}")]
    Bind {
        /// Address that failed to bind.
        addr: SocketAddr,
        /// Underlying socket error.
        #[source]
        source: std::io::Error,
    },
    /// The HTTP server terminated with an I/O error.
    #[error("Server terminated with error: {0}")]
    Serve(#[source] std::io::Error),
}

/// Initializes storage and runs the web service.
#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args_os().nth(1).is_some() {
        eprintln!("zero-g is a web service and does not accept command-line arguments");
        return ExitCode::from(2);
    }
    dotenvy::dotenv().ok();
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let (settings, logs) = match open_stores().await {
        Ok(stores) => stores,
        Err(e) => {
            error!("Storage error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = run_server(settings, logs).await {
        error!("Server error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Opens settings and request logs and applies the configured retention.
async fn open_stores() -> Result<(SettingsStore, LogStore), String> {
    let path = std::env::var("ZERO_G_LOG_DB").unwrap_or_else(|_| "zero-g-logs.db".into());
    let key = std::env::var("ZERO_G_ENCRYPTION_KEY")
        .map_err(|_| "ZERO_G_ENCRYPTION_KEY is required".to_string())?;
    let database = Database::open(&path).await?;
    let mut logs = LogStore::new(database.clone());
    let settings = SettingsStore::new(database, &key).await?;
    let retention = settings
        .setting_get("log_retention_days")
        .await?
        .unwrap_or_else(|| "30".into())
        .parse::<i64>()
        .map_err(|e| ServerError::InvalidLogRetention(e).to_string())?;
    if !(1..=36500).contains(&retention) {
        return Err(ServerError::LogRetentionOutOfRange.to_string());
    }
    logs.set_retention(retention).await?;
    Ok((settings, logs))
}

/// Constructs shared HTTP state and configures dashboard authentication.
async fn init_state(settings: SettingsStore, logs: LogStore) -> Result<AppState, ServerError> {
    let dashboard_auth = crate::server::dashboard_auth::DashboardAuth::from_env()
        .await
        .map_err(ServerError::DashboardAuth)?;

    Ok(AppState {
        providers: Providers::new(),
        settings,
        logs,
        dashboard_auth,
    })
}

/// Logs the listening address and inference route format.
fn log_endpoints(addr: SocketAddr) {
    info!("zero-g multi-provider proxy running at http://{addr}");
    info!("  Inference routes: http://{addr}/{{kind}}/{{id}}/v1");
    info!("  Inference routes require a virtual key");
}
/// Waits for an interrupt or, on Unix, a termination signal.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::SignalKind;
        match tokio::signal::unix::signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = term.recv() => {},
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Serves the router until shutdown, then stops the provider process.
async fn bind_and_serve(
    addr: SocketAddr,
    app: axum::Router,
    mut provider_process: Option<tokio::process::Child>,
) -> Result<(), ServerError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServerError::Bind { addr, source })?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServerError::Serve)?;

    if let Some(child) = provider_process.as_mut() {
        let _ = child.kill().await;
    }

    Ok(())
}

/// Configures the listener, background tasks, and provider process.
async fn run_server(settings: SettingsStore, logs: LogStore) -> Result<(), ServerError> {
    let bind = std::env::var("ZERO_G_BIND")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "127.0.0.1:8080".into());
    let addr = bind.parse().map_err(ServerError::InvalidBindAddress)?;
    let state = init_state(settings, logs).await?;
    tokio::spawn(crate::server::model_settings::run_background(state.clone()));
    log_endpoints(addr);
    let app = create_router(state);
    let provider_process = crate::provider::start().await;
    bind_and_serve(addr, app, provider_process).await
}

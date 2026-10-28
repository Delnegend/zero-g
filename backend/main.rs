#[cfg(test)]
mod contract;
mod openai;
mod provider;
mod responses;
mod server;
mod storage;
mod util;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;

use crate::provider::antigravity::{AntigravityClient, AuthManager};
use crate::provider::codex::{CodexAuthManager, CodexClient};
use crate::server::observability::LogStore;
use crate::server::settings::SettingsStore;
use crate::server::{AppState, create_router};

#[derive(Debug, thiserror::Error)]
enum ServerError {
    #[error("log_retention_days must be a positive integer")]
    InvalidLogRetention(#[source] std::num::ParseIntError),
    #[error("log_retention_days must be between 1 and 36500")]
    LogRetentionOutOfRange,
    #[error("{0}")]
    LogStore(String),
    #[error("{0}")]
    DashboardAuth(String),
    #[error("Invalid bind address: {0}")]
    InvalidBindAddress(#[source] std::net::AddrParseError),
    #[error("Failed to bind to {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("Server terminated with error: {0}")]
    Serve(#[source] std::io::Error),
}

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

async fn migrate_legacy_settings(settings: &SettingsStore) -> Result<(), String> {
    for (name, env_name) in [
        ("log_retention_days", "ZERO_G_LOG_RETENTION_DAYS"),
        ("zen_url", "ZERO_G_ZEN_URL"),
    ] {
        let marker = format!("{name}.imported");
        if settings.setting_get(&marker).await?.is_none() {
            if settings.setting_get(name).await?.is_none()
                && let Ok(value) = std::env::var(env_name)
                && !value.trim().is_empty()
            {
                settings.setting_put(name, &value).await?;
            }
            settings.setting_put(&marker, "1").await?;
        }
    }
    if settings
        .setting_get("zen_api_key.imported")
        .await?
        .is_none()
    {
        if settings.secret_get("zen_api_key").await?.is_none()
            && let Ok(value) = std::env::var("ZERO_G_ZEN_API_KEY")
            && !value.trim().is_empty()
        {
            settings.secret_put("zen_api_key", &value).await?;
        }
        settings.setting_put("zen_api_key.imported", "1").await?;
    }
    Ok(())
}

async fn open_stores() -> Result<(SettingsStore, LogStore), String> {
    let path = std::env::var("ZERO_G_LOG_DB").unwrap_or_else(|_| "zero-g-logs.db".into());
    let key = std::env::var("ZERO_G_ENCRYPTION_KEY")
        .map_err(|_| "ZERO_G_ENCRYPTION_KEY is required".to_string())?;
    let mut logs = LogStore::open_unpruned(&path).await?;
    let settings = SettingsStore::new(logs.clone(), &key).await?;
    migrate_legacy_settings(&settings).await?;
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
    provider::antigravity::auth::migrate_oauth_settings(&settings).await?;
    Ok((settings, logs))
}

async fn init_state(settings: SettingsStore, logs: LogStore) -> Result<AppState, ServerError> {
    let auth = AuthManager::load(settings.clone())
        .await
        .map_err(ServerError::LogStore)?;
    let client = Arc::new(AntigravityClient::new());
    let codex_auth = CodexAuthManager::load(settings.clone())
        .await
        .map_err(ServerError::LogStore)?;
    let codex_client = Arc::new(CodexClient::new());
    if let Some(a) = &auth
        && let Ok(creds) = a.get_credentials().await
    {
        info!("Google Antigravity: GCP Project ID {}", creds.project_id);
        if let Some(email) = &creds.email {
            info!("Signed in as (Antigravity): {}", email);
        }
    }

    if let Some(ca) = &codex_auth
        && let Ok(creds) = ca.get_credentials().await
    {
        if let Some(email) = &creds.email {
            info!("Signed in as (Codex): {}", email);
        }
        if let Some(plan) = &creds.plan_type {
            info!("OpenAI Codex plan: {}", plan);
        }
        if let Some(acc) = &creds.account_id {
            info!("OpenAI Codex Account ID: {}", acc);
        }
    }

    let dashboard_auth = crate::server::dashboard_auth::DashboardAuth::from_env()
        .await
        .map_err(ServerError::DashboardAuth)?;

    Ok(AppState {
        auth: Arc::new(tokio::sync::RwLock::new(auth)),
        client,
        codex_auth: Arc::new(tokio::sync::RwLock::new(codex_auth)),
        codex_client,
        settings,
        logs,
        dashboard_auth,
    })
}

fn log_endpoints(addr: SocketAddr) {
    info!("zero-g multi-provider proxy running at http://{addr}");
    info!("  Google Antigravity: http://{addr}/agy/v1");
    info!("  OpenAI Codex:       http://{addr}/codex/v1");
    info!("  OpenCode Zen:       http://{addr}/zen/v1");
    info!("  OpenAI compatible:  http://{addr}/openai/v1");
    info!("  Inference routes require a virtual key");
}
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

async fn bind_and_serve(
    addr: SocketAddr,
    app: axum::Router,
    mut shim: Option<tokio::process::Child>,
) -> Result<(), ServerError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServerError::Bind { addr, source })?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServerError::Serve)?;

    if let Some(child) = shim.as_mut() {
        let _ = child.kill().await;
    }

    Ok(())
}

async fn run_server(settings: SettingsStore, logs: LogStore) -> Result<(), ServerError> {
    let bind = std::env::var("ZERO_G_BIND")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "127.0.0.1:8080".into());
    let addr = bind.parse().map_err(ServerError::InvalidBindAddress)?;
    let zen_url = settings
        .setting_get("zen_url")
        .await
        .map_err(ServerError::LogStore)?
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8090/v1".into());
    let state = init_state(settings, logs).await?;
    log_endpoints(addr);
    let app = create_router(state);
    let shim = crate::provider::shim::ensure_zen_shim(&zen_url).await;
    bind_and_serve(addr, app, shim).await
}

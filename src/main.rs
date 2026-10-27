mod antigravity;
mod codex;
mod legacy_import;
mod openai;
mod responses;
mod server;
mod shim;
mod storage;
mod util;
mod zen;

use std::net::SocketAddr;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;

use crate::antigravity::{AntigravityClient, AuthManager};
use crate::codex::{CodexAuthManager, CodexClient};
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

#[derive(Parser)]
#[command(name = "zero-g")]
#[command(
    about = "OpenAI-compatible proxy for Google Antigravity and OpenAI Codex subscriptions",
    long_about = None
)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the OpenAI-compatible proxy server (default)
    Serve,
    /// Interactive OAuth login to authorize Antigravity or Codex
    Login {
        /// Provider to login ("agy" or "codex")
        provider: String,
        /// Use device code authorization flow (Codex only)
        #[arg(long)]
        device: bool,
    },
    /// List available models for a provider
    Models {
        /// Provider to list models for ("agy", "codex", or "zen")
        provider: String,
    },
    /// Show current authentication status
    Status {
        /// Provider to check status for ("agy", "codex", or "zen")
        provider: Option<String>,
    },
    /// Copy a stopped legacy database into a new migrated database
    ImportLegacy {
        /// Existing database path
        source: std::path::PathBuf,
        /// New database path (must not exist)
        destination: std::path::PathBuf,
    },
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Commands::Serve);

    if let Commands::ImportLegacy {
        source,
        destination,
    } = &command
    {
        if let Err(e) = legacy_import::import(source, destination).await {
            error!("Legacy import failed: {e}");
            std::process::exit(1);
        }
        println!("Imported legacy database into {}", destination.display());
        return;
    }

    let (settings, logs) = match open_stores().await {
        Ok(stores) => stores,
        Err(e) => {
            error!("Storage error: {e}");
            std::process::exit(1);
        }
    };
    match command {
        Commands::Serve => {
            if let Err(e) = run_server(settings, logs).await {
                error!("Server error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Login { provider, device } => {
            if let Err(e) = run_login(&settings, provider, device).await {
                error!("Login failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::Models { provider } => {
            if let Err(e) = run_models(&settings, provider).await {
                error!("Failed to fetch models: {e}");
                std::process::exit(1);
            }
        }
        Commands::Status { provider } => {
            if let Err(e) = run_status(&settings, provider).await {
                error!("Status check failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::ImportLegacy { .. } => {}
    }
}

async fn migrate_legacy_settings(settings: &SettingsStore) -> Result<(), String> {
    for (name, env_name) in [
        ("bind", "ZERO_G_BIND"),
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
    antigravity::auth::migrate_oauth_settings(&settings).await?;
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
    let bind = settings
        .setting_get("bind")
        .await
        .map_err(ServerError::LogStore)?
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
    let shim = crate::shim::ensure_zen_shim(&zen_url).await;
    bind_and_serve(addr, app, shim).await
}

async fn run_login(settings: &SettingsStore, provider: String, device: bool) -> Result<(), String> {
    let prov = provider.to_lowercase();
    match prov.as_str() {
        "codex" | "openai-codex" => {
            let creds = codex::login::run_login(device).await?;
            CodexAuthManager::save(settings, &creds).await?;
            println!("\nOpenAI Codex login successful!");
            println!("Credentials saved to encrypted storage.");
            if let Some(email) = &creds.email {
                println!("Account: {email}");
            }
            if let Some(acc) = &creds.account_id {
                println!("Account ID: {acc}");
            }
            if let Some(plan) = &creds.plan_type {
                println!("Plan Type: {plan}");
            }
            println!("\nYou can now run `zero-g serve` to use Codex models at `/codex/v1`.");
            Ok(())
        }
        "agy" | "antigravity" => {
            let creds = antigravity::login::run_login(settings).await?;
            AuthManager::save(settings, &creds).await?;
            println!("\nLogin successful!");
            println!("Credentials saved to encrypted storage.");
            println!("GCP Project ID: {}", creds.project_id);
            if let Some(email) = &creds.email {
                println!("Account: {email}");
            }
            println!("\nYou can now run `zero-g serve` to start the OpenAI proxy.");

            Ok(())
        }
        other => Err(format!(
            "Unknown provider '{other}'. Supported providers: 'agy', 'codex'"
        )),
    }
}

async fn run_models(settings: &SettingsStore, provider: String) -> Result<(), String> {
    let prov = provider.to_lowercase();
    match prov.as_str() {
        "codex" | "openai-codex" => run_codex_models(settings).await,
        "agy" | "antigravity" => run_antigravity_models(settings).await,
        "zen" | "opencode-zen" => {
            run_zen_models();
            Ok(())
        }
        other => Err(format!(
            "Unknown provider '{other}'. Supported providers: 'agy', 'codex', 'zen'"
        )),
    }
}

fn run_zen_models() {
    println!("{:<32} {:<16}", "MODEL ID", "OWNED BY");
    println!("{:-<32} {:-<16}", "", "");
    let json = crate::zen::models_json();
    if let Some(arr) = json.get("data").and_then(|d| d.as_array()) {
        for m in arr {
            let id = m.get("id").and_then(|i| i.as_str()).unwrap_or("-");
            let owned = m.get("owned_by").and_then(|o| o.as_str()).unwrap_or("-");
            println!("{id:<32} {owned:<16}");
        }
    }
}

async fn run_antigravity_models(settings: &SettingsStore) -> Result<(), String> {
    let auth = AuthManager::load(settings.clone())
        .await?
        .ok_or_else(|| "No Antigravity credentials found; run `zero-g login agy`".to_string())?;
    let client = AntigravityClient::new();
    let creds = auth.get_credentials().await?;

    println!(
        "\nFetching Google Antigravity models (project {})...",
        creds.project_id
    );
    let models = client.fetch_models(&creds).await?;

    println!("Available Antigravity Models ({} total):", models.len());
    println!(
        "{:<32} {:<30} {:<10} {:<10}",
        "MODEL ID", "DISPLAY NAME", "THINKING", "MAX TOKENS"
    );
    println!("{:-<32} {:-<30} {:-<10} {:-<10}", "", "", "", "");
    for (id, info) in models {
        let name = info.display_name.unwrap_or_else(|| id.clone());
        let thinking = if info.supports_thinking.unwrap_or(false) {
            "yes"
        } else {
            "no"
        };
        let tokens = info
            .max_output_tokens
            .map_or("-".to_string(), |t| t.to_string());
        println!("{id:<32} {name:<30} {thinking:<10} {tokens:<10}");
    }
    Ok(())
}

async fn run_codex_models(settings: &SettingsStore) -> Result<(), String> {
    let codex_auth = CodexAuthManager::load(settings.clone())
        .await?
        .ok_or_else(|| "No Codex credentials found; run `zero-g login codex`".to_string())?;
    println!("\nFetching OpenAI Codex models...");
    let models_resp = codex::models::fetch_codex_models(&codex_auth).await?;

    println!(
        "Available OpenAI Codex Models ({} total):",
        models_resp.data.len()
    );
    println!("{:<32} {:<16}", "MODEL ID", "OWNED BY");
    println!("{:-<32} {:-<16}", "", "");
    for m in models_resp.data {
        println!("{:<32} {:<16}", m.id, m.owned_by);
    }
    Ok(())
}

async fn run_status(settings: &SettingsStore, provider: Option<String>) -> Result<(), String> {
    let prov = provider.map(|p| p.to_lowercase());

    if prov.as_deref() == Some("codex") || prov.as_deref() == Some("openai-codex") {
        return print_codex_status(settings).await;
    }

    if prov.as_deref() == Some("antigravity") || prov.as_deref() == Some("agy") {
        return print_antigravity_status(settings).await;
    }

    if prov.as_deref() == Some("zen") || prov.as_deref() == Some("opencode-zen") {
        print_zen_status(settings).await?;
        return Ok(());
    }

    print_antigravity_status(settings).await?;
    print_codex_status(settings).await?;
    print_zen_status(settings).await?;
    Ok(())
}

async fn print_zen_status(settings: &SettingsStore) -> Result<(), String> {
    println!("\n--- OpenCode Zen ---");
    println!("Status: Active (free tier)");
    let url = settings
        .setting_get("zen_url")
        .await?
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8090/v1".into());
    println!("Base URL: {url}");
    Ok(())
}
async fn print_antigravity_status(settings: &SettingsStore) -> Result<(), String> {
    println!("\n--- Google Antigravity ---");
    let Some(auth) = AuthManager::load(settings.clone()).await? else {
        println!("Status: Not configured");
        return Ok(());
    };

    let creds = auth.get_credentials().await?;

    println!("Status: Authenticated");
    println!("GCP Project ID: {}", creds.project_id);
    if let Some(email) = &creds.email {
        println!("Account: {email}");
    }
    if let Some(exp) = creds.expires_at {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let remaining_secs = (exp - now_ms) / 1000;
        if remaining_secs > 0 {
            println!(
                "Token valid for: {} seconds (~{} minutes)",
                remaining_secs,
                remaining_secs / 60
            );
        } else {
            println!("Token: expired (auto-refresh enabled via refresh_token)");
        }
    }
    println!("Has refresh token: {}", creds.refresh_token.is_some());
    Ok(())
}

async fn print_codex_status(settings: &SettingsStore) -> Result<(), String> {
    println!("\n--- OpenAI Codex (ChatGPT OAuth) ---");
    let Some(ca) = CodexAuthManager::load(settings.clone()).await? else {
        println!("Status: Not configured");
        return Ok(());
    };
    let creds = ca.get_credentials().await?;
    println!("Status: Authenticated");
    if let Some(email) = &creds.email {
        println!("Account: {email}");
    }
    if let Some(acc) = &creds.account_id {
        println!("Account ID: {acc}");
    }
    if let Some(plan) = &creds.plan_type {
        println!("Plan Type: {plan}");
    }
    if let Some(res) = &creds.residency {
        println!("Data Residency: {res}");
    }
    if let Some(exp) = creds.expires_at {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let remaining_secs = (exp - now_ms) / 1000;
        if remaining_secs > 0 {
            println!(
                "Token valid for: {} seconds (~{} minutes)",
                remaining_secs,
                remaining_secs / 60
            );
        } else {
            println!("Token: expired (auto-refresh enabled via refresh_token)");
        }
    }
    println!("Has refresh token: {}", creds.refresh_token.is_some());
    Ok(())
}

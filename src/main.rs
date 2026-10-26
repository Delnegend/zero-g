mod antigravity;
mod codex;
mod openai;
mod responses;
mod server;
mod shim;
mod util;
mod zen;

use std::net::SocketAddr;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;

use crate::antigravity::{AntigravityClient, AuthManager};
use crate::codex::{CodexAuthManager, CodexClient};
use crate::server::{AppState, create_router};
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

    match command {
        Commands::Serve => {
            let (host, port) = resolve_bind_config();
            if let Err(e) = run_server(host, port).await {
                error!("Server error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Login { provider, device } => {
            if let Err(e) = run_login(provider, device).await {
                error!("Login failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::Models { provider } => {
            if let Err(e) = run_models(provider).await {
                error!("Failed to fetch models: {e}");
                std::process::exit(1);
            }
        }
        Commands::Status { provider } => {
            if let Err(e) = run_status(provider).await {
                error!("Status check failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn resolve_bind_config() -> (String, u16) {
    if let Ok(bind) = std::env::var("ZERO_G_BIND")
        && let Some((h, p)) = bind.rsplit_once(':')
        && let Ok(port) = p.parse::<u16>()
    {
        let host = if h.is_empty() { "127.0.0.1" } else { h };
        return (host.to_string(), port);
    }
    ("127.0.0.1".to_string(), 8080)
}

async fn init_state() -> Result<AppState, String> {
    let auth = AuthManager::load().ok();
    let client = Arc::new(AntigravityClient::new());
    let codex_auth = CodexAuthManager::load().ok();
    let codex_client = Arc::new(CodexClient::new());

    if auth.is_none() && codex_auth.is_none() {
        return Err(
            "No credentials found for Google Antigravity or OpenAI Codex.\nRun `zero-g login agy` or `zero-g login codex` to authenticate."
                .to_string(),
        );
    }

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

    let tokens = Arc::new(crate::server::ProviderTokens::from_env());

    Ok(AppState {
        auth,
        client,
        codex_auth,
        codex_client,
        tokens,
    })
}

fn parse_socket_addr(host: &str, port: u16) -> Result<SocketAddr, String> {
    format!("{host}:{port}")
        .parse()
        .map_err(|e| format!("Invalid bind address: {e}"))
}

fn log_endpoints(addr: SocketAddr, tokens: &crate::server::ProviderTokens) {
    info!("zero-g multi-provider proxy running at http://{addr}");
    info!(
        "  Google Antigravity: http://{addr}/agy/v1 (auth: {})",
        if tokens.agy.is_some() {
            "protected"
        } else {
            "open"
        }
    );
    info!(
        "  OpenAI Codex:       http://{addr}/codex/v1 (auth: {})",
        if tokens.codex.is_some() {
            "protected"
        } else {
            "open"
        }
    );
    info!(
        "  OpenCode Zen:       http://{addr}/zen/v1 (auth: {})",
        if tokens.zen.is_some() {
            "protected"
        } else {
            "open"
        }
    );
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
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind to {addr}: {e}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("Server terminated with error: {e}"))?;

    if let Some(child) = shim.as_mut() {
        let _ = child.kill().await;
    }

    Ok(())
}

async fn run_server(host: String, port: u16) -> Result<(), String> {
    let state = init_state().await?;
    let addr = parse_socket_addr(&host, port)?;
    log_endpoints(addr, &state.tokens);
    let app = create_router(state);
    let shim = crate::shim::ensure_zen_shim().await;
    bind_and_serve(addr, app, shim).await
}

async fn run_login(provider: String, device: bool) -> Result<(), String> {
    let prov = provider.to_lowercase();
    match prov.as_str() {
        "codex" | "openai-codex" => {
            let creds = codex::login::run_login(device).await?;
            let path = CodexAuthManager::save_to_file(&creds)?;
            println!("\nOpenAI Codex login successful!");
            println!("Credentials saved to: {}", path.display());
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
            let creds = antigravity::login::run_login().await?;
            let path = AuthManager::save_to_file(&creds)?;
            println!("\nLogin successful!");
            println!("Credentials saved to: {}", path.display());
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

async fn run_models(provider: String) -> Result<(), String> {
    let prov = provider.to_lowercase();
    match prov.as_str() {
        "codex" | "openai-codex" => run_codex_models().await,
        "agy" | "antigravity" => run_antigravity_models().await,
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

async fn run_antigravity_models() -> Result<(), String> {
    let auth = AuthManager::load()?;
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

async fn run_codex_models() -> Result<(), String> {
    let codex_auth = CodexAuthManager::load()?;
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

async fn run_status(provider: Option<String>) -> Result<(), String> {
    let prov = provider.map(|p| p.to_lowercase());

    if prov.as_deref() == Some("codex") || prov.as_deref() == Some("openai-codex") {
        print_codex_status();
        return Ok(());
    }

    if prov.as_deref() == Some("antigravity") || prov.as_deref() == Some("agy") {
        return print_antigravity_status().await;
    }

    if prov.as_deref() == Some("zen") || prov.as_deref() == Some("opencode-zen") {
        print_zen_status();
        return Ok(());
    }

    let _ = print_antigravity_status().await;
    print_codex_status();
    print_zen_status();
    Ok(())
}

fn print_zen_status() {
    println!("\n--- OpenCode Zen ---");
    println!("Status: Active (free tier)");
    println!("Base URL: {}", crate::shim::zen_base_url());
}
async fn print_antigravity_status() -> Result<(), String> {
    println!("\n--- Google Antigravity ---");
    let Ok(auth) = AuthManager::load() else {
        println!("Status: Not configured");
        return Ok(());
    };

    let creds = match auth.get_credentials().await {
        Ok(c) => c,
        Err(e) => {
            println!("Status: Error loading credentials ({e})");
            return Ok(());
        }
    };

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

fn print_codex_status() {
    println!("\n--- OpenAI Codex (ChatGPT OAuth) ---");
    let Ok(ca) = CodexAuthManager::load() else {
        println!("Status: Not configured");
        return;
    };

    let creds = match futures_util::FutureExt::now_or_never(ca.get_credentials()) {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            println!("Status: Error loading credentials ({e})");
            return;
        }
        None => {
            println!("Status: Pending refresh");
            return;
        }
    };
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_bind_config() {
        let (h, p) = resolve_bind_config();
        assert_ne!(h, "");
        assert!(p > 0);
    }
}

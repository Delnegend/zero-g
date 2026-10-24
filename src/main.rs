mod antigravity;
mod auth;
mod login;
mod openai;
mod responses;
mod server;

use std::net::SocketAddr;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;

use crate::antigravity::AntigravityClient;
use crate::auth::AuthManager;
use crate::server::{AppState, create_router};

#[derive(Parser)]
#[command(name = "zero-g")]
#[command(about = "OpenAI-compatible proxy for Google Antigravity subscription", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the OpenAI-compatible proxy server (default)
    Serve {
        /// Optional API key for proxy authentication
        #[arg(long, env = "ZERO_G_API_KEY")]
        api_key: Option<String>,
    },
    /// Interactive Google OAuth login to authorize Antigravity
    Login,
    /// List available models on your Google Antigravity account
    Models,
    /// Show current authentication status and project ID
    Status,
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
    let command = cli.command.unwrap_or(Commands::Serve { api_key: None });

    match command {
        Commands::Serve { api_key } => {
            let (host, port) = resolve_bind_config();
            let api_key = api_key
                .or_else(|| std::env::var("ZERO_G_API_KEY").ok())
                .or_else(|| std::env::var("GAP_API_KEY").ok());
            if let Err(e) = run_server(host, port, api_key).await {
                error!("Server error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Login => {
            if let Err(e) = run_login().await {
                error!("Login failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::Models => {
            if let Err(e) = run_models().await {
                error!("Failed to fetch models: {e}");
                std::process::exit(1);
            }
        }
        Commands::Status => {
            if let Err(e) = run_status().await {
                error!("Status check failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn resolve_bind_config() -> (String, u16) {
    let (env_host, env_port) = match std::env::var("ZERO_G_BIND")
        .or_else(|_| std::env::var("GAP_BIND"))
        .or_else(|_| std::env::var("BIND"))
    {
        Ok(bind) => match bind.rsplit_once(':') {
            Some((h, p)) => (
                if h.is_empty() {
                    None
                } else {
                    Some(h.to_string())
                },
                p.parse::<u16>().ok(),
            ),
            None => (Some(bind), None),
        },
        Err(_) => (None, None),
    };

    let host = env_host
        .or_else(|| std::env::var("ZERO_G_HOST").ok())
        .or_else(|| std::env::var("GAP_HOST").ok())
        .or_else(|| std::env::var("HOST").ok())
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let port = env_port
        .or_else(|| {
            std::env::var("ZERO_G_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
        })
        .or_else(|| std::env::var("GAP_PORT").ok().and_then(|p| p.parse().ok()))
        .or_else(|| std::env::var("PORT").ok().and_then(|p| p.parse().ok()))
        .unwrap_or(8080);

    (host, port)
}

async fn run_server(host: String, port: u16, api_key: Option<String>) -> Result<(), String> {
    let auth = AuthManager::load()?;
    let client = Arc::new(AntigravityClient::new());

    let creds = auth.get_credentials().await?;
    info!("Using GCP Project ID: {}", creds.project_id);
    if let Some(email) = &creds.email {
        info!("Signed in as: {}", email);
    }
    if api_key.is_some() {
        info!("Bearer API key authentication enabled");
    } else {
        info!("No API key required (local access open)");
    }

    let state = AppState {
        auth,
        client,
        api_key,
    };

    let app = create_router(state);
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| format!("Invalid bind address: {e}"))?;

    info!("OpenAI-compatible proxy running at http://{addr}");
    info!("API Base URL: http://{addr}/v1");
    info!("Models: http://{addr}/v1/models");
    info!("Chat Completions: http://{addr}/v1/chat/completions");
    info!("Responses: http://{addr}/v1/responses");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind to {addr}: {e}"))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("Server terminated with error: {e}"))?;

    Ok(())
}

async fn run_login() -> Result<(), String> {
    let creds = login::run_login().await?;
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

async fn run_models() -> Result<(), String> {
    let auth = AuthManager::load()?;
    let client = AntigravityClient::new();
    let creds = auth.get_credentials().await?;

    println!("Fetching models for project {}...", creds.project_id);
    let models = client.fetch_models(&creds).await?;

    println!("\nAvailable Models ({} total):", models.len());
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
    println!("\nConvenience aliases supported:");
    println!("  claude-3-5-sonnet / claude-sonnet -> claude-sonnet-5-5-medium");
    println!("  claude-opus                     -> claude-opus-5-5-medium");
    println!("  gemini-flash                    -> gemini-3.8-flash-low");
    println!("  gemini-pro / gemini-4-argon     -> gemini-4-argon-low");
    println!("  gpt-oss                         -> gpt-oss-120b-medium");
    Ok(())
}

async fn run_status() -> Result<(), String> {
    let auth = AuthManager::load()?;
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

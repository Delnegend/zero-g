//! Pushes the official `agy` CLI's Antigravity quota and plan to a Zero-G gateway.
//!
//! Runs once and exits, so a scheduler (a Homebrew service, cron, a systemd timer) decides how
//! often. Zero-G never contacts Google for this: the CLI already knows its own quota.

mod agy;
mod config;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

/// What to show for `--help`.
const HELP: &str = "\
zero-g-agy-quota: push agy's Antigravity quota and plan to Zero-G

Usage: zero-g-agy-quota [--config FILE]

Settings (environment, or KEY=value lines in the settings file):
  ZERO_G_URL              Gateway root, without /v1
  ZERO_G_VIRTUAL_KEY      Virtual key without a provider allowlist
  ZERO_G_AGY_ACCOUNT      Name of the Google Antigravity credential in Zero-G
  AGY_BIN                 Path to agy (default: agy)
  AGY_TIER_REFRESH_HOURS  Hours between plan reads (default: 24)
  ZERO_G_CACHE_DIR        Plan cache directory (default: ~/.cache/zero-g)

The settings file defaults to ~/.config/zero-g/agy-quota.env.
";

/// What the command line asked for.
enum Action {
    /// Collect and push once.
    Run(Option<PathBuf>),
    /// Print usage.
    Help,
    /// Print the version.
    Version,
}

/// Read the command line.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut args = args;
    let mut config = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "--config" => {
                config = Some(PathBuf::from(
                    args.next().ok_or("--config needs a file path")?,
                ));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Action::Run(config))
}

/// Post the report to the gateway.
async fn push(settings: &config::Settings, body: &Value) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("zero-g-agy-quota/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(format!("{}/v1/agy/quota", settings.url))
        .bearer_auth(&settings.key)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("Zero-G did not accept the report: {error}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let reason: String = response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    Err(format!(
        "Zero-G did not accept the report: HTTP {status} {reason}"
    ))
}

/// One collection run.
async fn run(config: Option<PathBuf>) -> Result<(), String> {
    let settings = config::load(config.as_deref())?;
    let report = agy::usage(&settings.agy)
        .await
        .map_err(|error| format!("could not read quota from agy: {error}"))?;
    let mut body = json!({"account": settings.account, "report": report});
    if let (Some(tier), Some(object)) = (agy::cached_tier(&settings).await, body.as_object_mut()) {
        object.insert("tier".to_owned(), tier);
    }
    push(&settings, &body).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let action = match parse_args(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(message) => {
            eprintln!("zero-g-agy-quota: {message}");
            return ExitCode::from(2);
        }
    };
    match action {
        Action::Help => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Action::Version => {
            println!("zero-g-agy-quota {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Action::Run(config) => match run(config).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("zero-g-agy-quota: {message}");
                ExitCode::FAILURE
            }
        },
    }
}

//! Reports the official `agy` CLI's Antigravity quota, plan and usage to a Zero-G gateway.
//!
//! Two commands share one binary, one settings file and one version:
//!
//! - `quota` pushes the quota and plan once; a scheduler (a Homebrew service, cron, a systemd
//!   timer) decides how often.
//! - `observe` is agy's `PostInvocation` hook and records each model response.
//!
//! Zero-G never contacts Google for any of this: the CLI already knows its own quota.

mod agy;
mod config;
mod gateway;
mod observe;
mod quota;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// What to show for `--help`.
const HELP: &str = "\
zero-g-agy-integration: report agy's Antigravity quota, plan and usage to Zero-G

Usage: zero-g-agy-integration <command> [--config FILE]

Commands:
  quota     Push agy's quota and plan once (run it on a schedule)
  observe   agy's PostInvocation hook: record the model response (reads the event on stdin)

Settings (environment, or KEY=value lines in the settings file):
  ZERO_G_URL              Gateway root, without /v1
  ZERO_G_VIRTUAL_KEY      Virtual key without a provider allowlist
  ZERO_G_AGY_ACCOUNT      Name of the Google Antigravity credential in Zero-G (quota only)
  AGY_BIN                 Path to agy (default: agy)
  AGY_TIER_REFRESH_HOURS  Hours between plan reads (default: 24)
  ZERO_G_CACHE_DIR        Plan cache directory (default: ~/.cache/zero-g)

The settings file defaults to ~/.config/zero-g/agy-integration.env.
";

/// The longest `observe` may run, safely inside agy's 5 second hook timeout.
const OBSERVE_BUDGET: Duration = Duration::from_secs(4);

/// One of the two commands.
enum Command {
    /// Push the quota once.
    Quota,
    /// Run the hook.
    Observe,
}

/// What the command line asked for.
enum Action {
    /// Push the quota once.
    Quota(Option<PathBuf>),
    /// Run the hook.
    Observe(Option<PathBuf>),
    /// Print usage.
    Help,
    /// Print the version.
    Version,
}

/// Read the command line.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut args = args;
    let mut command = None;
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
            "quota" if command.is_none() => command = Some(Command::Quota),
            "observe" if command.is_none() => command = Some(Command::Observe),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    match command {
        Some(Command::Quota) => Ok(Action::Quota(config)),
        Some(Command::Observe) => Ok(Action::Observe(config)),
        None => Err("name a command: quota or observe (see --help)".to_owned()),
    }
}

/// Run one command and return its exit code.
async fn execute(action: Action) -> ExitCode {
    match action {
        Action::Help => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Action::Version => {
            println!("zero-g-agy-integration {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Action::Quota(config) => {
            let result = match config::load(config.as_deref()) {
                Ok(settings) => quota::run(&settings).await,
                Err(message) => Err(message),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("zero-g-agy-integration: {message}");
                    ExitCode::FAILURE
                }
            }
        }
        Action::Observe(config) => {
            // A hook must stay silent and succeed, whatever goes wrong.
            let settings = config::load(config.as_deref()).ok();
            let _ = tokio::time::timeout(OBSERVE_BUDGET, observe::collect(settings.as_ref())).await;
            println!("{{}}");
            ExitCode::SUCCESS
        }
    }
}

fn main() -> ExitCode {
    let action = match parse_args(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(message) => {
            eprintln!("zero-g-agy-integration: {message}");
            return ExitCode::from(2);
        }
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        eprintln!("zero-g-agy-integration: cannot start the async runtime");
        return ExitCode::FAILURE;
    };
    let code = runtime.block_on(execute(action));
    // A hook whose stdin never closes must not keep the process alive after its time budget.
    runtime.shutdown_background();
    code
}

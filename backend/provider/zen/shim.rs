use std::time::Duration;

use tracing::{info, warn};

/// Maximum time to wait for a spawned shim to become ready.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// Delay between shim readiness probes.
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Timeout for a single shim health probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Extract the TCP port from an http(s) base URL like `http://127.0.0.1:8090/v1`.
fn shim_port(base_url: &str) -> Option<u16> {
    let authority = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))?
        .split('/')
        .next()?;
    let port_str = authority.rsplit(':').next()?;
    // No explicit port (e.g. a hostname, or IPv6 without port handling) -> give up.
    if port_str == authority || port_str.contains(']') || port_str.is_empty() {
        return None;
    }
    port_str.parse().ok()
}

/// Check whether the shim health endpoint responds successfully.
pub(super) async fn is_alive(client: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    match tokio::time::timeout(PROBE_TIMEOUT, client.get(&url).send()).await {
        Ok(Ok(resp)) => resp.status().is_success(),
        _ => false,
    }
}

/// Locate a bundled shim next to the current executable.
fn sibling_shim() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join("zen-shim");
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

/// Ensure something answers at the Zen shim URL, spawning the bundled
/// `zen-shim` sibling binary when needed. Returns the child when we spawned
/// it so the caller can terminate it on shutdown.
pub(crate) async fn ensure_zen_shim(base_url: &str) -> Option<tokio::process::Child> {
    let client = reqwest::Client::new();

    if is_alive(&client, base_url).await {
        info!("Zen shim already running at {base_url}");
        return None;
    }

    let Some(path) = sibling_shim() else {
        warn!(
            "No zen shim at {base_url} and no bundled `zen-shim` binary found; /zen routes will fail"
        );
        return None;
    };
    let Some(port) = shim_port(base_url) else {
        warn!("Cannot parse port from Zen URL {base_url}; skipping shim auto-spawn");
        return None;
    };

    info!("Spawning bundled zen shim on port {port}");
    let child = tokio::process::Command::new(&path)
        .env("PORT", port.to_string())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            warn!(
                "Failed to spawn {}: {e}; /zen routes will fail",
                path.display()
            );
        })
        .ok()?;

    let start = tokio::time::Instant::now();
    while start.elapsed() < READY_TIMEOUT {
        if is_alive(&client, base_url).await {
            info!("Zen shim ready at {base_url}");
            return Some(child);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    warn!("Bundled zen shim did not become ready within 10s; /zen routes may fail");
    Some(child)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_shim_port() {
        assert_eq!(shim_port("http://127.0.0.1:8090/v1"), Some(8090));
        assert_eq!(shim_port("http://127.0.0.1:8099"), Some(8099));
        assert_eq!(shim_port("http://localhost:1234/v1/"), Some(1234));
        assert_eq!(shim_port("http://127.0.0.1/v1"), None);
        assert_eq!(shim_port("not-a-url"), None);
    }
}

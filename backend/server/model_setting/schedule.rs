use std::time::{Duration, Instant};

use super::datasheet::sync_datasheets;
use crate::server::{AppState, settings::SettingsStore};

/// Frequency at which background jobs check their schedules.
const SCHEDULER_POLL: Duration = Duration::from_secs(5);

/// Determine whether a nonzero interval has elapsed since the last run.
fn due(last: Option<Instant>, seconds: u64) -> bool {
    seconds != 0 && last.is_none_or(|last| last.elapsed().as_secs() >= seconds)
}
/// Load the configured pricing and discovery intervals from settings.
async fn load_intervals(store: &SettingsStore) -> (u64, u64) {
    let pricing = store
        .setting_get("pricing_interval_hours")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(24);
    let discovery = store
        .setting_get("discovery_interval_minutes")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60);
    (pricing, discovery)
}

/// Run independent periodic datasheet and provider-discovery loops.
pub(crate) async fn run_background(state: AppState) {
    // Discovery must not wait behind large or slow datasheet downloads.
    let pricing = async {
        let mut last = None;
        loop {
            let (pricing_hours, _) = load_intervals(&state.settings).await;
            if due(last, pricing_hours.saturating_mul(3600)) {
                last = Some(Instant::now());
                if let Err(message) = sync_datasheets(&state.settings).await {
                    tracing::warn!("Model datasheet sync failed: {message}");
                }
            }
            tokio::time::sleep(SCHEDULER_POLL).await;
        }
    };
    let discovery = async {
        let mut last = None;
        loop {
            let (_, discovery_mins) = load_intervals(&state.settings).await;
            if due(last, discovery_mins.saturating_mul(60)) {
                last = Some(Instant::now());
                if let Err(error) = crate::server::model_catalog::refresh_catalog(&state).await {
                    tracing::error!("Failed to refresh model catalog: {error}");
                }
            }
            tokio::time::sleep(SCHEDULER_POLL).await;
        }
    };
    tokio::join!(pricing, discovery);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zero intervals disable jobs and new schedules run immediately.
    #[test]
    fn disabled_and_initial_schedules() {
        assert!(!due(None, 0));
        assert!(due(None, 60));
        assert!(!due(Some(Instant::now()), 60));
    }
}

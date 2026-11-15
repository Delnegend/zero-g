use std::time::{Duration, Instant};

use super::datasheet::sync_datasheets;
use crate::server::settings::keys::{
    DISCOVERY_INTERVAL_MINUTES, IntSetting, PRICING_INTERVAL_HOURS,
};
use crate::server::{AppState, settings::SettingsStore};

/// Frequency at which background jobs check their schedules.
const SCHEDULER_POLL: Duration = Duration::from_secs(5);

/// Determine whether a nonzero interval has elapsed since the last run.
fn due(last: Option<Instant>, seconds: u64) -> bool {
    seconds != 0 && last.is_none_or(|last| last.elapsed().as_secs() >= seconds)
}

/// Read one interval setting in seconds. An unreadable or invalid value skips
/// the run and is logged once until the error changes or clears.
async fn interval(
    store: &SettingsStore,
    setting: &IntSetting,
    unit: u64,
    last_error: &mut Option<String>,
) -> Option<u64> {
    match setting.read(&mut (*store.database.db).clone()).await {
        Ok(value) => {
            *last_error = None;
            Some(value.saturating_mul(unit))
        }
        Err(error) => {
            let error = error.to_string();
            if last_error.as_ref() != Some(&error) {
                tracing::warn!("Skipping scheduled job: {error}");
                *last_error = Some(error);
            }
            None
        }
    }
}

/// Run independent periodic datasheet and provider-discovery loops.
pub(crate) async fn run_background(state: AppState) {
    // Discovery must not wait behind large or slow datasheet downloads.
    let pricing = async {
        let mut last = None;
        let mut last_error = None;
        loop {
            let seconds = interval(
                &state.settings,
                &PRICING_INTERVAL_HOURS,
                3600,
                &mut last_error,
            )
            .await;
            if seconds.is_some_and(|seconds| due(last, seconds)) {
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
        let mut last_error = None;
        loop {
            let seconds = interval(
                &state.settings,
                &DISCOVERY_INTERVAL_MINUTES,
                60,
                &mut last_error,
            )
            .await;
            if seconds.is_some_and(|seconds| due(last, seconds)) {
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

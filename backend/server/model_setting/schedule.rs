use std::time::{Duration, Instant};

use super::{datasheet::sync_datasheets, load_settings};
use crate::server::AppState;

/// Frequency at which background jobs check their schedules.
const SCHEDULER_POLL: Duration = Duration::from_secs(5);

/// Determine whether a nonzero interval has elapsed since the last run.
fn due(last: Option<Instant>, seconds: u64) -> bool {
    seconds != 0 && last.is_none_or(|last| last.elapsed().as_secs() >= seconds)
}

/// Run independent periodic datasheet and provider-discovery loops.
pub(crate) async fn run_background(state: AppState) {
    // Discovery must not wait behind large or slow datasheet downloads.
    let pricing = async {
        let mut last = None;
        loop {
            match load_settings(&state.settings).await {
                Ok(settings) if due(last, settings.pricing_interval_hours.saturating_mul(3600)) => {
                    last = Some(Instant::now());
                    if let Err(message) = sync_datasheets(&state.settings).await {
                        tracing::warn!("Model datasheet sync failed: {message}");
                    }
                }
                Ok(_) => {}
                Err(_) => tracing::warn!("Unable to load model settings for datasheet refresh"),
            }
            tokio::time::sleep(SCHEDULER_POLL).await;
        }
    };
    let discovery = async {
        let mut last = None;
        loop {
            match load_settings(&state.settings).await {
                Ok(settings)
                    if due(last, settings.discovery_interval_minutes.saturating_mul(60)) =>
                {
                    last = Some(Instant::now());
                    if let Err(error) = crate::server::model_catalog::refresh_catalog(&state).await
                    {
                        tracing::error!("Failed to refresh model catalog: {error}");
                    }
                }
                Ok(_) => {}
                Err(_) => tracing::warn!("Unable to load model settings for model discovery"),
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

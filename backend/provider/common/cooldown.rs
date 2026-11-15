//! In-process cooldowns for credentials an upstream has rate limited.
//!
//! A cooled-down credential is skipped until its deadline, so a failover loop
//! stops re-sending requests to an account that just refused them. State is
//! kept in memory: it lasts seconds to hours, and a restart only means the next
//! real 429 sets it again.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;

/// Wait used when a 429 carries no usable reset hint.
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);
/// Longest cooldown ever applied. No Anthropic usage window exceeds 7 days, and a
/// monthly billing reset must never bench a credential for weeks, because nothing
/// would reach upstream to observe that the limit lifted.
const MAX_COOLDOWN: Duration = Duration::from_hours(7 * 24);

/// Deadline per credential identifier.
static COOLDOWNS: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Lock the map, recovering it if a panicking holder poisoned the lock.
fn cooldowns() -> std::sync::MutexGuard<'static, HashMap<String, Instant>> {
    COOLDOWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How long to wait after a 429, from its headers.
///
/// Prefers `retry-after` (seconds), then Anthropic's subscription
/// `anthropic-ratelimit-unified-reset` (Unix seconds) unless that reset is the
/// monthly overage billing boundary, then a short default. Always capped.
pub(crate) fn wait_for(headers: &HeaderMap) -> Duration {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let retry_after = header("retry-after")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let unified_reset = || {
        let reset = header("anthropic-ratelimit-unified-reset")?
            .trim()
            .parse::<u64>()
            .ok()?;
        // Under the overage claim the unified reset is the billing period, not a limit.
        if header("anthropic-ratelimit-unified-representative-claim") == Some("overage")
            || header("anthropic-ratelimit-unified-overage-reset")
                .is_some_and(|v| v.trim().parse::<u64>().ok() == Some(reset))
        {
            return None;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        Some(Duration::from_secs(reset.saturating_sub(now)))
    };
    retry_after
        .or_else(unified_reset)
        .filter(|wait| !wait.is_zero())
        .unwrap_or(DEFAULT_COOLDOWN)
        .min(MAX_COOLDOWN)
}

/// Bench a credential for `wait`, keeping any later deadline already set.
pub(crate) fn start(credential_id: &str, wait: Duration) {
    let now = Instant::now();
    let Some(until) = now.checked_add(wait.min(MAX_COOLDOWN)) else {
        return;
    };
    let mut map = cooldowns();
    map.retain(|_, deadline| *deadline > now);
    let entry = map.entry(credential_id.to_owned()).or_insert(until);
    if *entry < until {
        *entry = until;
    }
    drop(map);
}

/// Time left on a credential's cooldown, or `None` when it may be used.
pub(crate) fn remaining(credential_id: &str) -> Option<Duration> {
    let now = Instant::now();
    let mut map = cooldowns();
    match map.get(credential_id) {
        Some(until) if *until > now => Some(until.saturating_duration_since(now)),
        Some(_) => {
            map.remove(credential_id);
            None
        }
        None => None,
    }
}

/// Lift a credential's cooldown after it served a request successfully.
pub(crate) fn clear(credential_id: &str) {
    cooldowns().remove(credential_id);
}

/// Whole seconds for a `Retry-After` header, rounded up so clients never retry early.
pub(crate) fn retry_after_secs(wait: Duration) -> u64 {
    wait.as_secs()
        .saturating_add(u64::from(wait.subsec_nanos() > 0))
}

/// Copy the headers a client needs to back off from an upstream rate limit:
/// `retry-after`, `x-should-retry`, and every `anthropic-ratelimit-*` value.
pub(crate) fn copy_rate_limit_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for (name, value) in from {
        let name_str = name.as_str();
        if name_str == "retry-after"
            || name_str == "x-should-retry"
            || name_str.starts_with("anthropic-ratelimit-")
        {
            to.append(name.clone(), value.clone());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn wait_prefers_retry_after_then_unified_reset_then_default() {
        // Real five-hour rejection: retry-after wins.
        let five_hour = headers(&[
            ("retry-after", "344".into()),
            (
                "anthropic-ratelimit-unified-representative-claim",
                "five_hour".into(),
            ),
            (
                "anthropic-ratelimit-unified-reset",
                (now() + 9000).to_string(),
            ),
        ]);
        assert_eq!(wait_for(&five_hour), Duration::from_secs(344));
        // No retry-after: the unified reset of the binding window.
        let reset_only = headers(&[
            (
                "anthropic-ratelimit-unified-representative-claim",
                "five_hour".into(),
            ),
            (
                "anthropic-ratelimit-unified-reset",
                (now() + 600).to_string(),
            ),
        ]);
        let wait = wait_for(&reset_only).as_secs();
        assert!((598..=600).contains(&wait), "{wait}");
        // Nothing usable: short default.
        assert_eq!(wait_for(&HeaderMap::new()), DEFAULT_COOLDOWN);
    }

    #[test]
    fn overage_billing_reset_never_benches_a_credential_for_weeks() {
        // CLIProxyAPI #6465: the overage claim's reset is the monthly billing boundary.
        let month = (now() + 24 * 86400).to_string();
        let overage = headers(&[
            (
                "anthropic-ratelimit-unified-representative-claim",
                "overage".into(),
            ),
            ("anthropic-ratelimit-unified-reset", month.clone()),
            ("anthropic-ratelimit-unified-overage-reset", month),
        ]);
        assert_eq!(wait_for(&overage), DEFAULT_COOLDOWN);
        // Any other huge hint is capped at the longest real window.
        let huge = headers(&[("retry-after", "2034192".into())]);
        assert_eq!(wait_for(&huge), MAX_COOLDOWN);
    }

    #[test]
    fn cooldown_tracks_deadline_and_clears() {
        let id = "cooldown-test-credential";
        assert!(remaining(id).is_none());
        start(id, Duration::from_secs(60));
        start(id, Duration::from_secs(5)); // a shorter hint never shortens it
        assert!(remaining(id).unwrap() > Duration::from_secs(55));
        clear(id);
        assert!(remaining(id).is_none());
        start(id, Duration::ZERO);
        assert!(remaining(id).is_none());
        assert_eq!(retry_after_secs(Duration::from_millis(1500)), 2);
        assert_eq!(retry_after_secs(Duration::from_secs(3)), 3);
    }
}

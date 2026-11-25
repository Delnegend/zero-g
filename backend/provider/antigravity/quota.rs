use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::quota::{QuotaGroup, QuotaSnapshot, QuotaWindow};
use crate::server::settings::SettingsStore;

/// A snapshot older than this is reported as stale instead of shown as current.
const STALE_AFTER_SECS: i64 = 3 * 60 * 60;

/// Setting that holds a credential's latest pushed snapshot.
pub(crate) fn setting_name(credential_id: &str) -> String {
    format!("agy_quota.{credential_id}")
}

/// What `agy --print /usage --output-format json` reports under `command.data.groups`.
#[derive(Deserialize)]
pub(crate) struct Report {
    /// Model pools that share their limits.
    groups: Vec<Group>,
}

/// One model pool.
#[derive(Deserialize)]
struct Group {
    /// Pool name.
    name: String,
    /// Limit windows of the pool.
    #[serde(default)]
    buckets: Vec<Bucket>,
}

/// One limit window; missing values stay unknown.
#[derive(Deserialize)]
struct Bucket {
    /// Stable bucket id.
    id: Option<String>,
    /// Display name, such as "Weekly Limit Remaining".
    name: Option<String>,
    /// Window length, such as `5h` or `weekly`.
    window: Option<String>,
    /// Fraction of the limit that is left.
    remaining_fraction: Option<f64>,
    /// Reset time in RFC 3339; the epoch means "not started".
    reset_time: Option<String>,
}

/// A stored snapshot and when it arrived.
#[derive(Serialize, Deserialize)]
struct Stored {
    /// Unix seconds at which the snapshot was received.
    updated_at: i64,
    /// Display-ready pools.
    groups: Vec<Pool>,
}

/// Display-ready pool.
#[derive(Serialize, Deserialize)]
struct Pool {
    /// Pool name.
    label: String,
    /// Windows, five-hour first.
    windows: Vec<Win>,
}

/// Display-ready window.
#[derive(Serialize, Deserialize)]
struct Win {
    /// Window label.
    label: String,
    /// Remaining percentage, if known.
    remaining_percent: Option<f64>,
    /// Reset time in Unix seconds, if one is running.
    reset_at: Option<i64>,
}

/// Human label for a window.
fn window_label(bucket: &Bucket) -> String {
    match bucket.window.as_deref() {
        Some("5h") => "5 hours".into(),
        Some("weekly") => "Weekly".into(),
        Some(other) if !other.is_empty() => other.to_owned(),
        _ => bucket
            .name
            .clone()
            .or_else(|| bucket.id.clone())
            .unwrap_or_else(|| "Quota".into()),
    }
}

/// Convert a report into stored pools; an empty report is an error.
pub(crate) fn convert(report: Report, now: i64) -> Result<String, String> {
    let groups: Vec<Pool> = report
        .groups
        .into_iter()
        .filter(|group| !group.buckets.is_empty())
        .map(|group| {
            let mut windows: Vec<Win> = group
                .buckets
                .iter()
                .map(|bucket| Win {
                    label: window_label(bucket),
                    remaining_percent: bucket
                        .remaining_fraction
                        .filter(|fraction| fraction.is_finite())
                        .map(|fraction| fraction.clamp(0.0, 1.0) * 100.0),
                    reset_at: bucket
                        .reset_time
                        .as_deref()
                        .and_then(|time| {
                            time::OffsetDateTime::parse(
                                time,
                                &time::format_description::well_known::Rfc3339,
                            )
                            .ok()
                        })
                        .map(time::OffsetDateTime::unix_timestamp)
                        .filter(|reset| *reset > 0),
                })
                .collect();
            windows.sort_by_key(|window| window.label != "5 hours");
            Pool {
                label: group.name,
                windows,
            }
        })
        .collect();
    if groups.is_empty() {
        return Err("The report contains no quota windows".into());
    }
    serde_json::to_string(&Stored {
        updated_at: now,
        groups,
    })
    .map_err(|e| e.to_string())
}

/// Read a credential's latest pushed snapshot for the dashboard.
pub(crate) async fn stored(
    store: &SettingsStore,
    credential: &str,
) -> Result<QuotaSnapshot, String> {
    let raw = store.setting_get(&setting_name(credential)).await?.ok_or(
        "No quota has been reported yet; run scripts/agy-quota.py on the machine with agy",
    )?;
    let snapshot: Stored =
        serde_json::from_str(&raw).map_err(|_| "Stored quota snapshot is invalid".to_string())?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let age = now.saturating_sub(snapshot.updated_at);
    if age > STALE_AFTER_SECS {
        return Err(format!(
            "Quota was last reported {} hours ago; check that scripts/agy-quota.py is still scheduled",
            age / 3600
        ));
    }
    Ok(QuotaSnapshot::GroupedQuota {
        groups: snapshot
            .groups
            .into_iter()
            .map(|pool| QuotaGroup {
                label: pool.label,
                windows: pool
                    .windows
                    .into_iter()
                    .map(|window| QuotaWindow {
                        label: window.label,
                        remaining_percent: window.remaining_percent,
                        reset_at: window.reset_at,
                    })
                    .collect(),
            })
            .collect(),
    })
}

/// Parse the `/usage` command data pushed by the script.
pub(crate) fn parse(value: Value) -> Result<Report, String> {
    serde_json::from_value(value).map_err(|_| "Invalid quota report".to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_the_cli_report_keeping_pools_and_hiding_unstarted_resets() {
        let report = parse(json!({"groups": [
            {"name": "Gemini Models", "buckets": [
                {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.95,
                 "reset_time": "2026-12-01T13:31:25Z"},
                {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.99,
                 "reset_time": "2026-11-25T10:30:13Z"}]},
            {"name": "Claude and GPT models", "buckets": [
                {"id": "3p-5h", "window": "5h", "remaining_fraction": 1.0,
                 "reset_time": "1970-01-01T00:00:00Z"}]},
            {"name": "Empty", "buckets": []}
        ]}))
        .unwrap();
        let stored: Stored = serde_json::from_str(&convert(report, 100).unwrap()).unwrap();
        assert_eq!(stored.groups.len(), 2);
        assert_eq!(stored.groups[0].windows[0].label, "5 hours");
        assert_eq!(stored.groups[0].windows[1].label, "Weekly");
        assert_eq!(stored.groups[0].windows[1].remaining_percent, Some(95.0));
        assert_eq!(stored.groups[1].windows[0].reset_at, None);
    }

    #[test]
    fn rejects_empty_or_malformed_reports() {
        assert!(parse(json!({"nope": 1})).is_err());
        let empty = parse(json!({"groups": []})).unwrap();
        assert!(convert(empty, 1).is_err());
    }
}

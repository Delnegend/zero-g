use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::auth::{CODEX_BASE_URL, CODEX_CLIENT_VERSION, CodexAuthManager};
use crate::provider::quota::{QuotaSnapshot, QuotaWindow};
use crate::server::settings::SettingsStore;

/// Fetches and parses usage windows for a stored Codex credential.
pub(crate) async fn fetch(
    store: &SettingsStore,
    provider_id: &str,
    credential_id: &str,
) -> Result<QuotaSnapshot, String> {
    let auth = CodexAuthManager::load(store.clone(), provider_id, credential_id)
        .await?
        .ok_or_else(|| "OpenAI Codex is not connected".to_string())?;
    // Refresh errors can contain upstream response bodies; never return them to the caller.
    let creds = auth
        .get_credentials()
        .await
        .map_err(|_| "Unable to load OpenAI Codex credentials".to_string())?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .user_agent(format!("codex_cli_rs/{CODEX_CLIENT_VERSION}"))
        .build()
        .map_err(|_| "Unable to initialize Codex usage client".to_string())?;
    let mut request = client
        .get(format!("{CODEX_BASE_URL}/wham/usage"))
        .bearer_auth(&creds.access_token);
    if let Some(account_id) = &creds.account_id {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Codex usage request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Codex usage request failed with status {}",
            response.status().as_u16()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|_| "Invalid Codex usage response".to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or_else(|| "System clock is unavailable".to_string())?;
    parse_usage(&payload, now)
}

/// Extracts primary and additional usage windows from a Codex usage payload.
fn parse_usage(payload: &Value, now: i64) -> Result<QuotaSnapshot, String> {
    let mut windows = Vec::new();
    if let Some(rate_limit) = payload.get("rate_limit") {
        append_windows(&mut windows, rate_limit, None, now);
    }
    if let Some(additional) = payload
        .get("additional_rate_limits")
        .and_then(Value::as_array)
    {
        for extra in additional {
            let name = extra
                .get("limit_name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty());
            let feature = extra
                .get("metered_feature")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty());
            let spark = name.into_iter().chain(feature).any(|s| {
                let lower = s.to_ascii_lowercase();
                lower.contains("spark") || lower.contains("bengalfox")
            });
            let display = name
                .or(if spark { Some("Spark") } else { feature })
                .unwrap_or("Additional limit");
            if let Some(rate_limit) = extra.get("rate_limit") {
                append_windows(&mut windows, rate_limit, Some(display), now);
            }
        }
    }
    if windows.is_empty() {
        return Err("Codex usage response contains no usage windows".to_string());
    }
    Ok(QuotaSnapshot::Quota { windows })
}

/// Appends valid primary and secondary windows, deriving labels and reset times.
fn append_windows(
    windows: &mut Vec<QuotaWindow>,
    rate_limit: &Value,
    name: Option<&str>,
    now: i64,
) {
    for (key, fallback) in [
        ("primary_window", "Primary window"),
        ("secondary_window", "Secondary window"),
    ] {
        let Some(window) = rate_limit.get(key).filter(|v| v.is_object()) else {
            continue;
        };
        let used = window
            .get("used_percent")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite());
        let duration = window
            .get("limit_window_seconds")
            .and_then(Value::as_u64)
            .filter(|&s| s > 0);
        let reset_at = window
            .get("reset_at")
            .and_then(Value::as_i64)
            .filter(|&v| v > 0)
            .map(|v| if v > 1_000_000_000_000 { v / 1000 } else { v })
            .or_else(|| {
                window
                    .get("reset_after_seconds")
                    .and_then(Value::as_i64)
                    .filter(|&v| v >= 0)
                    .and_then(|v| now.checked_add(v))
            });
        let label = duration.map_or_else(|| fallback.to_string(), format_duration);
        let label = match name {
            Some(name) => format!("{label} ({name})"),
            None => label,
        };
        windows.push(QuotaWindow {
            label,
            remaining_percent: used.map(|value| 100.0 - value.clamp(0.0, 100.0)),
            reset_at,
        });
    }
}

/// Formats a duration using the largest evenly divisible supported unit.
fn format_duration(seconds: u64) -> String {
    for (unit, size) in [("day", 86_400), ("hour", 3_600), ("minute", 60)] {
        if seconds >= size
            && seconds.is_multiple_of(size)
            && let Some(count) = seconds.checked_div(size)
        {
            return format!("{count} {unit}{}", if count == 1 { "" } else { "s" });
        }
    }
    format!("{seconds} second{}", if seconds == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn windows(payload: &Value) -> Vec<QuotaWindow> {
        match parse_usage(payload, 1_700_000_000).unwrap() {
            QuotaSnapshot::Quota { windows } => windows,
            _ => panic!("expected quota"),
        }
    }

    #[test]
    fn parses_primary_secondary_and_additional_limits() {
        let result = windows(&json!({
            "rate_limit": {
                "primary_window": {"used_percent": 12.5, "limit_window_seconds": 18_000, "reset_at": 1_700_001_000, "reset_after_seconds": 50},
                "secondary_window": {"used_percent": 100, "limit_window_seconds": 604_800, "reset_after_seconds": 900}
            },
            "additional_rate_limits": [{"limit_name": "Spark", "metered_feature": "codex_bengalfox", "rate_limit": {
                "primary_window": {"used_percent": 30, "limit_window_seconds": 3600, "reset_at": 1_700_002_000_000_i64},
                "secondary_window": null
            }}]
        }));
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].label, "5 hours");
        assert_eq!(result[0].remaining_percent, Some(87.5));
        assert_eq!(result[0].reset_at, Some(1_700_001_000));
        assert_eq!(result[1].label, "7 days");
        assert_eq!(result[1].remaining_percent, Some(0.0));
        assert_eq!(result[1].reset_at, Some(1_700_000_900));
        assert_eq!(result[2].label, "1 hour (Spark)");
        assert_eq!(result[2].reset_at, Some(1_700_002_000));
    }

    #[test]
    fn missing_and_malformed_values_are_unknown_and_percent_is_clamped() {
        let result = windows(&json!({"rate_limit": {
            "primary_window": {"used_percent": "bad", "reset_at": -1, "reset_after_seconds": -5},
            "secondary_window": {"used_percent": 120, "limit_window_seconds": 90}
        }, "additional_rate_limits": [{"metered_feature": "codex_bengalfox", "rate_limit": {
            "primary_window": {"used_percent": -20}
        }}]}));
        assert_eq!(result[0].remaining_percent, None);
        assert_eq!(result[0].reset_at, None);
        assert_eq!(result[0].label, "Primary window");
        assert_eq!(result[1].remaining_percent, Some(0.0));
        assert_eq!(result[1].label, "90 seconds");
        assert_eq!(result[2].remaining_percent, Some(100.0));
        assert_eq!(result[2].label, "Primary window (Spark)");
    }

    #[test]
    fn rejects_missing_windows_and_accepts_additional_only() {
        assert!(
            parse_usage(
                &json!({"rate_limit": {"allowed": true}, "additional_rate_limits": null}),
                0
            )
            .is_err()
        );
        let result = windows(
            &json!({"additional_rate_limits": [{"limit_name": "Custom model", "rate_limit": {
                "secondary_window": {"used_percent": 42}
            }}]}),
        );
        assert_eq!(result[0].label, "Secondary window (Custom model)");
        assert_eq!(result[0].remaining_percent, Some(58.0));
    }
}

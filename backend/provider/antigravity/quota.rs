use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::auth::{ANTIGRAVITY_USER_AGENT, AuthManager, CLOUD_CODE_ASSIST_ENDPOINT};
use super::models::SANDBOX_ENDPOINT;
use crate::provider::quota::{QuotaSnapshot, QuotaWindow};
use crate::server::settings::SettingsStore;

/// Fetch and parse quota information for an Antigravity credential.
pub(crate) async fn fetch(
    store: &SettingsStore,
    provider_id: &str,
    credential_id: &str,
) -> Result<QuotaSnapshot, String> {
    let auth = AuthManager::load(store.clone(), provider_id, credential_id)
        .await
        .map_err(|_| "Failed to load Antigravity credentials".to_string())?
        .ok_or_else(|| "Antigravity credentials are unavailable".to_string())?;
    let credentials = auth
        .get_credentials()
        .await
        .map_err(|_| "Failed to obtain Antigravity credentials".to_string())?;
    if credentials.access_token.is_empty() || credentials.project_id.is_empty() {
        return Err("Antigravity credentials lack an access token or project ID".to_string());
    }
    if credentials.expires_at.is_some_and(|expiry| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .is_ok_and(|now| i64::try_from(now.as_millis()).is_ok_and(|now_ms| expiry <= now_ms))
    }) {
        return Err("Antigravity access token has expired".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Failed to create Antigravity usage client".to_string())?;
    let mut last_error = "Antigravity usage request failed".to_string();
    for endpoint in [CLOUD_CODE_ASSIST_ENDPOINT, SANDBOX_ENDPOINT] {
        let response = client
            .post(format!("{endpoint}/v1internal:fetchAvailableModels"))
            .bearer_auth(&credentials.access_token)
            .header(reqwest::header::USER_AGENT, ANTIGRAVITY_USER_AGENT)
            .json(&serde_json::json!({ "project": credentials.project_id }))
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                let data: Value = response
                    .json()
                    .await
                    .map_err(|_| "Invalid Antigravity usage response".to_string())?;
                return parse_response(&data);
            }
            Ok(response) => {
                let status = response.status();
                last_error = format!("Antigravity usage request failed (HTTP {status})");
                if !status.is_server_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
                    break;
                }
            }
            Err(_) => last_error = "Antigravity usage request failed".to_string(),
        }
    }
    Err(last_error)
}

/// One model quota counter before grouping into shared windows.
#[derive(Clone)]
struct Entry {
    /// Backend provider whose quota pool the model uses.
    provider: String,
    /// Quota tier, defaulting to `default` when unspecified.
    tier: String,
    /// Explicit or normalized window label, if supplied.
    window: Option<String>,
    /// Remaining quota as a percentage, when known.
    percent: Option<f64>,
    /// Reset time as Unix seconds, when supplied.
    reset: Option<i64>,
}

/// Returns a nonempty string property from a JSON value.
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Identifies the quota backend from provider metadata or model family.
fn provider(info: &Value, model: &Value, model_id: &str) -> String {
    for name in [
        text(info, "modelProvider"),
        text(info, "apiProvider"),
        text(model, "modelProvider"),
        text(model, "apiProvider"),
    ]
    .into_iter()
    .flatten()
    {
        match name {
            "MODEL_PROVIDER_GOOGLE" | "API_PROVIDER_GOOGLE_GEMINI" => return "Google".to_string(),
            "MODEL_PROVIDER_ANTHROPIC" | "API_PROVIDER_ANTHROPIC_VERTEX" => {
                return "Anthropic".to_string();
            }
            "MODEL_PROVIDER_OPENAI" | "API_PROVIDER_OPENAI_VERTEX" => return "OpenAI".to_string(),
            _ => {}
        }
    }
    // OMP also maps model families to their corresponding backend counters.
    let id = model_id.to_lowercase();
    if id.starts_with("gemini-") || id.starts_with("gemma-") {
        "Google".to_string()
    } else if id.starts_with("claude-") {
        "Anthropic".to_string()
    } else if id.starts_with("gpt-") || id.starts_with("openai/") {
        "OpenAI".to_string()
    } else {
        // An unknown backend is not evidence that different models share a pool.
        model_id.to_string()
    }
}

/// Normalizes recognizable daily and weekly window labels.
fn window_name(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.contains("week")
        || lower.contains("7d")
        || lower.contains("7_day")
        || lower.contains("7-day")
        || lower.contains("7 day")
    {
        "Weekly".to_string()
    } else if lower.contains("day") || lower.contains("daily") || lower.contains("24h") {
        "Daily".to_string()
    } else {
        name.to_string()
    }
}

/// Extracts quota counters from an object or array, applying inherited labels.
fn add_entries(
    entries: &mut Vec<Entry>,
    value: &Value,
    model: &Value,
    model_id: &str,
    tier: Option<&str>,
    window: Option<&str>,
) {
    if let Some(array) = value.as_array() {
        for item in array {
            add_entries(entries, item, model, model_id, tier, window);
        }
        return;
    }
    if !value.is_object() {
        return;
    }
    let provider = provider(value, model, model_id);
    let reset = text(value, "resetTime")
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|time| time.timestamp());
    let percent = value
        .get("remainingFraction")
        .and_then(Value::as_f64)
        .filter(|fraction| fraction.is_finite())
        .map(|fraction| fraction.clamp(0.0, 1.0) * 100.0)
        // OMP observed reset-only exhausted counters for Google/Gemini.
        .or_else(|| {
            (provider == "Google" && value.get("remainingFraction").is_none() && reset.is_some())
                .then_some(0.0)
        });
    entries.push(Entry {
        provider,
        tier: tier
            .or_else(|| text(value, "tier"))
            .unwrap_or("default")
            .to_string(),
        window: text(value, "windowLabel")
            .or_else(|| text(value, "windowId"))
            .or(window)
            .map(window_name),
        percent,
        reset,
    });
}

/// Collects all supported quota counter representations for a model.
fn model_entries(model_id: &str, model: &Value) -> Vec<Entry> {
    let mut entries = Vec::new();
    for (field, window) in [
        ("quotaInfo", None),
        ("quotaInfos", None),
        ("dailyQuotaInfo", Some("Daily")),
        ("dailyQuotaInfos", Some("Daily")),
        ("weeklyQuotaInfo", Some("Weekly")),
        ("weeklyQuotaInfos", Some("Weekly")),
    ] {
        if let Some(value) = model.get(field) {
            add_entries(&mut entries, value, model, model_id, None, window);
        }
    }
    for (field, is_tier) in [
        ("quotaInfoByTier", true),
        ("quotaInfoByWindow", false),
        ("quotaInfosByWindow", false),
    ] {
        if let Some(map) = model.get(field).and_then(Value::as_object) {
            for (key, value) in map {
                add_entries(
                    &mut entries,
                    value,
                    model,
                    model_id,
                    is_tier.then_some(key),
                    (!is_tier).then_some(key),
                );
            }
        }
    }
    entries
}

/// Groups model counters into a quota snapshot by provider, tier, and window.
fn parse_response(data: &Value) -> Result<QuotaSnapshot, String> {
    let models = data
        .get("models")
        .and_then(Value::as_object)
        .ok_or_else(|| "Invalid Antigravity usage response".to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0);
    let mut grouped: BTreeMap<(String, String, String), QuotaWindow> = BTreeMap::new();
    for (model_id, model) in models {
        let entries = model_entries(model_id, model);
        for entry in &entries {
            let latest = entries
                .iter()
                .filter(|other| {
                    other.window.is_none()
                        && other.provider == entry.provider
                        && other.tier.eq_ignore_ascii_case(&entry.tier)
                })
                .filter_map(|other| other.reset)
                .max();
            let earliest = entries
                .iter()
                .filter(|other| {
                    other.window.is_none()
                        && other.provider == entry.provider
                        && other.tier.eq_ignore_ascii_case(&entry.tier)
                })
                .filter_map(|other| other.reset)
                .min();
            let window = entry.window.clone().unwrap_or_else(|| {
                if entry.reset.is_some_and(|reset| {
                    (latest != earliest && Some(reset) == latest)
                        || reset.saturating_sub(now) > 86_400
                }) {
                    "Weekly".to_string()
                } else {
                    "Daily".to_string()
                }
            });
            let key = (
                entry.provider.clone(),
                entry.tier.to_lowercase(),
                window.to_lowercase(),
            );
            let label = if entry.tier.eq_ignore_ascii_case("default") {
                format!("{} · {window}", entry.provider)
            } else {
                format!("{} · {} · {window}", entry.provider, entry.tier)
            };
            grouped
                .entry(key)
                .and_modify(
                    |existing| match (existing.remaining_percent, entry.percent) {
                        (Some(a), Some(b)) if b < a => {
                            existing.remaining_percent = Some(b);
                            existing.reset_at = entry.reset.or(existing.reset_at);
                        }
                        (None, Some(b)) => {
                            existing.remaining_percent = Some(b);
                            existing.reset_at = entry.reset.or(existing.reset_at);
                        }
                        _ => {
                            if existing.reset_at.is_none() {
                                existing.reset_at = entry.reset;
                            }
                        }
                    },
                )
                .or_insert(QuotaWindow {
                    label,
                    remaining_percent: entry.percent,
                    reset_at: entry.reset,
                });
        }
    }
    if grouped.is_empty() {
        return Err("Antigravity returned no quota information".to_string());
    }
    Ok(QuotaSnapshot::Quota {
        windows: grouped.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn groups_backend_counters_and_preserves_exhausted_google() {
        let data = json!({"models": {
            "gemini-a": {"modelProvider":"MODEL_PROVIDER_GOOGLE", "quotaInfo":{"resetTime":"2030-01-01T00:00:00Z"}},
            "gemini-b": {"apiProvider":"API_PROVIDER_GOOGLE_GEMINI", "quotaInfo":{"remainingFraction":0.8, "resetTime":"2030-01-01T00:00:00Z"}},
            "claude": {"modelProvider":"MODEL_PROVIDER_ANTHROPIC", "quotaInfo":{"remainingFraction":0.5}},
            "gpt": {"apiProvider":"API_PROVIDER_OPENAI_VERTEX", "quotaInfo":{"remainingFraction":0.3}}
        }});
        let QuotaSnapshot::Quota { windows } = parse_response(&data).unwrap() else {
            panic!("expected quota")
        };
        assert_eq!(windows.len(), 3);
        assert_eq!(
            windows
                .iter()
                .find(|w| w.label.starts_with("Google"))
                .unwrap()
                .remaining_percent,
            Some(0.0)
        );
        assert_eq!(
            windows
                .iter()
                .find(|w| w.label.starts_with("OpenAI"))
                .unwrap()
                .remaining_percent,
            Some(30.0)
        );
    }

    #[test]
    fn parses_tier_and_window_maps_and_unknown_fractions() {
        let data = json!({"models": {
            "claude": {"modelProvider":"MODEL_PROVIDER_ANTHROPIC",
                "quotaInfoByTier":{"pro":[{"windowId":"WINDOW_DAILY", "remainingFraction":0.4}, {"windowId":"WINDOW_WEEKLY", "resetTime":"2030-01-01T00:00:00Z"}]},
                "quotaInfoByWindow":{"daily":{"tier":"pro", "remainingFraction":0.2}}},
            "mystery-a":{"quotaInfo":{"remainingFraction":0.7}},
            "mystery-b":{"quotaInfo":{"remainingFraction":0.6}}
        }});
        let QuotaSnapshot::Quota { windows } = parse_response(&data).unwrap() else {
            panic!("expected quota")
        };
        assert_eq!(windows.len(), 4);
        assert_eq!(
            windows
                .iter()
                .find(|w| w.label == "Anthropic · pro · Daily")
                .unwrap()
                .remaining_percent,
            Some(20.0)
        );
        let weekly = windows
            .iter()
            .find(|w| w.label == "Anthropic · pro · Weekly")
            .unwrap();
        assert_eq!(weekly.remaining_percent, None);
        assert_eq!(weekly.reset_at, Some(1_893_456_000));
    }
}

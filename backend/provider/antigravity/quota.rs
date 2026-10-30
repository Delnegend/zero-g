use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::Value;

use super::auth::{ANTIGRAVITY_USER_AGENT, AuthManager, CLOUD_CODE_ASSIST_ENDPOINT};
use super::models::SANDBOX_ENDPOINT;
use crate::provider::quota::{QuotaSnapshot, QuotaWindow};
use crate::server::settings::SettingsStore;

/// Fetch quota information for an Antigravity credential.
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
    fetch_usage(
        &client,
        &[CLOUD_CODE_ASSIST_ENDPOINT, SANDBOX_ENDPOINT],
        &credentials.access_token,
        &credentials.project_id,
    )
    .await
}

/// Fetch the native quota summary without substituting model counters.
async fn fetch_usage(
    client: &reqwest::Client,
    endpoints: &[&str],
    token: &str,
    project: &str,
) -> Result<QuotaSnapshot, String> {
    let mut last_error = "Antigravity usage request failed".to_string();
    for endpoint in endpoints {
        let response = client
            .post(format!("{endpoint}/v1internal:retrieveUserQuotaSummary"))
            .bearer_auth(token)
            .header(reqwest::header::USER_AGENT, ANTIGRAVITY_USER_AGENT)
            .json(&serde_json::json!({ "project": project }))
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                let data = response
                    .json::<Value>()
                    .await
                    .map_err(|_| "Invalid Antigravity quota summary".to_string())?;
                return parse_summary(data);
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

/// Native quota summary, which can contain grouped or top-level buckets.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    /// Groups reported by the upstream account.
    #[serde(default)]
    groups: Vec<Group>,
    /// Ungrouped buckets, when the upstream uses the flat response format.
    #[serde(default)]
    buckets: Vec<Bucket>,
}

/// A named group of quota buckets.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Group {
    /// Name of the shared model group.
    display_name: Option<String>,
    /// Quota windows within this group.
    #[serde(default)]
    buckets: Vec<Bucket>,
}

/// One native quota bucket; missing fractions remain unknown.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bucket {
    /// Stable upstream identifier for this bucket.
    #[serde(rename = "bucketId")]
    id: Option<String>,
    /// Upstream display label for the window.
    display_name: Option<String>,
    /// Upstream window identifier.
    window: Option<String>,
    /// Fraction of quota remaining.
    remaining_fraction: Option<f64>,
    /// Reset time in RFC 3339 format.
    reset_time: Option<String>,
    /// Whether the upstream disabled this bucket.
    #[serde(default)]
    disabled: bool,
}

/// Convert a timestamp without inventing a reset time.
fn reset_time(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|time| time.timestamp())
}

/// Normalize recognizable window names without inferring them from reset times.
fn window_name(name: &str) -> &str {
    let lower = name.to_lowercase();
    if lower.contains("week")
        || lower.contains("7d")
        || lower.contains("7_day")
        || lower.contains("7-day")
        || lower.contains("7 day")
    {
        "Weekly"
    } else if lower.contains("5h")
        || lower.contains("five hour")
        || lower.contains("5 hour")
        || lower.contains("5-hour")
        || lower.contains("5_hour")
    {
        "5 hours"
    } else if lower.contains("day") || lower.contains("24h") {
        "Daily"
    } else {
        name
    }
}

/// Return the upstream window label, normalized when its duration is recognizable.
fn bucket_window(bucket: &Bucket) -> &str {
    bucket
        .window
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| bucket.display_name.as_deref().filter(|s| !s.is_empty()))
        .or_else(|| bucket.id.as_deref().filter(|s| !s.is_empty()))
        .map_or("Quota", window_name)
}

/// Put the five-hour window first without changing the order of other windows.
fn window_order(bucket: &Bucket) -> u8 {
    u8::from(bucket_window(bucket) != "5 hours")
}

/// Convert a fraction into a percentage while preserving unknown values.
fn percent(value: Option<f64>) -> Option<f64> {
    value
        .filter(|fraction| fraction.is_finite())
        .map(|fraction| fraction.clamp(0.0, 1.0) * 100.0)
}

/// Read each upstream bucket separately, keeping shared groups shared.
fn parse_summary(data: Value) -> Result<QuotaSnapshot, String> {
    let mut summary: Summary = serde_json::from_value(data)
        .map_err(|_| "Invalid Antigravity quota summary".to_string())?;
    for group in &mut summary.groups {
        group.buckets.sort_by_key(window_order);
    }
    summary.buckets.sort_by_key(window_order);
    let mut windows = Vec::new();
    let grouped = summary.groups.iter().any(|group| !group.buckets.is_empty());
    let buckets: Vec<(Option<&str>, &Bucket)> = if grouped {
        summary
            .groups
            .iter()
            .flat_map(|group| {
                group
                    .buckets
                    .iter()
                    .map(move |bucket| (group.display_name.as_deref(), bucket))
            })
            .collect()
    } else {
        summary
            .buckets
            .iter()
            .map(|bucket| (None, bucket))
            .collect()
    };
    for (group, bucket) in buckets {
        if bucket.disabled {
            continue;
        }
        let bucket_id = bucket.id.as_deref().filter(|s| !s.is_empty());
        let group =
            if bucket_id.is_some_and(|id| id.to_ascii_lowercase().starts_with("gemini-argon-")) {
                "Gemini (Argon)"
            } else {
                group.filter(|s| !s.is_empty()).unwrap_or("Antigravity")
            };
        let window = bucket_window(bucket);
        let mut label = format!("{group} · {window}");
        if windows
            .iter()
            .any(|existing: &QuotaWindow| existing.label == label)
            && let Some(id) = bucket_id
        {
            label = format!("{label} · {id}");
        }
        windows.push(QuotaWindow {
            label,
            remaining_percent: percent(bucket.remaining_fraction),
            reset_at: reset_time(bucket.reset_time.as_deref()),
        });
    }
    if windows.is_empty() {
        return Err("Antigravity returned no quota information".to_string());
    }
    Ok(QuotaSnapshot::Quota { windows })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::post};
    use serde_json::json;

    /// Flat summaries retain their upstream labels and do not invent missing percentages.
    #[test]
    fn flat_summary_preserves_unknown_quota() {
        let QuotaSnapshot::Quota { windows } = parse_summary(json!({
            "buckets": [{"bucketId":"third-party", "displayName":"Five Hour Limit Remaining", "resetTime":"2030-01-01T00:00:00Z"}]
        })).unwrap() else { panic!("expected quota"); };
        assert_eq!(windows[0].label, "Antigravity · 5 hours");
        assert_eq!(windows[0].remaining_percent, None);
        assert_eq!(windows[0].reset_at, Some(1_893_456_000));
    }

    /// The native summary preserves the shared pool and distinct five-hour and weekly limits.
    #[tokio::test]
    async fn summary_reports_upstream_groups_and_windows() {
        let app = Router::new().route(
            "/v1internal:retrieveUserQuotaSummary",
            post(|| async {
                Json(json!({"groups":[
                    {"displayName":"Gemini Models","buckets":[
                        {"bucketId":"gemini-weekly","window":"weekly","remainingFraction":0.4},
                        {"bucketId":"gemini-5h","window":"5h","remainingFraction":0.8,"resetTime":"2030-01-01T00:00:00Z"},
                        {"bucketId":"gemini-argon-weekly","window":"weekly","remainingFraction":0.6}
                    ]},
                    {"displayName":"Claude and GPT models","buckets":[
                        {"bucketId":"3p-weekly","window":"weekly","remainingFraction":0.2},
                        {"bucketId":"3p-5h","window":"5h","remainingFraction":0.3},
                        {"bucketId":"disabled","window":"weekly","disabled":true}
                    ]}
                ]}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let QuotaSnapshot::Quota { windows } =
            fetch_usage(&reqwest::Client::new(), &[&base], "token", "project")
                .await
                .unwrap()
        else {
            panic!("expected quota");
        };
        assert_eq!(windows.len(), 5);
        assert_eq!(
            windows.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(),
            [
                "Gemini Models · 5 hours",
                "Gemini Models · Weekly",
                "Gemini (Argon) · Weekly",
                "Claude and GPT models · 5 hours",
                "Claude and GPT models · Weekly",
            ]
        );
        let window = windows
            .iter()
            .find(|w| w.label == "Gemini Models · 5 hours")
            .unwrap();
        assert_eq!(window.remaining_percent, Some(80.0));
        assert_eq!(window.reset_at, Some(1_893_456_000));
        assert!(
            windows
                .iter()
                .any(|w| w.label == "Gemini (Argon) · Weekly" && w.remaining_percent == Some(60.0))
        );
        assert!(
            windows
                .iter()
                .any(|w| w.label == "Claude and GPT models · 5 hours"
                    && w.remaining_percent == Some(30.0))
        );
        assert!(
            windows
                .iter()
                .any(|w| w.label == "Claude and GPT models · Weekly"
                    && w.remaining_percent == Some(20.0))
        );
        server.abort();
    }

    /// An unavailable summary is an error even if model counters are available.
    #[tokio::test]
    async fn unavailable_summary_is_an_error() {
        let app = Router::new()
            .route(
                "/v1internal:retrieveUserQuotaSummary",
                post(|| async { StatusCode::NOT_FOUND }),
            )
            .route(
                "/v1internal:fetchAvailableModels",
                post(|| async {
                    Json(json!({"models":{"gemini":{"quotaInfo":{"remainingFraction":0.8}}}}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let error = fetch_usage(&reqwest::Client::new(), &[&base], "token", "project")
            .await
            .unwrap_err();
        assert!(error.contains("404"));
        server.abort();
    }
}

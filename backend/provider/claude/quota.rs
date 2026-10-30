use serde::Deserialize;

use super::{auth, client};
use crate::provider::quota::{QuotaGroup, QuotaSnapshot, QuotaWindow};
use crate::server::settings::SettingsStore;

/// Claude Code's undocumented subscription usage endpoint.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Claude subscription quota buckets returned by the usage endpoint.
#[derive(Deserialize)]
struct Usage {
    /// Shared five-hour limit, if provided.
    five_hour: Option<Window>,
    /// Shared seven-day limit, if provided.
    seven_day: Option<Window>,
    /// Seven-day Opus limit, if applicable.
    seven_day_opus: Option<Window>,
    /// Seven-day Sonnet limit, if applicable.
    seven_day_sonnet: Option<Window>,
    /// Seven-day OAuth application limit, if applicable.
    seven_day_oauth_apps: Option<Window>,
    /// Optional extra usage spending limit.
    #[serde(rename = "extra_usage")]
    extra: Option<ExtraUsage>,
}

/// A percentage already consumed and its optional reset timestamp.
#[derive(Deserialize)]
struct Window {
    /// Usage already consumed, as a percentage.
    utilization: Option<f64>,
    /// RFC 3339 reset timestamp.
    resets_at: Option<String>,
}

/// Optional monthly usage beyond the subscription.
#[derive(Deserialize)]
struct ExtraUsage {
    /// Whether extra usage is enabled for the account.
    is_enabled: bool,
    /// Percentage of the configured spending limit consumed.
    utilization: Option<f64>,
}

/// Fetch subscription usage using the stored, refreshable OAuth credential.
pub(super) async fn fetch(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
) -> Result<QuotaSnapshot, String> {
    let credentials = auth::load(store, provider, credential)
        .await
        .map_err(|_| "Unable to load Claude credentials".to_string())?;
    let client = client::http()?;
    fetch_usage(&client, USAGE_URL, &credentials.access_token).await
}

/// Fetch the native quota summary without substituting estimates on errors.
async fn fetch_usage(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<QuotaSnapshot, String> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            format!(
                "claude-cli/{} (external, cli)",
                super::payload::CLAUDE_VERSION
            ),
        )
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| "Claude usage request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Claude usage request failed (HTTP {})",
            response.status()
        ));
    }
    let usage: Usage = response
        .json()
        .await
        .map_err(|_| "Invalid Claude usage response".to_string())?;
    parse_usage(usage)
}

/// Convert used percentages to the shared remaining-quota format.
fn parse_usage(usage: Usage) -> Result<QuotaSnapshot, String> {
    let mut windows = Vec::new();
    for (label, window) in [
        ("5 hours", usage.five_hour),
        ("Weekly", usage.seven_day),
        ("Weekly · Opus", usage.seven_day_opus),
        ("Weekly · Sonnet", usage.seven_day_sonnet),
        ("Weekly · OAuth apps", usage.seven_day_oauth_apps),
    ] {
        if let Some(window) = window {
            let remaining_percent = remaining(window.utilization)?;
            let reset_at = window
                .resets_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|time| time.timestamp());
            windows.push(QuotaWindow {
                label: label.to_string(),
                remaining_percent,
                reset_at,
            });
        }
    }
    if let Some(extra) = usage.extra.filter(|extra| extra.is_enabled) {
        windows.push(QuotaWindow {
            label: "Extra usage · Monthly".to_string(),
            remaining_percent: remaining(extra.utilization)?,
            reset_at: None,
        });
    }
    if windows.is_empty() {
        return Err("Claude returned no subscription quota information".to_string());
    }
    Ok(QuotaSnapshot::GroupedQuota {
        groups: vec![QuotaGroup {
            label: "Claude subscription".to_string(),
            windows,
        }],
    })
}

/// Reject invalid usage instead of displaying a fabricated remaining percentage.
fn remaining(utilization: Option<f64>) -> Result<Option<f64>, String> {
    match utilization {
        None => Ok(None),
        Some(value) if value.is_finite() && (0.0..=100.0).contains(&value) => {
            Ok(Some(100.0 - value))
        }
        Some(_) => Err("Invalid Claude usage percentage".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::get};
    use serde_json::json;

    /// The dashboard gets remaining capacity and the account's actual quota windows.
    #[tokio::test]
    async fn usage_returns_real_windows() {
        let app = Router::new().route(
            "/usage",
            get(|| async {
                Json(json!({
                    "five_hour": {"utilization": 6.0, "resets_at": "2030-01-01T00:00:00Z"},
                    "seven_day": {"utilization": 35.0, "resets_at": null},
                    "seven_day_opus": {"utilization": 100.0, "resets_at": null},
                    "seven_day_sonnet": null,
                    "extra_usage": {"is_enabled": true, "utilization": 12.5}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/usage", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let QuotaSnapshot::GroupedQuota { groups } =
            fetch_usage(&reqwest::Client::new(), &base, "token")
                .await
                .unwrap()
        else {
            panic!("expected grouped quota")
        };
        let group = &groups[0];
        assert_eq!(group.label, "Claude subscription");
        assert_eq!(
            group
                .windows
                .iter()
                .map(|w| w.label.as_str())
                .collect::<Vec<_>>(),
            [
                "5 hours",
                "Weekly",
                "Weekly · Opus",
                "Extra usage · Monthly"
            ]
        );
        assert_eq!(group.windows[0].remaining_percent, Some(94.0));
        assert_eq!(group.windows[0].reset_at, Some(1_893_456_000));
        assert_eq!(group.windows[1].remaining_percent, Some(65.0));
        assert_eq!(group.windows[2].remaining_percent, Some(0.0));
        assert_eq!(group.windows[3].remaining_percent, Some(87.5));
        server.abort();
    }

    /// Invalid or unavailable subscription usage must not become a fabricated quota.
    #[tokio::test]
    async fn unavailable_or_invalid_usage_returns_an_error() {
        let app = Router::new()
            .route("/limited", get(|| async { StatusCode::TOO_MANY_REQUESTS }))
            .route(
                "/invalid",
                get(|| async { Json(json!({"five_hour":{"utilization":101}})) }),
            )
            .route(
                "/empty",
                get(|| async {
                    Json(json!({"five_hour":null,"extra_usage":{"is_enabled":false}}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (path, expected) in [
            ("limited", "429"),
            ("invalid", "Invalid Claude usage percentage"),
            ("empty", "no subscription quota"),
        ] {
            let error = fetch_usage(
                &reqwest::Client::new(),
                &format!("http://{addr}/{path}"),
                "token",
            )
            .await
            .unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
        server.abort();
    }
}

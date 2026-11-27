use serde::Deserialize;

use super::{auth, upstream};

/// Claude Code release whose user agent the undocumented usage endpoint expects.
pub(super) const CLAUDE_VERSION: &str = "2.1.280";
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
    /// Weekly limit for the model family Anthropic meters separately (Fable), under its older name.
    seven_day_overage_included: Option<Window>,
    /// Weekly limits per model family, in the shape Claude Code reads (`display_name`, `utilization`).
    ///
    /// Kept as raw JSON: these are undocumented and a malformed entry must not break the
    /// whole quota card.
    model_scoped: Option<serde_json::Value>,
    /// Generic limit list. A `weekly_scoped` entry names its model in `scope.model.display_name`.
    limits: Option<serde_json::Value>,
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
    fetch_usage(&upstream::HTTP_CLIENT, USAGE_URL, &credentials.access_token).await
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
            format!("claude-cli/{CLAUDE_VERSION} (external, cli)"),
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
    // Read before the fixed windows below take their fields out of `usage`.
    let scoped = scoped_weekly(&usage);
    let mut windows = Vec::new();
    for (label, window) in [
        ("5 hours", usage.five_hour),
        ("Weekly", usage.seven_day),
        ("Weekly · Opus", usage.seven_day_opus),
        ("Weekly · Sonnet", usage.seven_day_sonnet),
        ("Weekly · OAuth apps", usage.seven_day_oauth_apps),
    ] {
        if let Some(window) = window {
            windows.push(QuotaWindow {
                label: label.to_string(),
                remaining_percent: remaining(window.utilization)?,
                reset_at: window.resets_at.as_deref().and_then(parse_reset),
            });
        }
    }
    for scoped in scoped {
        let label = format!("Weekly · {}", scoped.name);
        // The same model can be reported in more than one shape; show it once.
        if windows
            .iter()
            .any(|window| window.label.eq_ignore_ascii_case(&label))
        {
            continue;
        }
        windows.push(QuotaWindow {
            label,
            remaining_percent: Some(100.0 - scoped.used_percent),
            reset_at: scoped.reset_at,
        });
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

/// Parse an RFC 3339 reset timestamp into Unix seconds.
fn parse_reset(value: &str) -> Option<i64> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(time::OffsetDateTime::unix_timestamp)
}

/// A weekly limit that applies to one model family, such as Fable.
struct ScopedWeekly {
    /// Model family name as Anthropic labels it.
    name: String,
    /// Percentage already consumed, within 0 to 100.
    used_percent: f64,
    /// Reset time in Unix seconds, when reported.
    reset_at: Option<i64>,
}

/// Collect per-model weekly limits from every shape Anthropic has used for them.
///
/// Fable has its own weekly cap. Anthropic reports it in `limits[]` as a `weekly_scoped`
/// entry, in `model_scoped[]`, and under the older `seven_day_overage_included` name. Earlier
/// sources win, and an entry that is malformed or outside 0 to 100 is skipped rather than shown
/// as a made-up number.
fn scoped_weekly(usage: &Usage) -> Vec<ScopedWeekly> {
    let mut found: Vec<ScopedWeekly> = Vec::new();
    let mut add = |name: Option<&str>, used: Option<f64>, reset: Option<&str>| {
        let (Some(name), Some(used_percent)) =
            (name.map(str::trim).filter(|n| !n.is_empty()), used)
        else {
            return;
        };
        if !used_percent.is_finite() || !(0.0..=100.0).contains(&used_percent) {
            return;
        }
        let reset_at = reset.and_then(parse_reset);
        match found
            .iter_mut()
            .find(|existing| existing.name.eq_ignore_ascii_case(name))
        {
            Some(existing) => {
                existing.reset_at = existing.reset_at.or(reset_at);
            }
            None => found.push(ScopedWeekly {
                name: name.to_owned(),
                used_percent,
                reset_at,
            }),
        }
    };
    let entries = |value: &Option<serde_json::Value>| {
        value
            .as_ref()
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    for entry in entries(&usage.limits) {
        if entry.get("kind").and_then(serde_json::Value::as_str) == Some("weekly_scoped") {
            add(
                entry
                    .pointer("/scope/model/display_name")
                    .and_then(serde_json::Value::as_str),
                entry.get("percent").and_then(serde_json::Value::as_f64),
                entry.get("resets_at").and_then(serde_json::Value::as_str),
            );
        }
    }
    for entry in entries(&usage.model_scoped) {
        add(
            entry
                .get("display_name")
                .and_then(serde_json::Value::as_str),
            entry.get("utilization").and_then(serde_json::Value::as_f64),
            entry.get("resets_at").and_then(serde_json::Value::as_str),
        );
    }
    if let Some(window) = &usage.seven_day_overage_included {
        add(
            Some("Fable"),
            window.utilization,
            window.resets_at.as_deref(),
        );
    }
    found
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

    /// Windows of a parsed usage body, as `(label, remaining, reset)`.
    fn parsed(body: serde_json::Value) -> Vec<(String, Option<f64>, Option<i64>)> {
        let QuotaSnapshot::GroupedQuota { groups } =
            parse_usage(serde_json::from_value(body).unwrap()).unwrap()
        else {
            panic!("expected grouped quota")
        };
        groups[0]
            .windows
            .iter()
            .map(|w| (w.label.clone(), w.remaining_percent, w.reset_at))
            .collect()
    }

    /// Fable has its own weekly cap, reported in `limits[]` since Anthropic retired the per-model fields.
    #[test]
    fn fable_weekly_limit_comes_from_the_limit_list() {
        let windows = parsed(json!({
            "five_hour": {"utilization": 6.0, "resets_at": null},
            "seven_day": {"utilization": 15.0, "resets_at": null},
            "seven_day_opus": null,
            "seven_day_sonnet": null,
            "limits": [
                {"kind": "session", "group": "session", "percent": 6},
                {"kind": "weekly_all", "group": "weekly", "percent": 15, "scope": null},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 74,
                 "resets_at": "2030-01-01T00:00:00Z", "is_active": false,
                 "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}}
            ]
        }));
        let labels: Vec<_> = windows.iter().map(|w| w.0.as_str()).collect();
        assert_eq!(labels, ["5 hours", "Weekly", "Weekly · Fable"]);
        assert_eq!(
            windows[2],
            ("Weekly · Fable".to_owned(), Some(26.0), Some(1_893_456_000))
        );
    }

    /// The same Fable cap in the older shapes is read too, and listed once.
    #[test]
    fn fable_weekly_limit_is_read_from_older_shapes_and_listed_once() {
        let model_scoped = parsed(json!({
            "seven_day": {"utilization": 15.0, "resets_at": null},
            "model_scoped": [{"display_name": "Fable", "utilization": 2}]
        }));
        assert_eq!(
            model_scoped[1],
            ("Weekly · Fable".to_owned(), Some(98.0), None)
        );
        let legacy = parsed(json!({
            "seven_day": {"utilization": 15.0, "resets_at": null},
            "seven_day_overage_included": {"utilization": 3.0, "resets_at": "2030-01-01T00:00:00Z"}
        }));
        assert_eq!(
            legacy[1],
            ("Weekly · Fable".to_owned(), Some(97.0), Some(1_893_456_000))
        );
        // All three at once: one row, the earliest source's value, a reset filled in from a later one.
        let all = parsed(json!({
            "seven_day": {"utilization": 15.0, "resets_at": null},
            "limits": [{"kind": "weekly_scoped", "percent": 74,
                        "scope": {"model": {"display_name": "Fable"}}}],
            "model_scoped": [{"display_name": "fable", "utilization": 50,
                              "resets_at": "2030-01-01T00:00:00Z"}],
            "seven_day_overage_included": {"utilization": 3.0, "resets_at": null}
        }));
        assert_eq!(all.len(), 2);
        assert_eq!(
            all[1],
            ("Weekly · Fable".to_owned(), Some(26.0), Some(1_893_456_000))
        );
    }

    /// A malformed or out-of-range scoped entry is skipped; it never breaks the card or shows a made-up number.
    #[test]
    fn malformed_scoped_limits_are_skipped() {
        let windows = parsed(json!({
            "seven_day": {"utilization": 15.0, "resets_at": null},
            "limits": [
                {"kind": "weekly_scoped", "percent": 150, "scope": {"model": {"display_name": "Fable"}}},
                {"kind": "weekly_scoped", "percent": "high", "scope": {"model": {"display_name": "Other"}}},
                {"kind": "weekly_scoped", "percent": 10, "scope": null},
                "garbage"
            ],
            "model_scoped": {"not": "a list"}
        }));
        assert_eq!(windows.len(), 1);
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

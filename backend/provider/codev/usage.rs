//! Usage recorded from `CoDev` gateway responses.
//!
//! The gateway exposes no usage endpoint, but every inference response carries
//! `LiteLLM` headers with the key's total spend and per-minute rate limits.
//! The latest values are kept per credential and shown as its quota.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::provider::quota::{QuotaGroup, QuotaSnapshot, QuotaWindow};
use crate::server::settings::SettingsStore;

/// Rate-limit windows are per minute.
const WINDOW_SECS: i64 = 60;

/// Minimum seconds between saves of one credential's usage to the database.
const SAVE_EVERY_SECS: i64 = 60;

/// Latest usage per credential, with the time it was last saved.
///
/// Every response updates memory; the database is written at most once a
/// minute per credential, so recording adds no write to each request.
static LATEST: LazyLock<Mutex<HashMap<String, (Usage, i64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Setting holding a credential's latest recorded usage.
pub(crate) fn setting_name(credential_id: &str) -> String {
    format!("codev_usage.{credential_id}")
}

/// One rate limit as last reported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Limit {
    /// Display label.
    label: String,
    /// Units left in the window.
    remaining: f64,
    /// Units allowed per window.
    allowed: f64,
    /// Unix seconds of the response that reported it.
    #[serde(default)]
    updated_at: i64,
}

/// A credential's latest usage.
///
/// Responses report different subsets (streamed ones omit the per-user
/// limits), so each limit keeps its own last value.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Usage {
    /// Total the key has spent, in the gateway's currency.
    spend: Option<f64>,
    /// Limits that apply to the account across models.
    account: Vec<Limit>,
    /// Limits per model, by model name.
    models: BTreeMap<String, Vec<Limit>>,
}

/// Replaces limits with the same label and appends new ones.
fn merge(into: &mut Vec<Limit>, reported: Vec<Limit>, now: i64) {
    for mut limit in reported {
        limit.updated_at = now;
        if let Some(existing) = into.iter_mut().find(|old| old.label == limit.label) {
            *existing = limit;
        } else {
            into.push(limit);
        }
    }
    into.sort_by(|a, b| a.label.cmp(&b.label));
}

/// What one response reported.
#[derive(Debug, Default, PartialEq)]
struct Report {
    /// Model the request was routed to.
    model: Option<String>,
    /// Total key spend.
    spend: Option<f64>,
    /// Account-wide limits.
    account: Vec<Limit>,
    /// Limits for the routed model.
    model_limits: Vec<Limit>,
}

/// Formats a whole number with thousands separators.
fn grouped(value: f64) -> String {
    let digits = format!("{:.0}", value.max(0.0));
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && digits.len().saturating_sub(index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Reads the usage headers of one gateway response.
fn parse(headers: &HeaderMap) -> Report {
    let number = |name: &str| -> Option<f64> {
        headers
            .get(name)?
            .to_str()
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
    };
    let mut report = Report {
        model: headers
            .get("x-litellm-model-group")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .filter(|model| !model.is_empty()),
        spend: number("x-litellm-key-spend"),
        ..Report::default()
    };
    // `x-ratelimit[-<scope>]-remaining-<unit>` pairs with `...-limit-<unit>`.
    for name in headers.keys().map(reqwest::header::HeaderName::as_str) {
        let Some(rest) = name.strip_prefix("x-ratelimit-") else {
            continue;
        };
        let (scope, unit) = if let Some(unit) = rest.strip_prefix("remaining-") {
            ("", unit)
        } else if let Some((scope, unit)) = rest.split_once("-remaining-") {
            (scope, unit)
        } else {
            continue;
        };
        let limit_name = if scope.is_empty() {
            format!("x-ratelimit-limit-{unit}")
        } else {
            format!("x-ratelimit-{scope}-limit-{unit}")
        };
        let (Some(remaining), Some(limit)) = (number(name), number(&limit_name)) else {
            continue;
        };
        if limit <= 0.0 {
            continue;
        }
        let unit_label = match unit {
            "requests" => "Requests",
            "tokens" => "Tokens",
            _ => continue,
        };
        let (who, account) = match scope {
            "team_member" => ("you", true),
            "model_per_key" => ("your key", false),
            "model_per_team" => ("your team", false),
            "" => ("all users", false),
            other => (other, false),
        };
        let entry = Limit {
            label: format!("{unit_label} per minute, {who} (limit {})", grouped(limit)),
            remaining,
            allowed: limit,
            updated_at: 0,
        };
        if account {
            report.account.push(entry);
        } else {
            report.model_limits.push(entry);
        }
    }
    report.account.sort_by(|a, b| a.label.cmp(&b.label));
    report.model_limits.sort_by(|a, b| a.label.cmp(&b.label));
    report
}

impl Usage {
    /// Merges one response's report, keeping limits it did not mention.
    fn apply(&mut self, report: Report, now: i64) {
        if report.spend.is_some() {
            self.spend = report.spend;
        }
        merge(&mut self.account, report.account, now);
        if let Some(model) = report.model
            && !report.model_limits.is_empty()
        {
            merge(
                self.models.entry(model).or_default(),
                report.model_limits,
                now,
            );
        }
    }

    /// Dashboard view. A limit's window is over a minute after it was reported.
    fn snapshot(&self) -> QuotaSnapshot {
        let windows = |limits: &[Limit]| {
            limits
                .iter()
                .map(|limit| QuotaWindow {
                    label: limit.label.clone(),
                    remaining_percent: Some(
                        (limit.remaining / limit.allowed * 100.0).clamp(0.0, 100.0),
                    ),
                    reset_at: limit.updated_at.checked_add(WINDOW_SECS),
                })
                .collect()
        };
        let account_label = self.spend.map_or_else(
            || "Account".to_owned(),
            |spend| format!("Account, ${spend:.2} spent so far"),
        );
        let mut groups = vec![QuotaGroup {
            label: account_label,
            windows: windows(&self.account),
        }];
        groups.extend(self.models.iter().map(|(model, limits)| QuotaGroup {
            label: model.clone(),
            windows: windows(limits),
        }));
        QuotaSnapshot::GroupedQuota { groups }
    }
}

/// Reads a credential's saved usage.
async fn load(store: &SettingsStore, credential_id: &str) -> Result<Option<Usage>, String> {
    store
        .setting_get(&setting_name(credential_id))
        .await?
        .map(|raw| {
            serde_json::from_str(&raw).map_err(|_| "Recorded CoDev usage is invalid".to_string())
        })
        .transpose()
}

/// Records the usage headers of a gateway response for a credential.
pub(super) async fn record(store: &SettingsStore, credential_id: &str, headers: &HeaderMap) {
    let report = parse(headers);
    if report.spend.is_none() && report.account.is_empty() && report.model_limits.is_empty() {
        return;
    }
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let json = {
        let mut latest = LATEST.lock().await;
        if !latest.contains_key(credential_id) {
            let saved = load(store, credential_id).await.ok().flatten();
            latest.insert(credential_id.to_owned(), (saved.unwrap_or_default(), 0));
        }
        let Some((usage, saved_at)) = latest.get_mut(credential_id) else {
            return;
        };
        usage.apply(report, now);
        if now.saturating_sub(*saved_at) < SAVE_EVERY_SECS {
            return;
        }
        *saved_at = now;
        let json = serde_json::to_string(usage);
        drop(latest);
        json
    };
    let Ok(json) = json else {
        return;
    };
    let name = setting_name(credential_id);
    let saved = crate::database::transaction_with_retry!(store.database, |mut tx| {
        crate::storage::Setting::upsert_by_name(name.as_str())
            .value(json.clone())
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(())
    });
    if let Err(error) = saved {
        tracing::warn!(credential = %credential_id, "Failed to save CoDev usage: {error}");
    }
}

/// Latest usage for a credential, from memory or the last save.
pub(super) async fn stored(
    store: &SettingsStore,
    credential_id: &str,
) -> Result<QuotaSnapshot, String> {
    if let Some((usage, _)) = LATEST.lock().await.get(credential_id) {
        return Ok(usage.snapshot());
    }
    load(store, credential_id)
        .await?
        .map(|usage| usage.snapshot())
        .ok_or_else(|| {
            "No usage recorded yet; it appears after this account's first CoDev request".into()
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    /// The headers seen on a real gateway response become account and model limits.
    #[test]
    fn parses_spend_and_scoped_limits() {
        let report = parse(&headers(&[
            ("x-litellm-model-group", "zai-org/GLM-5.3-Flash"),
            ("x-litellm-key-spend", "360.73757961999866"),
            ("x-litellm-response-cost", "4.45e-06"),
            ("x-ratelimit-remaining-tokens", "26229510"),
            ("x-ratelimit-limit-tokens", "30000000"),
            ("x-ratelimit-team_member-remaining-requests", "29"),
            ("x-ratelimit-team_member-limit-requests", "30"),
            ("x-ratelimit-team_member-remaining-tokens", "799994"),
            ("x-ratelimit-team_member-limit-tokens", "800000"),
            ("x-ratelimit-model_per_key-remaining-requests", "299"),
            ("x-ratelimit-model_per_key-limit-requests", "300"),
            ("x-ratelimit-model_per_team-remaining-requests", "226"),
            ("x-ratelimit-model_per_team-limit-requests", "300"),
        ]));
        assert_eq!(report.model.as_deref(), Some("zai-org/GLM-5.3-Flash"));
        assert_eq!(report.spend, Some(360.737_579_619_998_66));
        let labels = |limits: &[Limit]| limits.iter().map(|l| l.label.clone()).collect::<Vec<_>>();
        assert_eq!(
            labels(&report.account),
            [
                "Requests per minute, you (limit 30)",
                "Tokens per minute, you (limit 800,000)"
            ]
        );
        assert_eq!(
            labels(&report.model_limits),
            [
                "Requests per minute, your key (limit 300)",
                "Requests per minute, your team (limit 300)",
                "Tokens per minute, all users (limit 30,000,000)"
            ]
        );
    }

    /// A later response for another model keeps the first model's limits.
    #[test]
    fn merging_keeps_other_models_and_builds_groups() {
        let mut usage = Usage::default();
        usage.apply(
            parse(&headers(&[
                ("x-litellm-model-group", "a"),
                ("x-litellm-key-spend", "1.5"),
                ("x-ratelimit-team_member-remaining-requests", "15"),
                ("x-ratelimit-team_member-limit-requests", "30"),
                ("x-ratelimit-model_per_key-remaining-requests", "300"),
                ("x-ratelimit-model_per_key-limit-requests", "300"),
            ])),
            100,
        );
        usage.apply(
            parse(&headers(&[
                ("x-litellm-model-group", "b"),
                ("x-ratelimit-model_per_key-remaining-requests", "150"),
                ("x-ratelimit-model_per_key-limit-requests", "300"),
            ])),
            200,
        );
        let QuotaSnapshot::GroupedQuota { groups } = usage.snapshot() else {
            panic!("expected groups");
        };
        let names: Vec<_> = groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(names, ["Account, $1.50 spent so far", "a", "b"]);
        assert_eq!(groups[0].windows[0].remaining_percent, Some(50.0));
        assert_eq!(groups[0].windows[0].reset_at, Some(160));
        assert_eq!(groups[2].windows[0].remaining_percent, Some(50.0));
        assert_eq!(groups[2].windows[0].reset_at, Some(260));

        // A streamed response reports only the model-wide tokens; the rest stays.
        usage.apply(
            parse(&headers(&[
                ("x-litellm-model-group", "a"),
                ("x-ratelimit-remaining-tokens", "10"),
                ("x-ratelimit-limit-tokens", "40"),
            ])),
            300,
        );
        let QuotaSnapshot::GroupedQuota { groups } = usage.snapshot() else {
            panic!("expected groups");
        };
        assert_eq!(groups[0].windows.len(), 1);
        let a: Vec<_> = groups[1]
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.reset_at))
            .collect();
        assert_eq!(
            a,
            [
                ("Requests per minute, your key (limit 300)", Some(160)),
                ("Tokens per minute, all users (limit 40)", Some(360))
            ]
        );
    }

    /// Responses without usage headers leave nothing to record.
    #[test]
    fn plain_responses_report_nothing() {
        assert_eq!(
            parse(&headers(&[("content-type", "application/json")])),
            Report::default()
        );
    }
}

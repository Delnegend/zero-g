//! Claude account subscription plan, read from Claude Code's undocumented profile endpoint.
//!
//! The plan changes rarely, so it is fetched lazily, kept in a setting, and refreshed daily.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{auth, quota::CLAUDE_VERSION, upstream};
use crate::server::settings::SettingsStore;

/// Claude Code's undocumented account profile endpoint.
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// A stored plan older than this is read again.
const REFRESH_AFTER_SECS: i64 = 24 * 60 * 60;
/// A failed or empty attempt is not repeated for this long.
const RETRY_AFTER: Duration = Duration::from_mins(10);

/// Last time an upstream read was attempted per credential, successful or not.
static ATTEMPTS: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Mutex::default);

/// The stored result of a profile read.
#[derive(Serialize, Deserialize)]
struct Stored {
    /// Unix seconds of the read.
    checked_at: i64,
    /// Plan label, when the profile named one.
    plan: Option<String>,
}

/// Setting that holds a credential's stored plan.
fn setting_name(credential: &str) -> String {
    format!("claude_profile.{credential}")
}

/// Human plan label such as "Max 5x" from a profile response.
fn plan_label(profile: &Value) -> Option<String> {
    let text = |path: &str| {
        profile
            .pointer(path)
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    let flag = |path: &str| profile.pointer(path).and_then(Value::as_bool) == Some(true);
    let kind = text("/organization/organization_type");
    let base = match kind.as_deref() {
        Some("claude_max") => "Max",
        Some("claude_pro") => "Pro",
        Some("claude_team") => "Team",
        Some("claude_enterprise") => "Enterprise",
        _ if flag("/account/has_claude_max") => "Max",
        _ if flag("/account/has_claude_pro") => "Pro",
        _ => return None,
    };
    let tier = text("/organization/rate_limit_tier").unwrap_or_default();
    let multiplier = (base == "Max")
        .then(|| {
            ["5x", "20x"]
                .into_iter()
                .find(|m| tier.ends_with(&format!("_{m}")))
        })
        .flatten();
    Some(multiplier.map_or_else(|| base.to_owned(), |m| format!("{base} {m}")))
}

/// Read the plan label from the profile endpoint.
async fn fetch_plan(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<Option<String>, String> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            format!("claude-cli/{CLAUDE_VERSION} (external, cli)"),
        )
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "Claude profile request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Claude profile request failed: {}",
            response.status()
        ));
    }
    let profile: Value = response
        .json()
        .await
        .map_err(|_| "Invalid Claude profile".to_string())?;
    let plan = plan_label(&profile);
    if plan.is_none() {
        // Field names only, never values, so an unexpected response shape can be diagnosed.
        let names = |value: Option<&Value>| {
            value
                .and_then(Value::as_object)
                .map(|object| object.keys().cloned().collect::<Vec<_>>())
        };
        tracing::warn!(
            top_level = ?names(Some(&profile)),
            organization = ?names(profile.get("organization")),
            account = ?names(profile.get("account")),
            "Claude profile named no subscription plan"
        );
    }
    Ok(plan)
}

/// Whether an upstream read for this credential was attempted recently; records this attempt.
fn attempted_recently(credential: &str) -> bool {
    let Ok(mut attempts) = ATTEMPTS.lock() else {
        return true;
    };
    let now = Instant::now();
    if attempts
        .get(credential)
        .is_some_and(|last| now.duration_since(*last) < RETRY_AFTER)
    {
        return true;
    }
    attempts.insert(credential.to_owned(), now);
    false
}

/// The credential's subscription plan: stored when fresh, otherwise read once and stored.
pub(crate) async fn plan(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
) -> Option<String> {
    let name = setting_name(credential);
    let stored: Option<Stored> = store
        .setting_get(&name)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if let Some(stored) = &stored
        && now.saturating_sub(stored.checked_at) < REFRESH_AFTER_SECS
    {
        return stored.plan.clone();
    }
    let known = stored.and_then(|stored| stored.plan);
    if attempted_recently(credential) {
        return known;
    }
    let Ok(credentials) = auth::load(store, provider, credential).await else {
        return known;
    };
    let Ok(plan) = fetch_plan(
        &upstream::HTTP_CLIENT,
        PROFILE_URL,
        &credentials.access_token,
    )
    .await
    else {
        return known;
    };
    if let Ok(value) = serde_json::to_string(&Stored {
        checked_at: now,
        plan: plan.clone(),
    }) {
        let mut db = (*store.database.db).clone();
        let _ = crate::storage::Setting::upsert_by_name(name.as_str())
            .value(value)
            .exec(&mut db)
            .await;
    }
    plan.or(known)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::HeaderMap, routing::get};
    use serde_json::json;

    #[test]
    fn plan_labels_follow_the_organization_type_and_tier() {
        let label = |value: Value| plan_label(&value);
        assert_eq!(
            label(json!({"organization": {"organization_type": "claude_max",
                "rate_limit_tier": "default_claude_max_5x"}})),
            Some("Max 5x".into())
        );
        assert_eq!(
            label(json!({"organization": {"organization_type": "claude_max",
                "rate_limit_tier": "default_claude_max_20x"}})),
            Some("Max 20x".into())
        );
        assert_eq!(
            label(json!({"organization": {"organization_type": "claude_max"}})),
            Some("Max".into())
        );
        assert_eq!(
            label(json!({"organization": {"organization_type": "claude_pro"}})),
            Some("Pro".into())
        );
        assert_eq!(
            label(json!({"organization": {"organization_type": "claude_team"}})),
            Some("Team".into())
        );
        assert_eq!(
            label(json!({"account": {"has_claude_pro": true}})),
            Some("Pro".into())
        );
        assert_eq!(label(json!({"account": {"email": "a@b.c"}})), None);
    }

    #[tokio::test]
    async fn profile_request_sends_the_token_and_reads_the_plan() {
        let app = Router::new().route(
            "/profile",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer secret");
                assert_eq!(headers["anthropic-beta"], "oauth-2025-04-20");
                Json(json!({"organization": {"organization_type": "claude_pro"}}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let plan = fetch_plan(
            &upstream::HTTP_CLIENT,
            &format!("http://{address}/profile"),
            "secret",
        )
        .await
        .unwrap();
        assert_eq!(plan.as_deref(), Some("Pro"));
        let refused = fetch_plan(
            &upstream::HTTP_CLIENT,
            &format!("http://{address}/missing"),
            "secret",
        )
        .await;
        assert!(refused.is_err());
    }
}

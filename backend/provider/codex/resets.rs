//! Codex rate-limit reset credits.
//!
//! OpenAI banks a reset for some accounts. It refills the five-hour and weekly windows at once and
//! expires 30 days after it is granted. The ChatGPT backend lists and spends them; the shapes below
//! follow the official Codex client.

use serde_json::{Value, json};

use super::auth::{CODEX_BASE_URL, CodexAuthManager};
use super::version;
use crate::provider::resets::{RedeemResult, ResetCredit, ResetCredits, is_safe_id, unix_now};
use crate::server::settings::SettingsStore;

/// Lists the account's reset credits.
pub(super) async fn list(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
) -> Result<ResetCredits, String> {
    let credentials = load(store, provider, credential).await?;
    list_at(
        &crate::util::HTTP_CLIENT,
        CODEX_BASE_URL,
        &credentials.access_token,
        credentials.account_id.as_deref(),
        unix_now(),
    )
    .await
}

/// Spends one reset credit after confirming it is still usable.
pub(super) async fn redeem(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
    reset: &str,
) -> Result<RedeemResult, String> {
    let credentials = load(store, provider, credential).await?;
    redeem_at(
        &crate::util::HTTP_CLIENT,
        CODEX_BASE_URL,
        &credentials.access_token,
        credentials.account_id.as_deref(),
        reset,
        unix_now(),
    )
    .await
}

/// Load the stored credential, refreshing it when needed.
async fn load(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
) -> Result<super::auth::Credentials, String> {
    // Refresh errors can contain upstream response bodies; never return them to the caller.
    CodexAuthManager::credentials(store, provider, credential)
        .await
        .map_err(|_| "Unable to load OpenAI Codex credentials".to_string())?
        .ok_or_else(|| "OpenAI Codex is not connected".to_string())
}

/// Headers the Codex client sends.
fn with_headers(
    request: reqwest::RequestBuilder,
    token: &str,
    account: Option<&str>,
) -> reqwest::RequestBuilder {
    let request = request
        .bearer_auth(token)
        .header(
            reqwest::header::USER_AGENT,
            format!("codex_cli_rs/{}", version::current()),
        )
        .header(reqwest::header::ACCEPT, "application/json");
    match account {
        Some(account) => request.header("ChatGPT-Account-Id", account),
        None => request,
    }
}

/// Read the reset credits from the backend at `base`.
async fn list_at(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    account: Option<&str>,
    now: i64,
) -> Result<ResetCredits, String> {
    let response = with_headers(
        client.get(format!("{base}/wham/rate-limit-reset-credits")),
        token,
        account,
    )
    .timeout(std::time::Duration::from_secs(15))
    .send()
    .await
    .map_err(|_| "Codex reset request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Codex reset request failed with status {}",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "Invalid Codex reset response".to_string())?;
    Ok(parse_credits(&body, now))
}

/// Turn the credit list into the shared reset list, leaving out credits that are spent or expired.
fn parse_credits(body: &Value, now: i64) -> ResetCredits {
    let credits = body
        .get("credits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|credit| parse_credit(credit, now))
        .collect();
    ResetCredits {
        credits,
        note: None,
    }
}

/// One credit, or `None` when it is malformed, not available or expired.
fn parse_credit(credit: &Value, now: i64) -> Option<ResetCredit> {
    let id = credit.get("id")?.as_str().filter(|id| is_safe_id(id))?;
    if !credit
        .get("status")?
        .as_str()?
        .eq_ignore_ascii_case("available")
    {
        return None;
    }
    let expires_at = credit
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(time::OffsetDateTime::unix_timestamp);
    if expires_at.is_some_and(|expires| expires <= now) {
        return None;
    }
    let label = credit
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Limit reset");
    Some(ResetCredit {
        id: id.to_owned(),
        label: label.to_owned(),
        remaining: 1,
        expires_at,
        usable: true,
        note: None,
    })
}

/// Spend a credit through the backend at `base`.
async fn redeem_at(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    account: Option<&str>,
    reset: &str,
    now: i64,
) -> Result<RedeemResult, String> {
    // Look again now: the list on screen may be stale, and a spent reset cannot be put back.
    let current = list_at(client, base, token, account, now).await?;
    if !current.credits.iter().any(|credit| credit.id == reset) {
        return Ok(not_redeemed("That reset is no longer available."));
    }
    let response = with_headers(
        client.post(format!("{base}/wham/rate-limit-reset-credits/consume")),
        token,
        account,
    )
    .timeout(std::time::Duration::from_secs(25))
    .json(&json!({
        "redeem_request_id": uuid::Uuid::new_v4().to_string(),
        "credit_id": reset,
    }))
    .send()
    .await
    // Never retried: the request may have reached OpenAI before the connection failed.
    .map_err(|_| {
        "Codex did not answer; the reset may or may not have been spent. Refresh to check."
            .to_string()
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => "Codex refused the credential; reconnect the account".to_string(),
            429 => "Codex is limiting reset requests; try again later".to_string(),
            code => format!("Codex reset request failed with status {code}"),
        });
    }
    let body: Value = response.json().await.map_err(|_| {
        "Codex sent an unreadable answer; the reset may have been spent. Refresh to check."
            .to_string()
    })?;
    match body.get("code").and_then(Value::as_str) {
        Some("reset") => Ok(RedeemResult {
            redeemed: true,
            message: "Reset applied.".to_string(),
        }),
        Some("nothing_to_reset") => Ok(not_redeemed(
            "Nothing to reset yet: your limits are not used up.",
        )),
        Some("no_credit") => Ok(not_redeemed("No reset credit is left.")),
        Some("already_redeemed") => Ok(not_redeemed("That reset was already used.")),
        _ => Err(
            "Codex sent an unexpected answer; the reset may have been spent. Refresh to check."
                .to_string(),
        ),
    }
}

/// A reset that was not spent.
fn not_redeemed(message: &str) -> RedeemResult {
    RedeemResult {
        redeemed: false,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::HeaderMap,
        routing::{get, post},
    };
    use std::sync::{Arc, Mutex};

    const NOW: i64 = 1_790_000_000;

    /// Spend requests the mock backend received: `(account header, body)`.
    type Spent = Arc<Mutex<Vec<(String, Value)>>>;

    /// Mock `ChatGPT` backend with an available credit, a spent one, an expired one and one without a deadline.
    async fn serve(code: &'static str) -> (String, Spent) {
        let spent: Spent = Arc::default();
        let app = Router::new()
            .route(
                "/wham/rate-limit-reset-credits",
                get(|| async {
                    Json(json!({"available_count": 2, "credits": [
                        {"id": "c1", "reset_type": "codex_rate_limits", "status": "available",
                         "granted_at": "2026-11-12T00:00:00Z", "expires_at": "2027-01-15T00:00:00Z",
                         "title": "Full reset (Weekly + 5 hr)"},
                        {"id": "c2", "reset_type": "codex_rate_limits", "status": "redeemed",
                         "granted_at": "2026-10-01T00:00:00Z", "expires_at": "2027-01-15T00:00:00Z"},
                        {"id": "c3", "reset_type": "codex_rate_limits", "status": "available",
                         "granted_at": "2026-01-01T00:00:00Z", "expires_at": "2026-02-01T00:00:00Z"},
                        {"id": "c4", "reset_type": "codex_rate_limits", "status": "available",
                         "granted_at": "2026-11-12T00:00:00Z", "expires_at": null},
                        {"id": "bad id", "status": "available"}
                    ]}))
                }),
            )
            .route(
                "/wham/rate-limit-reset-credits/consume",
                post(
                    |State((spent, code)): State<(Spent, &'static str)>,
                     headers: HeaderMap,
                     Json(body): Json<Value>| async move {
                        let account = headers
                            .get("chatgpt-account-id")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        spent.lock().unwrap().push((account, body));
                        Json(json!({"code": code, "windows_reset": 2}))
                    },
                ),
            )
            .with_state((spent.clone(), code));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, spent)
    }

    /// Only available, unexpired, well-formed credits are offered, each with its expiry.
    #[tokio::test]
    async fn lists_available_credits_with_expiry() {
        let (base, _) = serve("reset").await;
        let resets = list_at(&reqwest::Client::new(), &base, "token", Some("acct"), NOW)
            .await
            .unwrap();
        let summary: Vec<_> = resets
            .credits
            .iter()
            .map(|c| (c.id.as_str(), c.label.as_str(), c.expires_at, c.usable))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "c1",
                    "Full reset (Weekly + 5 hr)",
                    Some(1_799_971_200),
                    true
                ),
                ("c4", "Limit reset", None, true),
            ]
        );
    }

    /// Spending sends the chosen credit and the account header, and reports success.
    #[tokio::test]
    async fn redeems_the_chosen_credit() {
        let (base, spent) = serve("reset").await;
        let result = redeem_at(
            &reqwest::Client::new(),
            &base,
            "token",
            Some("acct"),
            "c1",
            NOW,
        )
        .await
        .unwrap();
        assert!(result.redeemed, "{}", result.message);
        let spent = spent.lock().unwrap().clone();
        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].0, "acct");
        assert_eq!(spent[0].1["credit_id"], "c1");
        assert!(uuid::Uuid::parse_str(spent[0].1["redeem_request_id"].as_str().unwrap()).is_ok());
    }

    /// A credit that is gone, spent, expired or unknown is refused here without calling the spend endpoint.
    #[tokio::test]
    async fn refuses_to_spend_what_is_not_available() {
        let (base, spent) = serve("reset").await;
        for reset in ["c2", "c3", "nope"] {
            let result = redeem_at(&reqwest::Client::new(), &base, "token", None, reset, NOW)
                .await
                .unwrap();
            assert!(!result.redeemed);
            assert!(result.message.contains("no longer available"), "{reset}");
        }
        assert!(spent.lock().unwrap().is_empty());
    }

    /// The backend's own refusals are explained and never count as spent; an unknown answer is an error to check.
    #[tokio::test]
    async fn reports_backend_outcomes() {
        for (code, expected) in [
            ("nothing_to_reset", "not used up"),
            ("no_credit", "No reset credit"),
            ("already_redeemed", "already used"),
        ] {
            let (base, _) = serve(code).await;
            let result = redeem_at(&reqwest::Client::new(), &base, "token", None, "c1", NOW)
                .await
                .unwrap();
            assert!(!result.redeemed);
            assert!(
                result.message.contains(expected),
                "{code}: {}",
                result.message
            );
        }
        let (base, _) = serve("something_new").await;
        let error = redeem_at(&reqwest::Client::new(), &base, "token", None, "c1", NOW)
            .await
            .unwrap_err();
        assert!(error.contains("may have been spent"), "{error}");
    }
}

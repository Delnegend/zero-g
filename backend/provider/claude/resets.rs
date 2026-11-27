//! Claude limit resets.
//!
//! Anthropic grants some subscriptions a banked reset that refills usage windows at once (the
//! `cedar_ember` program). Claude Code reads them from its usage endpoint and spends one through
//! the organization. Neither call is documented; the shapes below follow what Claude Code and
//! other open-source clients send and read.

use serde_json::{Value, json};

use super::{auth, upstream, version};
use crate::provider::resets::{
    RedeemResult, ResetCredit, ResetCredits, is_safe_id, plain_reason, unix_now,
};
use crate::server::settings::SettingsStore;

/// Reset program Anthropic runs for subscription limit resets.
const PROGRAM: &str = "cedar_ember";

/// Lists the account's resets.
pub(super) async fn list(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
) -> Result<ResetCredits, String> {
    let credentials = load(store, provider, credential).await?;
    list_at(
        &upstream::HTTP_CLIENT,
        upstream::API_ROOT,
        &credentials.access_token,
        unix_now(),
    )
    .await
}

/// Spends one reset after confirming it is still usable.
pub(super) async fn redeem(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
    reset: &str,
) -> Result<RedeemResult, String> {
    let credentials = load(store, provider, credential).await?;
    let organization = credentials.organization_id.as_deref().unwrap_or_default();
    redeem_at(
        &upstream::HTTP_CLIENT,
        upstream::API_ROOT,
        &credentials.access_token,
        organization,
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
) -> Result<auth::Credentials, String> {
    auth::load(store, provider, credential)
        .await
        .map_err(|_| "Unable to load Claude credentials".to_string())
}

/// Headers Claude Code sends; Anthropic offers resets to no other client.
fn with_headers(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, version::user_agent())
}

/// Read the reset program from the usage endpoint at `root`.
async fn list_at(
    client: &reqwest::Client,
    root: &str,
    token: &str,
    now: i64,
) -> Result<ResetCredits, String> {
    let response = with_headers(
        client.get(format!("{root}/api/oauth/usage?cedar_ember=1&skip_spend=1")),
        token,
    )
    .timeout(std::time::Duration::from_secs(15))
    .send()
    .await
    .map_err(|_| "Claude reset request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Claude reset request failed (HTTP {})",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "Invalid Claude reset response".to_string())?;
    Ok(parse_grants(&body, now))
}

/// Turn the `cedar_ember` block into the shared reset list.
///
/// A grant that is spent, expired or malformed is left out: there is nothing to offer for it.
fn parse_grants(body: &Value, now: i64) -> ResetCredits {
    let Some(block) = body.get("cedar_ember").filter(|block| block.is_object()) else {
        return ResetCredits::default();
    };
    if block.get("eligible").and_then(Value::as_bool) == Some(false) {
        let reason = block
            .get("ineligible_reason")
            .and_then(Value::as_str)
            .map(plain_reason)
            .filter(|reason| !reason.is_empty());
        return ResetCredits {
            credits: Vec::new(),
            note: Some(reason.map_or_else(
                || "Anthropic does not offer resets to this account".to_string(),
                |reason| format!("Anthropic does not offer resets to this account ({reason})"),
            )),
        };
    }
    let credits = block
        .get("grants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|grant| parse_grant(grant, now))
        .collect();
    ResetCredits {
        credits,
        note: None,
    }
}

/// One grant, or `None` when it is malformed, spent or expired.
fn parse_grant(grant: &Value, now: i64) -> Option<ResetCredit> {
    let id = grant.get("id")?.as_str().filter(|id| is_safe_id(id))?;
    let remaining = u32::try_from(grant.get("resets_left")?.as_u64()?).ok()?;
    if remaining == 0 {
        return None;
    }
    let expires_at = grant
        .get("ends_at")
        .and_then(Value::as_str)
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(time::OffsetDateTime::unix_timestamp);
    if expires_at.is_some_and(|expires| expires <= now) {
        return None;
    }
    let paused = grant.get("paused").and_then(Value::as_bool) == Some(true);
    let usable = grant.get("usable_now").and_then(Value::as_bool) == Some(true) && !paused;
    let label = grant
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or("Limit reset");
    Some(ResetCredit {
        id: id.to_owned(),
        label: label.to_owned(),
        remaining,
        expires_at,
        usable,
        note: (!usable).then(|| {
            if paused {
                "Paused by Anthropic".to_string()
            } else {
                "Nothing to reset yet".to_string()
            }
        }),
    })
}

/// Spend a grant through the organization at `root`.
async fn redeem_at(
    client: &reqwest::Client,
    root: &str,
    token: &str,
    organization: &str,
    reset: &str,
    now: i64,
) -> Result<RedeemResult, String> {
    if !is_safe_id(organization) {
        return Err(
            "Anthropic did not report an organization for this account; reconnect Claude to use resets"
                .to_string(),
        );
    }
    // Look again now: the list on screen may be stale, and a spent reset cannot be put back.
    let current = list_at(client, root, token, now).await?;
    let Some(credit) = current.credits.iter().find(|credit| credit.id == reset) else {
        return Ok(not_redeemed("That reset is no longer available."));
    };
    if !credit.usable {
        return Ok(not_redeemed(
            credit
                .note
                .as_deref()
                .unwrap_or("That reset cannot be used right now."),
        ));
    }
    let response = with_headers(
        client.post(format!(
            "{root}/api/organizations/{organization}/reset_rate_limits"
        )),
        token,
    )
    .timeout(std::time::Duration::from_secs(25))
    .json(&json!({
        "program": PROGRAM,
        "grant_id": reset,
        "request_id": uuid::Uuid::new_v4().to_string(),
    }))
    .send()
    .await
    // Never retried: the request may have reached Anthropic before the connection failed.
    .map_err(|_| {
        "Claude did not answer; the reset may or may not have been spent. Refresh to check."
            .to_string()
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => "Claude refused the credential; reconnect the account".to_string(),
            429 => "Claude is limiting reset requests; try again later".to_string(),
            code => format!("Claude reset request failed (HTTP {code})"),
        });
    }
    let body: Value = response.json().await.map_err(|_| {
        "Claude sent an unreadable answer; the reset may have been spent. Refresh to check."
            .to_string()
    })?;
    let result = body
        .get("result")
        .and_then(Value::as_str)
        .map(plain_reason)
        .filter(|result| !result.is_empty())
        .ok_or_else(|| {
            "Claude sent an unexpected answer; the reset may have been spent. Refresh to check."
                .to_string()
        })?;
    Ok(match result.as_str() {
        "reset" => RedeemResult {
            redeemed: true,
            message: "Reset applied.".to_string(),
        },
        "already_used" => not_redeemed("That reset was already used."),
        "not_limited" => not_redeemed("Nothing to reset yet."),
        other => not_redeemed(&format!("Anthropic answered: {other}")),
    })
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
        http::{HeaderMap, StatusCode},
        routing::{get, post},
    };
    use std::sync::{Arc, Mutex};

    const NOW: i64 = 1_790_000_000;

    /// Spend requests the mock Anthropic received: `(path, user agent, body)`.
    type Spent = Arc<Mutex<Vec<(String, String, Value)>>>;

    /// Mock Anthropic with one usable grant, one that clears nothing yet, and spent, expired and paused ones.
    async fn serve(answer: Value) -> (String, Spent) {
        let spent: Spent = Arc::default();
        let app = Router::new()
            .route(
                "/api/oauth/usage",
                get(|| async {
                    Json(json!({"five_hour": null, "cedar_ember": {
                        "eligible": true,
                        "grants": [
                            {"id": "fable-1", "label": "Fable reset", "resets_left": 2, "resets_total": 3,
                             "ends_at": "2027-01-15T00:00:00Z", "usable_now": true},
                            {"id": "weekly-1", "label": "Weekly reset", "resets_left": 1,
                             "ends_at": "2027-01-20T00:00:00Z", "usable_now": false},
                            {"id": "spent", "resets_left": 0, "usable_now": true},
                            {"id": "old", "resets_left": 1, "ends_at": "2020-01-01T00:00:00Z", "usable_now": true},
                            {"id": "paused", "resets_left": 1, "usable_now": true, "paused": true},
                            {"id": "bad id!", "resets_left": 1, "usable_now": true}
                        ],
                        "next_grant_id": "fable-1"
                    }}))
                }),
            )
            .route(
                "/api/organizations/{org}/reset_rate_limits",
                post(
                    |State((spent, answer)): State<(Spent, Value)>,
                     axum::extract::Path(org): axum::extract::Path<String>,
                     headers: HeaderMap,
                     Json(body): Json<Value>| async move {
                        let agent = headers
                            .get("user-agent")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        spent.lock().unwrap().push((org, agent, body));
                        Json(answer)
                    },
                ),
            )
            .with_state((spent.clone(), answer));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (root, spent)
    }

    /// Only usable, unexpired, well-formed grants with resets left are offered, each with its expiry.
    #[tokio::test]
    async fn lists_grants_with_expiry_and_usability() {
        let (root, _) = serve(json!({"result": "reset"})).await;
        let resets = list_at(&reqwest::Client::new(), &root, "token", NOW)
            .await
            .unwrap();
        let summary: Vec<_> = resets
            .credits
            .iter()
            .map(|c| (c.id.as_str(), c.label.as_str(), c.remaining, c.usable))
            .collect();
        assert_eq!(
            summary,
            [
                ("fable-1", "Fable reset", 2, true),
                ("weekly-1", "Weekly reset", 1, false),
                ("paused", "Limit reset", 1, false),
            ]
        );
        assert_eq!(resets.credits[0].expires_at, Some(1_799_971_200));
        assert_eq!(resets.credits[0].note, None);
        assert_eq!(
            resets.credits[1].note.as_deref(),
            Some("Nothing to reset yet")
        );
        assert_eq!(
            resets.credits[2].note.as_deref(),
            Some("Paused by Anthropic")
        );
        assert_eq!(resets.note, None);
    }

    /// An account Anthropic does not offer resets to gets an explanation, not an error.
    #[test]
    fn explains_why_an_account_has_no_resets() {
        let resets = parse_grants(
            &json!({"cedar_ember": {"eligible": false, "ineligible_reason": "surface", "grants": []}}),
            NOW,
        );
        assert!(resets.credits.is_empty());
        assert_eq!(
            resets.note.as_deref(),
            Some("Anthropic does not offer resets to this account (surface)")
        );
        // No program block at all means the account has none, with nothing to explain.
        let none = parse_grants(&json!({"cedar_ember": null, "five_hour": null}), NOW);
        assert!(none.credits.is_empty() && none.note.is_none());
    }

    /// Spending sends the grant, program and organization with Claude Code's identity, and reports success.
    #[tokio::test]
    async fn redeems_the_chosen_grant() {
        let (root, spent) =
            serve(json!({"result": "reset", "cleared": ["seven_day_overage_included"]})).await;
        let result = redeem_at(
            &reqwest::Client::new(),
            &root,
            "token",
            "org-1",
            "fable-1",
            NOW,
        )
        .await
        .unwrap();
        assert!(result.redeemed, "{}", result.message);
        let spent = spent.lock().unwrap().clone();
        assert_eq!(spent.len(), 1);
        let (org, agent, body) = &spent[0];
        assert_eq!(org, "org-1");
        assert!(agent.starts_with("claude-cli/"), "{agent}");
        assert_eq!(body["program"], "cedar_ember");
        assert_eq!(body["grant_id"], "fable-1");
        assert!(
            uuid::Uuid::parse_str(body["request_id"].as_str().unwrap()).is_ok(),
            "{body}"
        );
    }

    /// A reset that is stale, unusable or unknown is refused here, without ever calling Anthropic's spend endpoint.
    #[tokio::test]
    async fn refuses_to_spend_what_is_not_usable() {
        let (root, spent) = serve(json!({"result": "reset"})).await;
        for (reset, expected) in [
            ("weekly-1", "Nothing to reset yet"),
            ("paused", "Paused by Anthropic"),
            ("old", "no longer available"),
            ("nope", "no longer available"),
        ] {
            let result = redeem_at(&reqwest::Client::new(), &root, "token", "org-1", reset, NOW)
                .await
                .unwrap();
            assert!(!result.redeemed);
            assert!(
                result.message.contains(expected),
                "{reset}: {}",
                result.message
            );
        }
        assert!(spent.lock().unwrap().is_empty());
    }

    /// Anthropic's other answers are passed on in plain words and never count as spent.
    #[tokio::test]
    async fn reports_answers_other_than_reset() {
        for (answer, expected) in [
            ("already_used", "already used"),
            ("not_limited", "Nothing to reset yet"),
            ("weird<script>", "Anthropic answered: weirdscript"),
        ] {
            let (root, _) = serve(json!({"result": answer})).await;
            let result = redeem_at(
                &reqwest::Client::new(),
                &root,
                "token",
                "org-1",
                "fable-1",
                NOW,
            )
            .await
            .unwrap();
            assert!(!result.redeemed);
            assert!(
                result.message.contains(expected),
                "{answer}: {}",
                result.message
            );
        }
        // An answer with no result may have spent the reset, so it is an error to check, not a clean "no".
        let (root, _) = serve(json!({"unexpected": true})).await;
        let error = redeem_at(
            &reqwest::Client::new(),
            &root,
            "token",
            "org-1",
            "fable-1",
            NOW,
        )
        .await
        .unwrap_err();
        assert!(error.contains("may have been spent"), "{error}");
    }

    /// Without an organization the request is not sent; a refusal status becomes a clear message.
    #[tokio::test]
    async fn needs_an_organization_and_explains_refusals() {
        let (root, spent) = serve(json!({"result": "reset"})).await;
        for organization in ["", "../x", "a/b"] {
            let error = redeem_at(
                &reqwest::Client::new(),
                &root,
                "token",
                organization,
                "fable-1",
                NOW,
            )
            .await
            .unwrap_err();
            assert!(error.contains("reconnect Claude"), "{error}");
        }
        assert!(spent.lock().unwrap().is_empty());
        let app = Router::new().route(
            "/api/oauth/usage",
            get(|| async { StatusCode::TOO_MANY_REQUESTS }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let limited = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let error = list_at(&reqwest::Client::new(), &limited, "token", NOW)
            .await
            .unwrap_err();
        assert!(error.contains("429"), "{error}");
    }
}

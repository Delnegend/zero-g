use std::borrow::Cow;
use std::sync::LazyLock;

use crate::provider::common::cooldown;
use crate::provider::common::pool::{PoolError, eligible_by, rank_accounts};
use crate::provider::credentials::Credential;
use axum::http::StatusCode;
/// Shared bounded client that never forwards credentials through redirects.
pub(super) static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});
/// Anthropic API origin used with subscription tokens.
pub(super) const API_ROOT: &str = "https://api.anthropic.com";

/// Beta flags a subscription token needs to be accepted as Claude Code.
pub(super) const REQUIRED_BETAS: [&str; 2] = ["claude-code-20250219", "oauth-2025-04-20"];
/// Send a prepared body and caller-chosen headers with a subscription token.
pub(super) async fn forward(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    headers: reqwest::header::HeaderMap,
    body: axum::body::Bytes,
) -> Result<reqwest::Response, String> {
    client
        .post(url)
        .headers(headers)
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Claude request failed: {e}"))
}

/// Why no credential produced a usable response.
pub(super) enum Refusal {
    /// Every connected credential is cooling down; retry after this long.
    CoolingDown(std::time::Duration),
    /// A gateway-side failure, reported with this status and message.
    Failed(StatusCode, String),
}

/// Claude's per-credential, per-model cooldown key.
fn cooldown_key(credential: &Credential, model: &str) -> String {
    format!("claude:{}:{}:{}", credential.id.len(), credential.id, model)
}

/// Try connected credentials in ranked order until one is not refused.
///
/// A 401, 403, 429, or 503 moves on to the next credential. Rate limits are
/// scoped to the requested model; other refusals take precedence over a 429.
pub(super) async fn first_accepted_at<F, Fut>(
    store: &crate::server::settings::SettingsStore,
    provider: &crate::provider::registry::Provider,
    catalog_base: &str,
    model: &str,
    mut send: F,
) -> Result<(reqwest::Response, Credential), Refusal>
where
    F: FnMut(super::auth::Credentials, &Credential) -> Fut,
    Fut: Future<Output = Result<reqwest::Response, String>>,
{
    let credentials = match eligible_by(store, provider, |credential| {
        Cow::Owned(cooldown_key(credential, model))
    })
    .await
    {
        Ok(credentials) => credentials,
        Err(PoolError::Cooling(wait)) => return Err(Refusal::CoolingDown(wait)),
        Err(PoolError::Internal(message)) => {
            return Err(Refusal::Failed(StatusCode::INTERNAL_SERVER_ERROR, message));
        }
        Err(err) => {
            return Err(Refusal::Failed(
                StatusCode::SERVICE_UNAVAILABLE,
                err.to_string(),
            ));
        }
    };
    let mut failure = None;
    let mut candidates = Vec::with_capacity(credentials.len());
    for credential in credentials {
        let creds = match super::auth::load(store, &provider.id, &credential.id).await {
            Ok(creds) => creds,
            Err(e) => {
                failure = Some(Refusal::Failed(StatusCode::SERVICE_UNAVAILABLE, e));
                continue;
            }
        };
        let catalog = super::models::account_catalog(
            &HTTP_CLIENT,
            catalog_base,
            &credential.id,
            &creds.access_token,
        )
        .await;
        candidates.push(((credential, creds), catalog));
    }
    let ranked = rank_accounts(candidates, model);
    let mut last = None;
    let mut limited = None;
    for (credential, creds) in ranked {
        let cooldown_id = cooldown_key(&credential, model);
        let response = match send(creds, &credential).await {
            Ok(response) => response,
            Err(e) => {
                failure = Some(Refusal::Failed(StatusCode::BAD_GATEWAY, e));
                continue;
            }
        };
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            super::models::invalidate_catalog_on_auth_error(&credential.id, Some(status)).await;
            last = Some((response, credential));
            continue;
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            cooldown::start(&cooldown_id, cooldown::wait_for(response.headers()));
            limited = Some((response, credential));
            continue;
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            last = Some((response, credential));
            continue;
        }
        cooldown::clear(&cooldown_id);
        return Ok((response, credential));
    }
    if let Some(refused) = last {
        return Ok(refused);
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    if let Some(refused) = limited {
        return Ok(refused);
    }
    Err(Refusal::Failed(
        StatusCode::SERVICE_UNAVAILABLE,
        "No connected Claude credentials".to_owned(),
    ))
}

/// HTTP transport and credential routing tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use serde_json::Value;

    /// Send a minimal Messages body for `model` through credential selection.
    async fn send_at(
        state: &crate::server::AppState,
        provider: &crate::provider::registry::Provider,
        root: &str,
        model: &str,
    ) -> Result<(reqwest::Response, Credential), Refusal> {
        let body = axum::body::Bytes::from(serde_json::json!({"model": model}).to_string());
        let url = format!("{root}/v1/messages");
        first_accepted_at(&state.settings, provider, root, model, |creds, _| {
            let (url, body) = (url.clone(), body.clone());
            async move {
                forward(
                    &HTTP_CLIENT,
                    &url,
                    &creds.access_token,
                    reqwest::header::HeaderMap::new(),
                    body,
                )
                .await
            }
        })
        .await
    }

    /// Never follow upstream redirects while carrying credentials.
    #[tokio::test]
    async fn refuses_redirects() {
        let app = Router::new()
            .route(
                "/",
                get(|| async { axum::response::Redirect::temporary("/target") }),
            )
            .route("/target", get(|| async { "unexpected" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let response = HTTP_CLIENT
            .get(format!("http://{address}/"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    }

    /// Route to the account advertising a model, aggregate discovery across accounts, and keep unlisted models reachable.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn routes_by_account_catalog_aggregates_discovery_and_allows_unlisted() {
        use crate::database::Database;
        use crate::provider::registry::Provider;
        use crate::server::{AppState, settings::SettingsStore};
        use crate::storage::{ProviderCredential, ProviderRecord, Secret};
        use axum::{Json, http::HeaderMap, routing::post};
        use serde_json::json;
        use std::sync::{Arc, Mutex};

        let database = Database::open(":memory:").await.unwrap();
        let settings = SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*database.db).clone();
        toasty::create!(ProviderRecord {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            kind: "claude".to_string(),
            base_url: String::new(),
            keyless: false,
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        for (id, weight) in [("a", 1_000_000_000), ("b", 1)] {
            toasty::create!(ProviderCredential {
                id: id.to_string(),
                provider_id: "claude".to_string(),
                name: format!("account {id}"),
                weight,
                enabled: true
            })
            .exec(&mut db)
            .await
            .unwrap();
            let name = format!("credential.{id}.credential");
            let credential = json!({
                "access_token": format!("token-{id}"),
                "refresh_token": "refresh",
                "expires_at": i64::MAX,
                "account_id": "acct"
            });
            Secret::upsert_by_name(name.as_str())
                .value(settings.encrypt(&name, &credential.to_string()).unwrap())
                .exec(&mut db)
                .await
                .unwrap();
        }
        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings,
            database,
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        let provider = Provider {
            id: "claude".into(),
            name: "Claude Code".into(),
            kind: "claude".into(),
            base_url: String::new(),
            keyless: false,
        };

        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let seen_messages = seen.clone();
        let app = Router::new()
            .route(
                "/v1/models",
                get(|headers: HeaderMap| async move {
                    let auth = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default();
                    if auth == "Bearer token-a" {
                        Json(json!({"data":[{"id":"model-x"},{"id":"shared"}],"has_more":false}))
                    } else {
                        Json(json!({"data":[{"id":"model-y"},{"id":"shared"}],"has_more":false}))
                    }
                }),
            )
            .route(
                "/v1/messages",
                post(move |headers: HeaderMap, body: axum::body::Bytes| {
                    let seen_messages = seen_messages.clone();
                    async move {
                        let auth = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        let value: Value = serde_json::from_slice(&body).unwrap();
                        let model = value["model"].as_str().unwrap_or_default().to_owned();
                        seen_messages.lock().unwrap().push((auth, model));
                        Json(json!({"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"test","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let discovered = super::super::models::discover_at(&state, &provider, &root, None)
            .await
            .unwrap();
        let ids: Vec<_> = discovered.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"model-x"));
        assert!(ids.contains(&"model-y"));
        assert!(ids.contains(&"shared"));

        let (resp_y, cred_y) = send_at(&state, &provider, &root, "model-y")
            .await
            .unwrap_or_else(|_| panic!("expected model-y to succeed"));
        assert!(resp_y.status().is_success());
        assert_eq!(cred_y.id, "b");

        let (resp_z, _) = send_at(&state, &provider, &root, "model-z")
            .await
            .unwrap_or_else(|_| panic!("expected unlisted model-z to reach pool"));
        assert!(resp_z.status().is_success());

        let recorded = seen.lock().unwrap();
        assert_eq!(
            recorded[0],
            ("Bearer token-b".to_owned(), "model-y".to_owned())
        );
        assert_eq!(recorded[1].1, "model-z");
    }
}

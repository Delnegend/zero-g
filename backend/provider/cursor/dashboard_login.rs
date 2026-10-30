use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::auth::{self, Credentials};
use crate::provider::login::{credential_lock, error, require_credential, same_origin};
use crate::server::AppState;

/// Cursor's device login polling endpoint.
const POLL_URL: &str = "https://api2.cursor.sh/auth/poll";
/// Minimum polling interval in seconds.
const INTERVAL: u64 = 5;
/// Maximum lifetime of a pending login flow.
const LIFETIME: Duration = Duration::from_secs(600);

/// Shared pending login flows.
#[derive(Clone, Default)]
struct Flows(
    /// Pending flows indexed by their opaque IDs.
    Arc<tokio::sync::Mutex<HashMap<String, Flow>>>,
);

/// Provider-bound PKCE authorization state.
struct Flow {
    /// Provider owning this flow.
    provider: String,
    /// Credential that receives the tokens.
    credential: String,
    /// Secret PKCE verifier.
    verifier: String,
    /// Cursor's device identifier.
    uuid: String,
    /// Flow creation time.
    created: Instant,
    /// Next permitted upstream poll.
    next_poll: Instant,
    /// Whether a poll is in progress.
    in_flight: bool,
}

/// Cursor login start response.
#[derive(Serialize, utoipa::ToSchema)]
struct CursorStartResponse {
    /// Opaque flow identifier.
    id: String,
    /// Cursor authorization URL.
    url: String,
    /// Suggested polling interval in seconds.
    interval: u64,
}

/// Identifies a pending flow.
#[derive(Deserialize, utoipa::ToSchema)]
struct CursorPollInput {
    /// Opaque flow identifier.
    id: String,
}

/// Cursor login poll status.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CursorPollResponse {
    /// The user has not completed authorization.
    Pending,
    /// Tokens have been saved.
    Complete,
}

/// `OpenAPI` definitions for Cursor login.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(cursor_start, cursor_poll),
    components(schemas(CursorStartResponse, CursorPollInput, CursorPollResponse))
)]
pub(crate) struct Api;

/// Builds Cursor dashboard login routes.
pub(crate) fn router(app: AppState) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/cursor/start",
            post(cursor_start),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/cursor/poll",
            post(cursor_poll),
        )
        .with_state((app, Flows::default()))
}

/// Starts a provider-bound Cursor PKCE flow.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/cursor/start", operation_id = "startCursorLogin", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 200, body = CursorStartResponse)))]
async fn cursor_start(
    State((app, flows)): State<(AppState, Flows)>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<CursorStartResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider, &credential, "cursor").await?;
    // Two independent random UUIDs supply 244 random bits, encoded as a valid PKCE verifier.
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let uuid = Uuid::new_v4().to_string();
    let id = Uuid::new_v4().to_string();
    let url = format!(
        "https://cursor.com/loginDeepControl?challenge={challenge}&uuid={uuid}&mode=login&redirectTarget=cli"
    );
    let now = Instant::now();
    let mut pending = flows.0.lock().await;
    pending.retain(|_, flow| flow.created.elapsed() < LIFETIME);
    pending.insert(
        id.clone(),
        Flow {
            provider,
            credential,
            verifier,
            uuid,
            created: now,
            next_poll: now,
            in_flight: false,
        },
    );
    drop(pending);
    Ok(Json(CursorStartResponse {
        id,
        url,
        interval: INTERVAL,
    }))
}

/// Tokens returned after successful authorization.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenResponse {
    /// Access token issued by Cursor.
    access_token: String,
    /// Refresh token issued by Cursor.
    refresh_token: String,
}

/// Polls Cursor once without exposing the verifier in errors.
async fn poll_upstream(
    client: &reqwest::Client,
    url: &str,
    uuid: &str,
    verifier: &str,
) -> Result<Option<TokenResponse>, String> {
    let response = client
        .get(url)
        .query(&[("uuid", uuid), ("verifier", verifier)])
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Cursor login polling failed: {}", e.without_url()))?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!(
            "Cursor login polling failed: {}",
            response.status()
        ));
    }
    let tokens: TokenResponse = response
        .json()
        .await
        .map_err(|e| format!("Invalid Cursor login response: {e}"))?;
    if tokens.access_token.is_empty() || tokens.refresh_token.is_empty() {
        return Err("Cursor login returned empty credentials".into());
    }
    Ok(Some(tokens))
}

/// Polls a bound flow and stores its resulting tokens.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/cursor/poll", operation_id = "pollCursorLogin", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = CursorPollInput, responses((status = 200, body = CursorPollResponse)))]
async fn cursor_poll(
    State((app, flows)): State<(AppState, Flows)>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<CursorPollInput>,
) -> Result<Json<CursorPollResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider, &credential, "cursor").await?;
    let (uuid, verifier) = {
        let mut pending = flows.0.lock().await;
        let flow = pending
            .get_mut(&input.id)
            .ok_or_else(|| error("Cursor login flow is invalid or expired"))?;
        if flow.provider != provider || flow.credential != credential {
            return Err(error("Cursor login flow is invalid or expired"));
        }
        if flow.created.elapsed() >= LIFETIME {
            pending.remove(&input.id);
            drop(pending);
            return Err(error("Cursor login flow expired"));
        }
        if flow.in_flight || Instant::now() < flow.next_poll {
            return Ok(Json(CursorPollResponse::Pending));
        }
        flow.in_flight = true;
        flow.next_poll = Instant::now()
            .checked_add(Duration::from_secs(INTERVAL))
            .ok_or_else(|| error("Cursor polling interval is too large"))?;
        let values = (flow.uuid.clone(), flow.verifier.clone());
        drop(pending);
        values
    };
    let response = poll_upstream(&reqwest::Client::new(), POLL_URL, &uuid, &verifier).await;
    let mut pending = flows.0.lock().await;
    let flow = pending
        .get_mut(&input.id)
        .ok_or_else(|| error("Cursor login flow is invalid or expired"))?;
    if flow.created.elapsed() >= LIFETIME {
        pending.remove(&input.id);
        return Err(error("Cursor login flow expired"));
    }
    flow.in_flight = false;
    let Some(tokens) = response.map_err(error)? else {
        return Ok(Json(CursorPollResponse::Pending));
    };
    // Consume the flow before storing tokens, so a second poll cannot race the save.
    pending.remove(&input.id);
    drop(pending);
    let lock = credential_lock(&credential).await;
    let _guard = lock.lock().await;
    require_credential(&app.settings, &provider, &credential, "cursor").await?;
    auth::save(
        &app.settings,
        &provider,
        &credential,
        &Credentials::new(tokens.access_token, tokens.refresh_token),
    )
    .await
    .map_err(error)?;
    Ok(Json(CursorPollResponse::Complete))
}

/// Cursor login protocol tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{extract::Query, routing::get};
    /// Polling forwards PKCE parameters and handles pending and completed login.
    #[tokio::test]
    async fn poll_passes_pkce_and_accepts_pending_then_tokens() {
        let router = Router::new().route(
            "/",
            get(|Query(query): Query<HashMap<String, String>>| async move {
                assert_eq!(query.get("verifier").map(String::as_str), Some("secret"));
                if query.get("uuid").map(String::as_str) == Some("pending") {
                    (StatusCode::NOT_FOUND, Json(serde_json::json!({})))
                } else {
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({"accessToken":"access", "refreshToken":"refresh"})),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::new();
        assert!(
            poll_upstream(&client, &url, "pending", "secret")
                .await
                .unwrap()
                .is_none()
        );
        let tokens = poll_upstream(&client, &url, "complete", "secret")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token, "refresh");
        server.abort();
    }
}

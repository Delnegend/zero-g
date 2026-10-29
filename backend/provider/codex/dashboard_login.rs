use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::provider::codex::auth::{
    CODEX_CLIENT_ID, CODEX_DEVICE_AUTH_URL, CODEX_DEVICE_REDIRECT_URI, CODEX_DEVICE_TOKEN_URL,
    CODEX_DEVICE_USERCODE_URL, CodexAuthManager,
};
use crate::provider::login::{credential_lock, error, require_credential, same_origin};
use crate::server::AppState;

/// Shared collection of pending device authorization flows.
#[derive(Clone, Default)]
struct OAuthFlows {
    /// Flows indexed by their dashboard-facing identifier.
    pending: Arc<tokio::sync::Mutex<HashMap<String, Flow>>>,
}

/// State retained while a user authorizes a device.
struct Flow {
    /// Provider to which this flow belongs.
    provider_id: String,
    /// Credential to populate on successful authorization.
    credential_id: String,
    /// Time at which the flow was created.
    created: Instant,
    /// Device authorization identifier issued by `OpenAI`.
    device_id: String,
    /// Code the user enters on the authorization page.
    user_code: String,
    /// Minimum interval between upstream polls.
    interval: Duration,
    /// Earliest time another upstream poll may start.
    next_poll: Instant,
    /// Whether an upstream poll is currently running.
    in_flight: bool,
}

impl Flow {
    /// Checks that the flow belongs to the requested provider and credential.
    fn belongs_to(&self, provider: &str, credential: &str) -> bool {
        self.provider_id == provider && self.credential_id == credential
    }
}

/// Application state and pending flows shared by login handlers.
type LoginState = (AppState, OAuthFlows);

/// Builds the dashboard routes for Codex device authorization.
pub(crate) fn router(app: AppState) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/codex/start",
            post(codex_start),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/codex/poll",
            post(codex_poll),
        )
        .with_state((app, OAuthFlows::default()))
}

/// Response from the device authorization start endpoint.
#[derive(Deserialize)]
struct DeviceStart {
    /// Identifier used to poll the device authorization.
    device_auth_id: String,
    /// Code displayed to the user for authorization.
    user_code: String,
    /// Suggested polling interval, represented as a number or string.
    interval: Option<serde_json::Value>,
}

/// Successful device authorization flow creation.
#[derive(serde::Serialize, utoipa::ToSchema)]
struct CodexStartResponse {
    /// Device flow identifier.
    id: String,
    /// Authorization URL.
    url: &'static str,
    /// Code to enter on the authorization page.
    user_code: String,
    /// Minimum polling interval in seconds.
    interval: u64,
}

/// Device authorization polling result.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CodexPollResponse {
    /// Authorization has not completed.
    Pending,
    /// Credentials were saved.
    Complete,
}

/// Codex device authorization dashboard API.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(codex_start, codex_poll),
    components(schemas(CodexStartResponse, CodexPollInput, CodexPollResponse))
)]
pub(crate) struct Api;

/// Starts a device authorization flow for a Codex credential.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/codex/start", operation_id = "startCodexLogin", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 200, body = CodexStartResponse)))]
async fn codex_start(
    State((app, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<CodexStartResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider_id, &credential_id, "codex").await?;
    let response = reqwest::Client::new()
        .post(CODEX_DEVICE_USERCODE_URL)
        .json(&json!({"client_id": CODEX_CLIENT_ID}))
        .send()
        .await
        .map_err(|e| error(format!("Device authorization failed: {e}")))?;
    if !response.status().is_success() {
        return Err(error(format!(
            "Device authorization failed: {}",
            response.status()
        )));
    }
    let data: DeviceStart = response
        .json()
        .await
        .map_err(|e| error(format!("Invalid device response: {e}")))?;
    let seconds = data
        .interval
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(5)
        .max(1);
    let interval = Duration::from_secs(seconds);
    let id = Uuid::new_v4().to_string();
    let user_code = data.user_code.clone();
    let next_poll = Instant::now()
        .checked_add(interval)
        .ok_or_else(|| error("Device polling interval is too large"))?;
    let mut pending = flows.pending.lock().await;
    pending.retain(|_, flow| flow.created.elapsed() < Duration::from_secs(600));
    pending.insert(
        id.clone(),
        Flow {
            provider_id,
            credential_id,
            created: Instant::now(),
            device_id: data.device_auth_id,
            user_code: data.user_code,
            interval,
            next_poll,
            in_flight: false,
        },
    );
    drop(pending);
    Ok(Json(CodexStartResponse {
        id,
        url: CODEX_DEVICE_AUTH_URL,
        user_code,
        interval: seconds,
    }))
}

/// Dashboard request identifying a pending device flow.
#[derive(Deserialize, utoipa::ToSchema)]
struct CodexPollInput {
    /// Dashboard-facing flow identifier.
    id: String,
}
/// Response from the device authorization polling endpoint.
#[derive(Deserialize)]
struct DeviceToken {
    /// Authorization code to exchange after approval.
    authorization_code: Option<String>,
    /// PKCE verifier accompanying the authorization code.
    code_verifier: Option<String>,
}

/// Polls a pending device flow and saves credentials after authorization.
// Keep the device polling state transition and token exchange together.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/codex/poll", operation_id = "pollCodexLogin", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = CodexPollInput, responses((status = 200, body = CodexPollResponse)))]
#[allow(clippy::too_many_lines)]
async fn codex_poll(
    State((app, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<CodexPollInput>,
) -> Result<Json<CodexPollResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider_id, &credential_id, "codex").await?;
    let settings = &app.settings;
    let (device_id, user_code) = {
        let mut pending = flows.pending.lock().await;
        if !pending
            .get(&input.id)
            .is_some_and(|flow| flow.belongs_to(&provider_id, &credential_id))
        {
            return Err(error("Device flow is invalid or expired"));
        }
        let Some(Flow {
            created,
            device_id,
            user_code,
            interval,
            next_poll,
            in_flight,
            ..
        }) = pending.get_mut(&input.id)
        else {
            return Err(error("Device flow is invalid or expired"));
        };
        if created.elapsed() >= Duration::from_secs(600) {
            pending.remove(&input.id);
            return Err(error("Device flow expired"));
        }
        if *in_flight || Instant::now() < *next_poll {
            return Ok(Json(CodexPollResponse::Pending));
        }
        let next = Instant::now()
            .checked_add(*interval)
            .ok_or_else(|| error("Device polling interval is too large"))?;
        *in_flight = true;
        *next_poll = next;
        let credentials = (device_id.clone(), user_code.clone());
        drop(pending);
        credentials
    };
    let response = tokio::time::timeout(Duration::from_secs(30), async {
        reqwest::Client::new()
            .post(CODEX_DEVICE_TOKEN_URL)
            .json(&json!({"device_auth_id":device_id,"user_code":user_code}))
            .send()
            .await
    })
    .await;
    let response = match response {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => {
            if let Some(Flow { in_flight, .. }) = flows.pending.lock().await.get_mut(&input.id) {
                *in_flight = false;
            }
            return Err(error(format!("Device polling failed: {e}")));
        }
        Err(_) => {
            if let Some(Flow { in_flight, .. }) = flows.pending.lock().await.get_mut(&input.id) {
                *in_flight = false;
            }
            return Err(error("Device polling timed out"));
        }
    };
    if response.status() == StatusCode::FORBIDDEN || response.status() == StatusCode::NOT_FOUND {
        if let Some(Flow { in_flight, .. }) = flows.pending.lock().await.get_mut(&input.id) {
            *in_flight = false;
        }
        return Ok(Json(CodexPollResponse::Pending));
    }
    flows.pending.lock().await.remove(&input.id);
    if !response.status().is_success() {
        return Err(error(format!(
            "Device polling failed: {}",
            response.status()
        )));
    }
    let data: DeviceToken = response
        .json()
        .await
        .map_err(|e| error(format!("Invalid device token response: {e}")))?;
    let code = data
        .authorization_code
        .ok_or_else(|| error("Missing authorization code"))?;
    let verifier = data
        .code_verifier
        .ok_or_else(|| error("Missing code verifier"))?;
    let creds =
        crate::provider::codex::login::exchange_token(&code, &verifier, CODEX_DEVICE_REDIRECT_URI)
            .await
            .map_err(error)?;
    let lock = credential_lock(&credential_id).await;
    let _guard = lock.lock().await;
    CodexAuthManager::save(settings, &provider_id, &credential_id, &creds)
        .await
        .map_err(error)?;
    Ok(Json(CodexPollResponse::Complete))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_is_bound_to_provider_and_credential() {
        let flow = Flow {
            provider_id: "codex-one".into(),
            credential_id: "codex-key".into(),
            created: Instant::now(),
            device_id: "device".into(),
            user_code: "code".into(),
            interval: Duration::from_secs(5),
            next_poll: Instant::now(),
            in_flight: false,
        };
        assert!(flow.belongs_to("codex-one", "codex-key"));
        assert!(!flow.belongs_to("codex-two", "codex-key"));
        assert!(!flow.belongs_to("codex-one", "other-key"));
    }
}

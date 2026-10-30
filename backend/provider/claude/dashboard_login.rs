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

use super::auth::{self, CLIENT_ID, REDIRECT_URI, TOKEN_URL};
use crate::{
    provider::login::{credential_lock, error, require_credential, same_origin},
    server::AppState,
};

/// Shared pending OAuth flows.
#[derive(Clone, Default)]
struct Flows(
    /// Pending flows indexed by their opaque identifier.
    Arc<tokio::sync::Mutex<HashMap<String, Flow>>>,
);
/// A credential-bound authorization attempt.
struct Flow {
    /// Owning provider identifier.
    provider: String,
    /// Owning credential identifier.
    credential: String,
    /// Creation time used for expiration.
    created: Instant,
    /// PKCE verifier.
    verifier: String,
    /// OAuth state nonce.
    state: String,
    /// Whether the credential was saved.
    complete: bool,
    /// Whether token exchange is in progress.
    in_flight: bool,
}
/// Dashboard state shared by handlers.
type LoginState = (AppState, Flows);

/// Build the dashboard login routes.
pub(super) fn router(app: AppState) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/claude/start",
            post(start),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/claude/poll",
            post(poll),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/claude/finish",
            post(finish),
        )
        .with_state((app, Flows::default()))
}

/// Login start response.
#[derive(Serialize, utoipa::ToSchema)]
struct ClaudeStartResponse {
    /// Flow identifier.
    id: String,
    /// Browser authorization URL.
    url: String,
}
/// Flow identifier for polling.
#[derive(Deserialize, utoipa::ToSchema)]
struct ClaudePollInput {
    /// Flow identifier.
    id: String,
}
/// Manual callback submission.
#[derive(Deserialize, utoipa::ToSchema)]
struct ClaudeFinishInput {
    /// Flow identifier.
    id: String,
    /// Pasted code or callback URL.
    code: String,
}
/// Login completion state.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ClaudePollResponse {
    /// Authorization is pending.
    Pending,
    /// Credentials were saved.
    Complete,
}

/// Claude login `OpenAPI` components.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(start, poll, finish),
    components(schemas(
        ClaudeStartResponse,
        ClaudePollInput,
        ClaudeFinishInput,
        ClaudePollResponse
    ))
)]
pub(crate) struct Api;

/// Start an OAuth authorization flow.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/claude/start", operation_id = "startClaudeLogin", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 200, body = ClaudeStartResponse)))]
async fn start(
    State((app, flows)): State<LoginState>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<ClaudeStartResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider, &credential, "claude").await?;
    let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let mut url = reqwest::Url::parse("https://claude.ai/oauth/authorize")
        .map_err(|e| error(e.to_string()))?;
    url.query_pairs_mut().append_pair("code", "true").append_pair("client_id", CLIENT_ID).append_pair("response_type", "code").append_pair("redirect_uri", REDIRECT_URI).append_pair("scope", "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload").append_pair("code_challenge", &challenge).append_pair("code_challenge_method", "S256").append_pair("state", &state);
    let id = Uuid::new_v4().to_string();
    let mut pending = flows.0.lock().await;
    pending.retain(|_, f| f.created.elapsed() < Duration::from_secs(600));
    pending.insert(
        id.clone(),
        Flow {
            provider,
            credential,
            created: Instant::now(),
            verifier,
            state,
            complete: false,
            in_flight: false,
        },
    );
    drop(pending);
    Ok(Json(ClaudeStartResponse {
        id,
        url: url.into(),
    }))
}

/// Validate the ownership and expiration of a flow.
fn valid<'a>(
    flows: &'a mut HashMap<String, Flow>,
    id: &str,
    provider: &str,
    credential: &str,
) -> Result<&'a mut Flow, (StatusCode, String)> {
    flows
        .get_mut(id)
        .filter(|f| {
            f.provider == provider
                && f.credential == credential
                && f.created.elapsed() < Duration::from_secs(600)
        })
        .ok_or_else(|| error("Claude login flow is invalid or expired"))
}

/// Poll the completion state of a flow.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/claude/poll", operation_id = "pollClaudeLogin", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = ClaudePollInput, responses((status = 200, body = ClaudePollResponse)))]
async fn poll(
    State((app, flows)): State<LoginState>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<ClaudePollInput>,
) -> Result<Json<ClaudePollResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider, &credential, "claude").await?;
    let mut pending = flows.0.lock().await;
    let flow = valid(&mut pending, &input.id, &provider, &credential)?;
    Ok(Json(if flow.complete {
        ClaudePollResponse::Complete
    } else {
        ClaudePollResponse::Pending
    }))
}

/// Accept a raw authorization code, code#state, or the registered callback URL.
fn parse_code(input: &str, expected_state: &str) -> Result<String, (StatusCode, String)> {
    let input = input.trim();
    if let Ok(url) = reqwest::Url::parse(input) {
        if url.scheme() != "http"
            || url.host_str() != Some("localhost")
            || url.port() != Some(54545)
            || url.path() != "/callback"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(error("Invalid Claude callback URL"));
        }
        let pairs: Vec<_> = url.query_pairs().collect();
        let code = pairs
            .iter()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.as_ref())
            .ok_or_else(|| error("Missing authorization code"))?;
        let state = pairs
            .iter()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.as_ref())
            .ok_or_else(|| error("Missing OAuth state"))?;
        if state != expected_state || code.is_empty() {
            return Err(error("Invalid OAuth state or code"));
        }
        return Ok(code.to_owned());
    }
    let (code, state) = input
        .split_once('#')
        .map_or((input, None), |(c, s)| (c, Some(s)));
    if code.is_empty() || state.is_some_and(|s| s != expected_state) {
        return Err(error("Invalid OAuth state or code"));
    }
    Ok(code.to_owned())
}

/// Exchange a manually supplied callback and save credentials.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/claude/finish", operation_id = "finishClaudeLogin", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = ClaudeFinishInput, responses((status = 200, body = ClaudePollResponse)))]
async fn finish(
    State((app, flows)): State<LoginState>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<ClaudeFinishInput>,
) -> Result<Json<ClaudePollResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider, &credential, "claude").await?;
    let (verifier, state, code) = {
        let mut pending = flows.0.lock().await;
        let flow = valid(&mut pending, &input.id, &provider, &credential)?;
        if flow.complete || flow.in_flight {
            return Err(error("Claude login flow already used"));
        }
        let code = parse_code(&input.code, &flow.state)?;
        flow.in_flight = true;
        let values = (flow.verifier.clone(), flow.state.clone(), code);
        drop(pending);
        values
    };
    let result = async {
        let tokens = auth::exchange(TOKEN_URL, serde_json::json!({"grant_type":"authorization_code", "client_id":CLIENT_ID, "code":code, "redirect_uri":REDIRECT_URI, "code_verifier":verifier, "state":state}), false).await?;
        let creds = tokens.credentials(None)?;
        let lock = credential_lock(&credential).await;
        let _guard = lock.lock().await;
        {
            let mut pending = flows.0.lock().await;
            valid(&mut pending, &input.id, &provider, &credential).map_err(|(_, message)| message)?;
        }
        auth::save(&app.settings, &provider, &credential, &creds).await
    }.await;
    let mut pending = flows.0.lock().await;
    if let Some(flow) = pending.get_mut(&input.id) {
        flow.in_flight = false;
        if result.is_ok() {
            flow.complete = true;
        }
    }
    drop(pending);
    result.map_err(error)?;
    Ok(Json(ClaudePollResponse::Complete))
}

/// OAuth callback validation tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Reject callback URLs with incorrect origins or state.
    #[test]
    fn callback_must_match_registered_endpoint_and_state() {
        assert_eq!(
            parse_code(
                "http://localhost:54545/callback?code=abc&state=state",
                "state"
            )
            .unwrap(),
            "abc"
        );
        assert_eq!(parse_code("abc#state", "state").unwrap(), "abc");
        assert_eq!(parse_code("abc", "state").unwrap(), "abc");
        assert!(
            parse_code(
                "http://localhost:54545/callback?code=abc&state=wrong",
                "state"
            )
            .is_err()
        );
        assert!(
            parse_code(
                "http://example.com:54545/callback?code=abc&state=state",
                "state"
            )
            .is_err()
        );
    }
}

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

use super::auth::{self, AUTHORIZE_PATH, CLIENT_ID, REDIRECT_PATH, SCOPE};
use crate::{
    provider::login::{credential_lock, error, require_credential, same_origin},
    server::AppState,
};

/// How long a started sign-in stays valid.
const FLOW_TTL: Duration = Duration::from_secs(600);

/// Shared pending sign-in flows.
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
    /// Whether the code has been submitted.
    used: bool,
}
/// Dashboard state shared by handlers.
type LoginState = (AppState, Flows);

/// Build the dashboard login routes.
pub(super) fn router(app: AppState) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/codev/start",
            post(start),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/codev/finish",
            post(finish),
        )
        .with_state((app, Flows::default()))
}

/// Login start response.
#[derive(Serialize, utoipa::ToSchema)]
struct CodevStartResponse {
    /// Flow identifier.
    id: String,
    /// Browser authorization URL.
    url: String,
}
/// Manual callback submission.
#[derive(Deserialize, utoipa::ToSchema)]
struct CodevFinishInput {
    /// Flow identifier.
    id: String,
    /// Pasted code or callback URL.
    code: String,
}
/// Login completion state.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CodevFinishResponse {
    /// Credentials were saved.
    Complete,
}

/// `CoDev` login `OpenAPI` components.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(start, finish),
    components(schemas(CodevStartResponse, CodevFinishInput, CodevFinishResponse))
)]
pub(crate) struct Api;

/// Start an SSO authorization flow.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/codev/start", operation_id = "startCodevLogin", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 200, body = CodevStartResponse)))]
async fn start(
    State((app, flows)): State<LoginState>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<CodevStartResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider, &credential, "codev").await?;
    let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = Uuid::new_v4().to_string();
    let redirect = auth::url(REDIRECT_PATH).map_err(error)?;
    let mut url = reqwest::Url::parse(&auth::url(AUTHORIZE_PATH).map_err(error)?)
        .map_err(|e| error(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", SCOPE)
        .append_pair("state", &state)
        .append_pair("nonce", &Uuid::new_v4().to_string())
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let id = Uuid::new_v4().to_string();
    let mut pending = flows.0.lock().await;
    pending.retain(|_, f| f.created.elapsed() < FLOW_TTL);
    pending.insert(
        id.clone(),
        Flow {
            provider,
            credential,
            created: Instant::now(),
            verifier,
            state,
            used: false,
        },
    );
    drop(pending);
    Ok(Json(CodevStartResponse {
        id,
        url: url.into(),
    }))
}

/// Accept a raw authorization code or the hosted success-page URL.
fn parse_code(
    input: &str,
    redirect: &str,
    expected_state: &str,
) -> Result<String, (StatusCode, String)> {
    let input = input.trim();
    let Ok(url) = reqwest::Url::parse(input) else {
        if input.is_empty() {
            return Err(error("Missing authorization code"));
        }
        return Ok(input.to_owned());
    };
    let expected = reqwest::Url::parse(redirect).map_err(|e| error(e.to_string()))?;
    if url.origin() != expected.origin()
        || url.path() != expected.path()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error("Invalid CoDev callback URL"));
    }
    let query: HashMap<_, _> = url.query_pairs().collect();
    if let Some(problem) = query.get("error") {
        let detail = query.get("error_description").unwrap_or(problem);
        return Err(error(format!("CoDev SSO returned an error: {detail}")));
    }
    let code = query
        .get("code")
        .filter(|code| !code.is_empty())
        .ok_or_else(|| error("Missing authorization code"))?;
    if query
        .get("state")
        .is_some_and(|state| state != expected_state)
    {
        return Err(error(
            "That URL is from a different sign-in attempt; use the latest one",
        ));
    }
    Ok(code.to_string())
}

/// Exchange a pasted callback and save credentials.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/codev/finish", operation_id = "finishCodevLogin", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = CodevFinishInput, responses((status = 200, body = CodevFinishResponse)))]
async fn finish(
    State((app, flows)): State<LoginState>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<CodevFinishInput>,
) -> Result<Json<CodevFinishResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider, &credential, "codev").await?;
    let (verifier, code) = {
        let mut pending = flows.0.lock().await;
        let flow = pending
            .get_mut(&input.id)
            .filter(|f| {
                f.provider == provider
                    && f.credential == credential
                    && f.created.elapsed() < FLOW_TTL
            })
            .ok_or_else(|| error("CoDev sign-in is invalid or expired"))?;
        if flow.used {
            return Err(error("CoDev sign-in already used; start again"));
        }
        let redirect = auth::url(REDIRECT_PATH).map_err(error)?;
        let code = parse_code(&input.code, &redirect, &flow.state)?;
        flow.used = true;
        let verifier = flow.verifier.clone();
        drop(pending);
        (verifier, code)
    };
    let creds = auth::sign_in(&code, &verifier)
        .await
        .map_err(|e| error(e.message))?;
    let lock = credential_lock(&credential).await;
    let _guard = lock.lock().await;
    auth::save(&app.settings, &provider, &credential, &creds)
        .await
        .map_err(error)?;
    flows.0.lock().await.remove(&input.id);
    Ok(Json(CodevFinishResponse::Complete))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::parse_code;

    /// Only the hosted success page or a bare code is accepted, and state must match.
    #[test]
    fn callback_must_match_success_page_and_state() {
        let page = "https://netmind.example.com/codev/oauth/success";
        let parse = |input: &str| parse_code(input, page, "s");
        assert_eq!(parse(&format!("{page}?code=abc&state=s")).unwrap(), "abc");
        assert_eq!(parse(" abc ").unwrap(), "abc");
        assert!(parse(&format!("{page}?code=abc&state=other")).is_err());
        assert!(parse("https://evil.example/codev/oauth/success?code=abc").is_err());
        assert!(parse(&format!("{page}?error=access_denied")).is_err());
    }
}

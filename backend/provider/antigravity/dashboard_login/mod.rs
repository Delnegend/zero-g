use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Html;
use axum::routing::post;
use axum::{Json, Router};
use openidconnect::PkceCodeChallenge;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::provider::antigravity::auth as agy;
use crate::provider::login::{error, require_credential, same_origin};
use crate::server::AppState;
use crate::server::settings::SettingsStore;
use crate::util::urlencode;

mod exchange;
use exchange::{finish_google_exchange, parse_google_callback};

/// Completed flow results keyed by OAuth state, with their time and ownership.
type FlowResults = HashMap<String, (Instant, String, String, Result<(), String>)>;

/// Shared pending flows, completed results, and local listener lock.
#[derive(Clone)]
struct OAuthFlows {
    /// Active flows indexed by OAuth state.
    pending: Arc<tokio::sync::Mutex<HashMap<String, Flow>>>,
    /// Completed flows awaiting dashboard polling.
    results: Arc<tokio::sync::Mutex<FlowResults>>,
    /// Ensures only one flow owns the local Google callback listener.
    google_listener: Arc<tokio::sync::Mutex<()>>,
    /// Token exchange endpoint.
    token_url: String,
    /// Google user information endpoint.
    userinfo_url: String,
    /// Cloud Code Assist endpoint.
    cloud_url: String,
}

impl Default for OAuthFlows {
    fn default() -> Self {
        Self {
            pending: Arc::default(),
            results: Arc::default(),
            google_listener: Arc::default(),
            token_url: agy::OAUTH_TOKEN_URL.into(),
            userinfo_url: "https://www.googleapis.com/oauth2/v2/userinfo".into(),
            cloud_url: agy::CLOUD_CODE_ASSIST_ENDPOINT.into(),
        }
    }
}

/// State and ownership of a pending Google OAuth flow.
struct Flow {
    /// Provider that owns the flow.
    provider_id: String,
    /// Credential being authenticated.
    credential_id: String,
    /// Time at which the flow was started.
    created: Instant,
    /// Whether the callback exchange is already in progress.
    in_flight: bool,
    /// PKCE verifier tied to this flow.
    verifier: String,
    /// Signal for shutting down the local callback listener.
    shutdown: Arc<tokio::sync::Notify>,
}

impl Flow {
    /// Checks that the flow belongs to the requested provider and credential.
    fn belongs_to(&self, provider: &str, credential: &str) -> bool {
        self.provider_id == provider && self.credential_id == credential
    }
}

/// Application and OAuth flow state shared by the login routes.
type LoginState = (SettingsStore, OAuthFlows);

/// Builds the dashboard routes for Google OAuth login.
pub(crate) fn router(app: AppState) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/agy/start",
            post(agy_start),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/agy/finish",
            post(agy_finish),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/agy/poll",
            post(agy_poll),
        )
        .with_state((app.settings, OAuthFlows::default()))
}

/// Successful Google OAuth flow creation.
#[derive(Serialize, utoipa::ToSchema)]
struct AntigravityStartResponse {
    /// OAuth flow identifier.
    id: String,
    /// Authorization URL.
    url: String,
    /// Whether the local callback listener is active.
    local_callback: bool,
}

/// Google OAuth flow completion response.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum AntigravityFinishResponse {
    /// Credentials were saved.
    Complete,
}

/// Google OAuth polling result.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum AntigravityPollResponse {
    /// Authorization has not completed.
    Pending,
    /// Credentials were saved.
    Complete,
    /// Authorization failed.
    Failed {
        /// Error reported by the flow.
        error: String,
    },
}

/// Google OAuth dashboard API.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(agy_start, agy_finish, agy_poll),
    components(schemas(
        AntigravityStartResponse,
        AntigravityFinishResponse,
        AntigravityPollResponse,
        AntigravityFinishInput,
        AntigravityPollInput
    ))
)]
pub(crate) struct Api;

/// Starts a Google OAuth flow and optionally serves its local callback.
// Keep the OAuth listener setup and its cleanup together.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/agy/start", operation_id = "startAntigravityLogin", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 200, body = AntigravityStartResponse)))]
#[allow(clippy::too_many_lines)]
async fn agy_start(
    State((settings, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<AntigravityStartResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&settings, &provider_id, &credential_id, "agy").await?;
    let listener_guard = flows.google_listener.clone().try_lock_owned().ok();
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let state = Uuid::new_v4().to_string();
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut pending = flows.pending.lock().await;
    pending.retain(|_, flow| flow.created.elapsed() < Duration::from_secs(600));
    pending.insert(
        state.clone(),
        Flow {
            provider_id: provider_id.clone(),
            credential_id: credential_id.clone(),
            created: Instant::now(),
            in_flight: false,
            verifier: verifier.secret().to_owned(),
            shutdown: shutdown.clone(),
        },
    );
    drop(pending);
    let listener = if listener_guard.is_some() {
        tokio::net::TcpListener::bind(("127.0.0.1", agy::OAUTH_CALLBACK_PORT))
            .await
            .ok()
    } else {
        None
    };
    let local_callback = listener.is_some();
    if let Some(listener) = listener {
        let callback_settings = settings.clone();
        let callback_shutdown = shutdown.clone();
        let callback_flows = flows.clone();
        let flow_id = state.clone();
        let callback_provider_id = provider_id.clone();
        let callback_credential_id = credential_id.clone();
        tokio::spawn(async move {
            let _listener_guard = listener_guard;
            let handler = axum::Router::new().route(
                agy::OAUTH_CALLBACK_PATH,
                axum::routing::get(move |Query(query): Query<HashMap<String, String>>| {
                    let settings = callback_settings.clone();
                    let flows = callback_flows.clone();
                    let shutdown = callback_shutdown.clone();
                    let flow_id = flow_id.clone();
                    let provider_id = callback_provider_id.clone();
                    let credential_id = callback_credential_id.clone();
                    async move {
                        if query.get("state").map(String::as_str) != Some(flow_id.as_str()) {
                            return Html("Invalid OAuth state. Return to the dashboard.");
                        }
                        let Ok(mut callback) = reqwest::Url::parse(agy::OAUTH_CALLBACK_URL) else {
                            return Html("Invalid callback URL.");
                        };
                        callback.query_pairs_mut().extend_pairs(query.iter());
                        let mut headers = HeaderMap::new();
                        headers.insert(
                            header::ORIGIN,
                            axum::http::HeaderValue::from_static("http://127.0.0.1:51123"),
                        );
                        headers.insert(
                            header::HOST,
                            axum::http::HeaderValue::from_static("127.0.0.1:51123"),
                        );
                        let result = agy_finish(
                            State((settings, flows.clone())),
                            Path((provider_id, credential_id)),
                            headers,
                            Json(AntigravityFinishInput {
                                callback_url: callback.to_string(),
                            }),
                        )
                        .await
                        .map(|_| ())
                        .map_err(|(_, message)| message);
                        let success = result.is_ok();
                        shutdown.notify_one();
                        if success {
                            Html("Authentication complete. Return to the dashboard.")
                        } else {
                            Html("Authentication failed. Return to the dashboard for details.")
                        }
                    }
                }),
            );
            let _ = tokio::time::timeout(Duration::from_secs(600), async move {
                axum::serve(listener, handler)
                    .with_graceful_shutdown(async move { shutdown.notified().await })
                    .await
            })
            .await;
        });
    } else {
        drop(listener_guard);
    }
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&access_type=offline&prompt=consent&code_challenge={}&code_challenge_method=S256",
        urlencode(agy::OAUTH_CLIENT_ID),
        urlencode(agy::OAUTH_CALLBACK_URL),
        urlencode(&agy::OAUTH_SCOPES.join(" ")),
        urlencode(&state),
        urlencode(challenge.as_str())
    );
    Ok(Json(AntigravityStartResponse {
        id: state,
        url,
        local_callback,
    }))
}

/// Dashboard request containing the Google redirect URL.
#[derive(Deserialize, utoipa::ToSchema)]
struct AntigravityFinishInput {
    /// Full callback URL containing the OAuth state and authorization code.
    callback_url: String,
}

/// Validates and completes a pending Google OAuth callback.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/agy/finish", operation_id = "finishAntigravityLogin", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = AntigravityFinishInput, responses((status = 200, body = AntigravityFinishResponse)))]
async fn agy_finish(
    State((settings, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<AntigravityFinishInput>,
) -> Result<Json<AntigravityFinishResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&settings, &provider_id, &credential_id, "agy").await?;
    let (state, code) = parse_google_callback(&input.callback_url)?;
    let verifier = {
        let mut pending = flows.pending.lock().await;
        if !pending
            .get(&state)
            .is_some_and(|flow| flow.belongs_to(&provider_id, &credential_id))
        {
            return Err(error("OAuth state is invalid or expired"));
        }
        match pending.get_mut(&state) {
            Some(Flow {
                created,
                in_flight,
                verifier,
                ..
            }) if created.elapsed() < Duration::from_secs(600) && !*in_flight => {
                *in_flight = true;
                verifier.clone()
            }
            _ => return Err(error("OAuth state is invalid or expired")),
        }
    };
    let result = finish_google_exchange(
        &settings,
        &flows,
        &provider_id,
        &credential_id,
        &code,
        &verifier,
    )
    .await;
    complete_google(
        &flows,
        &state,
        result
            .as_ref()
            .map(|_| ())
            .map_err(|(_, message)| message.clone()),
    )
    .await;
    result
}

/// Moves a pending flow to completed results and signals listener shutdown.
async fn complete_google(flows: &OAuthFlows, state: &str, result: Result<(), String>) {
    let mut pending = flows.pending.lock().await;
    if let Some(Flow {
        provider_id,
        credential_id,
        shutdown,
        ..
    }) = pending.remove(state)
    {
        flows.results.lock().await.insert(
            state.to_string(),
            (Instant::now(), provider_id, credential_id, result),
        );
        shutdown.notify_one();
    }
}

/// Dashboard request to poll a Google OAuth flow.
#[derive(Deserialize, utoipa::ToSchema)]
struct AntigravityPollInput {
    /// OAuth state identifying the flow.
    id: String,
}

/// Returns pending status or consumes the completed result for a flow.
#[utoipa::path(post, path = "/api/providers/{id}/credentials/{credential_id}/agy/poll", operation_id = "pollAntigravityLogin", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = AntigravityPollInput, responses((status = 200, body = AntigravityPollResponse)))]
async fn agy_poll(
    State((settings, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<AntigravityPollInput>,
) -> Result<Json<AntigravityPollResponse>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&settings, &provider_id, &credential_id, "agy").await?;
    let pending = flows.pending.lock().await;
    let mut results = flows.results.lock().await;
    results.retain(|_, (created, _, _, _)| created.elapsed() < Duration::from_secs(600));
    if results
        .get(&input.id)
        .is_some_and(|(_, id, credential, _)| id == &provider_id && credential == &credential_id)
    {
        let Some((_, _, _, result)) = results.remove(&input.id) else {
            return Err(error("OAuth flow is invalid or expired"));
        };
        return Ok(Json(match result {
            Ok(()) => AntigravityPollResponse::Complete,
            Err(error) => AntigravityPollResponse::Failed { error },
        }));
    }
    drop(results);
    match pending.get(&input.id) {
        Some(Flow {
            provider_id: id,
            credential_id: credential,
            created,
            ..
        }) if id == &provider_id
            && credential == &credential_id
            && created.elapsed() < Duration::from_secs(600) =>
        {
            Ok(Json(AntigravityPollResponse::Pending))
        }
        _ => Err(error("OAuth flow is invalid or expired")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;

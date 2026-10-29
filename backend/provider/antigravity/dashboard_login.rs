use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Html;
use axum::routing::post;
use axum::{Json, Router};
use openidconnect::PkceCodeChallenge;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use uuid::Uuid;

use crate::provider::antigravity::auth::{self as agy, AuthManager, Credentials};
use crate::provider::login::{credential_lock, error, require_credential, same_origin};
use crate::server::AppState;
use crate::server::settings::SettingsStore;
use crate::util::urlencode;

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
    let client_id = agy::oauth_client_id(&settings).await.map_err(error)?;
    // Check configuration before starting a flow.
    agy::oauth_client_secret(&settings).await.map_err(error)?;
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
        urlencode(&client_id),
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

/// Token fields returned by Google's authorization-code exchange.
#[derive(Deserialize)]
struct GoogleToken {
    /// Google OAuth access token.
    access_token: String,
    /// Optional token for refreshing access.
    refresh_token: Option<String>,
    /// Access-token lifetime in seconds.
    expires_in: i64,
}

/// Validates the local redirect URL and extracts its state and authorization code.
fn parse_google_callback(callback_url: &str) -> Result<(String, String), (StatusCode, String)> {
    let url = reqwest::Url::parse(callback_url).map_err(|_| error("Invalid callback URL"))?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port() != Some(agy::OAUTH_CALLBACK_PORT)
        || url.path() != agy::OAUTH_CALLBACK_PATH
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(error("Callback URL does not match the expected redirect"));
    }
    let mut state = None;
    let mut code = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "state" if state.is_none() => state = Some(value.into_owned()),
            "code" if code.is_none() => code = Some(value.into_owned()),
            "error" => return Err(error("Google authorization was denied")),
            "state" | "code" => return Err(error("Duplicate callback parameter")),
            _ => {}
        }
    }
    let state = state
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error("Missing OAuth state"))?;
    let code = code
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error("Missing authorization code"))?;
    Ok((state, code))
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

/// Exchanges the code, discovers the project, and stores Google credentials.
async fn finish_google_exchange(
    settings: &SettingsStore,
    flows: &OAuthFlows,
    provider_id: &str,
    credential_id: &str,
    code: &str,
    verifier: &str,
) -> Result<Json<AntigravityFinishResponse>, (StatusCode, String)> {
    let client_id = agy::oauth_client_id(settings).await.map_err(error)?;
    let client_secret = agy::oauth_client_secret(settings).await.map_err(error)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| error(format!("Failed to create OAuth client: {e}")))?;
    let response = client
        .post(&flows.token_url)
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", agy::OAUTH_CALLBACK_URL),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|e| error(format!("Token request failed: {e}")))?;
    if !response.status().is_success() {
        return Err(error(format!(
            "Google token exchange failed: {}",
            response.status()
        )));
    }
    let token: GoogleToken = response
        .json()
        .await
        .map_err(|e| error(format!("Invalid token response: {e}")))?;
    if token.access_token.trim().is_empty()
        || token
            .refresh_token
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        || token.expires_in <= 0
    {
        return Err(error(
            "Google token response is missing valid offline credentials",
        ));
    }
    let email = match client
        .get(&flows.userinfo_url)
        .bearer_auth(&token.access_token)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("email").and_then(|v| v.as_str()).map(str::to_string)),
        _ => None,
    };
    let project_id = crate::provider::antigravity::login::fetch_or_onboard_project_at(
        &client,
        &token.access_token,
        &flows.cloud_url,
    )
    .await
    .map_err(error)?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let expires_at = token
        .expires_in
        .checked_mul(1000)
        .and_then(|duration| now_ms.checked_add(duration))
        .ok_or_else(|| error("Token expiration is out of range"))?;
    let creds = Credentials {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        project_id,
        expires_at: Some(expires_at),
        email,
    };
    let lock = credential_lock(credential_id).await;
    let _guard = lock.lock().await;
    AuthManager::save(settings, provider_id, credential_id, &creds)
        .await
        .map_err(error)?;
    Ok(Json(AntigravityFinishResponse::Complete))
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
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
        routing::put,
    };
    use openidconnect::PkceCodeVerifier;
    use tower::ServiceExt;

    /// Sends a request to the dashboard routes.
    async fn request(
        router: &Router,
        method: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("host", "dashboard.test")
                    .header("origin", "http://dashboard.test")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"error":String::from_utf8_lossy(&bytes)}));
        (status, value)
    }

    /// Verifies the OAuth wire protocol and failed onboarding through dashboard routes.
    #[tokio::test]
    #[allow(clippy::too_many_lines, clippy::literal_string_with_formatting_args)]
    async fn oauth_pkce_project_and_expiry() {
        use crate::{
            database::Database,
            provider::{credentials, registry},
        };
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let settings = Router::new()
            .route("/api/settings", put(crate::server::settings::put_settings))
            .with_state(store.clone());
        assert_eq!(
            request(
                &settings,
                "PUT",
                "/api/settings",
                json!({"oauth_client_id":"custom-client","oauth_client_secret":"custom-secret"})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            request(
                &registry::router(store.clone()),
                "POST",
                "/api/providers",
                json!({"kind":"agy"})
            )
            .await
            .0,
            StatusCode::CREATED
        );
        let (status, created) = request(
            &credentials::router(store.clone()),
            "POST",
            "/api/providers/agy/credentials",
            json!({"name":"primary","weight":1,"enabled":true}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = created["id"].as_str().unwrap();
        let old = Credentials {
            access_token: "old-access".into(),
            refresh_token: Some("old-refresh".into()),
            project_id: "old-project".into(),
            expires_at: None,
            email: Some("old@example.test".into()),
        };
        AuthManager::save(&store, "agy", id, &old).await.unwrap();
        let challenge = Arc::new(tokio::sync::Mutex::new(String::new()));
        let project_exists = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let onboarding_error = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fake = Router::new()
            .route("/token", post({
                let challenge = challenge.clone();
                move |axum::Form(form): axum::Form<HashMap<String, String>>| {
                    let challenge = challenge.clone();
                    async move {
                        let verifier = PkceCodeVerifier::new(form.get("code_verifier").cloned().unwrap_or_default());
                        let valid = verifier.secret().len() >= 43 && verifier.secret().len() <= 128
                            && PkceCodeChallenge::from_code_verifier_sha256(&verifier).as_str() == *challenge.lock().await
                            && form.get("client_id").is_some_and(|v| v == "custom-client")
                            && form.get("client_secret").is_some_and(|v| v == "custom-secret")
                            && form.get("code").is_some_and(|v| v == "authorized-code")
                            && form.get("redirect_uri").is_some_and(|v| v == agy::OAUTH_CALLBACK_URL);
                        if valid { (StatusCode::OK, Json(json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}))) }
                        else { (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))) }
                    }
                }
            }))
            .route("/userinfo", axum::routing::get(|| async { Json(json!({"email":"user@example.test"})) }))
            .route("/v1internal:loadCodeAssist", post({
                let project_exists = project_exists.clone();
                move || {
                    let project_exists = project_exists.clone();
                    async move {
                        if project_exists.load(std::sync::atomic::Ordering::SeqCst) { Json(json!({"cloudaicompanionProject":"real-project"})) }
                        else { Json(json!({"currentTier":{"name":"free-tier"}})) }
                    }
                }
            }))
            .route("/v1internal:onboardUser", post({
                let onboarding_error = onboarding_error.clone();
                move || {
                    let onboarding_error = onboarding_error.clone();
                    async move {
                        if onboarding_error.load(std::sync::atomic::Ordering::SeqCst) {
                            Json(json!({"done":true,"error":{"message":"account ineligible"}}))
                        } else {
                            Json(json!({"done":true}))
                        }
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });
        let flows = OAuthFlows {
            token_url: format!("{base}/token"),
            userinfo_url: format!("{base}/userinfo"),
            cloud_url: base,
            ..OAuthFlows::default()
        };
        let _listener_guard = flows.google_listener.lock().await;
        let router = Router::new()
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
            .with_state((store.clone(), flows.clone()));
        let start = format!("/api/providers/agy/credentials/{id}/agy/start");
        let finish = format!("/api/providers/agy/credentials/{id}/agy/finish");
        let poll = format!("/api/providers/agy/credentials/{id}/agy/poll");
        let (status, flow) = request(&router, "POST", &start, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        let state = flow["id"].as_str().unwrap();
        let url = reqwest::Url::parse(flow["url"].as_str().unwrap()).unwrap();
        let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(params.get("access_type").unwrap(), "offline");
        assert!(
            params
                .get("scope")
                .unwrap()
                .contains("https://www.googleapis.com/auth/cloud-platform")
        );
        *challenge.lock().await = params.get("code_challenge").unwrap().clone();
        assert_eq!(
            request(&router, "POST", &poll, json!({"id":state})).await.1["status"],
            "pending"
        );
        let callback =
            format!("http://127.0.0.1:51123/oauth-callback?state={state}&code=authorized-code");
        let wrong_state = "http://127.0.0.1:51123/oauth-callback?state=wrong&code=authorized-code";
        assert_ne!(
            request(
                &router,
                "POST",
                &finish,
                json!({"callback_url":wrong_state})
            )
            .await
            .0,
            StatusCode::OK
        );
        let cross_origin = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&finish)
                    .header("content-type", "application/json")
                    .header("host", "dashboard.test")
                    .header("origin", "http://attacker.test")
                    .body(Body::from(json!({"callback_url":callback}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(cross_origin.status(), StatusCode::OK);
        assert_ne!(
            request(&router, "POST", &finish, json!({"callback_url":callback}))
                .await
                .0,
            StatusCode::OK
        );
        let (_, failed) = request(&router, "POST", &poll, json!({"id":state})).await;
        assert_eq!(failed["status"], "failed");
        assert!(failed["error"].as_str().unwrap().contains("project ID"));
        let stored = AuthManager::load(store.clone(), "agy", id)
            .await
            .unwrap()
            .unwrap()
            .get_credentials()
            .await
            .unwrap();
        assert_eq!(stored.access_token, "old-access");
        assert_eq!(stored.refresh_token.as_deref(), Some("old-refresh"));
        assert_eq!(stored.project_id, "old-project");
        onboarding_error.store(true, std::sync::atomic::Ordering::SeqCst);
        let (_, error_flow) = request(&router, "POST", &start, json!({})).await;
        let error_state = error_flow["id"].as_str().unwrap();
        let url = reqwest::Url::parse(error_flow["url"].as_str().unwrap()).unwrap();
        let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
        *challenge.lock().await = params.get("code_challenge").unwrap().clone();
        let callback = format!(
            "http://127.0.0.1:51123/oauth-callback?state={error_state}&code=authorized-code"
        );
        assert_ne!(
            request(&router, "POST", &finish, json!({"callback_url":callback}))
                .await
                .0,
            StatusCode::OK
        );
        let (_, failed) = request(&router, "POST", &poll, json!({"id":error_state})).await;
        assert_eq!(failed["status"], "failed");
        assert!(
            failed["error"]
                .as_str()
                .unwrap()
                .contains("account ineligible")
        );
        onboarding_error.store(false, std::sync::atomic::Ordering::SeqCst);
        project_exists.store(true, std::sync::atomic::Ordering::SeqCst);
        let (_, flow) = request(&router, "POST", &start, json!({})).await;
        let state = flow["id"].as_str().unwrap();
        let url = reqwest::Url::parse(flow["url"].as_str().unwrap()).unwrap();
        let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
        *challenge.lock().await = params.get("code_challenge").unwrap().clone();
        let callback =
            format!("http://127.0.0.1:51123/oauth-callback?state={state}&code=authorized-code");
        assert_eq!(
            request(&router, "POST", &finish, json!({"callback_url":callback}))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            request(&router, "POST", &poll, json!({"id":state})).await.1["status"],
            "complete"
        );
        assert_ne!(
            request(&router, "POST", &finish, json!({"callback_url":callback}))
                .await
                .0,
            StatusCode::OK
        );
        let stored = AuthManager::load(store.clone(), "agy", id)
            .await
            .unwrap()
            .unwrap()
            .get_credentials()
            .await
            .unwrap();
        assert_eq!(stored.access_token, "new-access");
        assert_eq!(stored.project_id, "real-project");
        let (_, flow) = request(&router, "POST", &start, json!({})).await;
        let state = flow["id"].as_str().unwrap();
        flows.pending.lock().await.get_mut(state).unwrap().created = Instant::now()
            .checked_sub(Duration::from_secs(601))
            .unwrap();
        let callback =
            format!("http://127.0.0.1:51123/oauth-callback?state={state}&code=authorized-code");
        assert_ne!(
            request(&router, "POST", &finish, json!({"callback_url":callback}))
                .await
                .0,
            StatusCode::OK
        );
        assert_ne!(
            request(&router, "POST", &poll, json!({"id":state})).await.0,
            StatusCode::OK
        );
        server.abort();
    }

    #[test]
    fn flow_is_bound_to_provider_and_credential() {
        let flow = Flow {
            provider_id: "google-one".into(),
            credential_id: "google-key".into(),
            created: Instant::now(),
            in_flight: false,
            verifier: "test".into(),
            shutdown: Arc::new(tokio::sync::Notify::new()),
        };
        assert!(flow.belongs_to("google-one", "google-key"));
        assert!(!flow.belongs_to("google-two", "google-key"));
        assert!(!flow.belongs_to("google-one", "other-key"));
    }

    #[test]
    fn callback_rejects_wrong_redirect_and_duplicate_state() {
        assert_eq!(
            parse_google_callback("http://127.0.0.1:51123/oauth-callback?state=abc&code=x%2By"),
            Ok(("abc".to_string(), "x+y".to_string()))
        );
        assert!(
            parse_google_callback("http://localhost:51123/oauth-callback?state=a&code=b").is_err()
        );
        assert!(
            parse_google_callback("http://127.0.0.1:51123/oauth-callback?state=a&state=b&code=c")
                .is_err()
        );
        assert!(
            parse_google_callback("http://127.0.0.1:51123/oauth-callback?state=&code=c").is_err()
        );
    }

    #[tokio::test]
    async fn simultaneous_google_flows_keep_results_bound_to_credentials() {
        let flows = OAuthFlows::default();
        for id in ["success", "failure"] {
            flows.pending.lock().await.insert(
                id.into(),
                Flow {
                    provider_id: "account".into(),
                    credential_id: id.into(),
                    created: Instant::now(),
                    in_flight: true,
                    verifier: "test".into(),
                    shutdown: Arc::new(tokio::sync::Notify::new()),
                },
            );
        }
        complete_google(&flows, "success", Ok(())).await;
        assert!(flows.pending.lock().await.contains_key("failure"));
        complete_google(&flows, "failure", Err("exchange failed".into())).await;
        let results = flows.results.lock().await;
        let (_, provider, credential, result) = results.get("success").unwrap();
        assert_eq!(
            (provider.as_str(), credential.as_str()),
            ("account", "success")
        );
        assert!(result.is_ok());
        let (_, provider, credential, result) = results.get("failure").unwrap();
        assert_eq!(
            (provider.as_str(), credential.as_str()),
            ("account", "failure")
        );
        assert_eq!(result.as_ref().unwrap_err(), "exchange failed");
        drop(results);
    }

    #[tokio::test]
    async fn google_listener_has_a_single_local_callback_owner() {
        let flows = OAuthFlows::default();
        let guard = flows.google_listener.clone().try_lock_owned();
        assert!(guard.is_ok());
        assert!(flows.google_listener.clone().try_lock_owned().is_err());
        drop(guard);
        assert!(flows.google_listener.try_lock_owned().is_ok());
    }
}

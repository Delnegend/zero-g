use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Html;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::provider::antigravity::auth::{self as agy, AuthManager, Credentials};
use crate::provider::login::{credential_lock, error, require_credential, same_origin};
use crate::server::AppState;
use crate::util::urlencode;

/// Completed flow results keyed by OAuth state, with their time and ownership.
type FlowResults = HashMap<String, (Instant, String, String, Result<(), String>)>;

/// Shared pending flows, completed results, and local listener lock.
#[derive(Clone, Default)]
struct OAuthFlows {
    /// Active flows indexed by OAuth state.
    pending: Arc<tokio::sync::Mutex<HashMap<String, Flow>>>,
    /// Completed flows awaiting dashboard polling.
    results: Arc<tokio::sync::Mutex<FlowResults>>,
    /// Ensures only one flow owns the local Google callback listener.
    google_listener: Arc<tokio::sync::Mutex<()>>,
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
type LoginState = (AppState, OAuthFlows);

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
        .with_state((app, OAuthFlows::default()))
}

/// Starts a Google OAuth flow and optionally serves its local callback.
// Keep the OAuth listener setup and its cleanup together.
#[allow(clippy::too_many_lines)]
async fn agy_start(
    State((app, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider_id, &credential_id, "agy").await?;
    let settings = &app.settings;
    let client_id = agy::oauth_client_id(settings).await.map_err(error)?;
    // Check configuration before starting a flow.
    agy::oauth_client_secret(settings).await.map_err(error)?;
    let listener_guard = flows.google_listener.clone().try_lock_owned().ok();
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let state = Uuid::new_v4().to_string();
    let mut pending = flows.pending.lock().await;
    pending.retain(|_, flow| flow.created.elapsed() < Duration::from_secs(600));
    pending.insert(
        state.clone(),
        Flow {
            provider_id: provider_id.clone(),
            credential_id: credential_id.clone(),
            created: Instant::now(),
            in_flight: false,
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
        let callback_app = app.clone();
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
                    let app = callback_app.clone();
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
                            State((app, flows.clone())),
                            Path((provider_id, credential_id)),
                            headers,
                            Json(GoogleFinish {
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
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&access_type=offline&prompt=consent",
        urlencode(&client_id),
        urlencode(agy::OAUTH_CALLBACK_URL),
        urlencode(&agy::OAUTH_SCOPES.join(" ")),
        urlencode(&state)
    );
    Ok(Json(
        json!({"id": state, "url": url, "local_callback": local_callback}),
    ))
}

/// Dashboard request containing the Google redirect URL.
#[derive(Deserialize)]
struct GoogleFinish {
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
async fn agy_finish(
    State((app, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<GoogleFinish>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider_id, &credential_id, "agy").await?;
    let (state, code) = parse_google_callback(&input.callback_url)?;
    {
        let mut pending = flows.pending.lock().await;
        if !pending
            .get(&state)
            .is_some_and(|flow| flow.belongs_to(&provider_id, &credential_id))
        {
            return Err(error("OAuth state is invalid or expired"));
        }
        match pending.get_mut(&state) {
            Some(Flow {
                created, in_flight, ..
            }) if created.elapsed() < Duration::from_secs(600) && !*in_flight => {
                *in_flight = true;
            }
            _ => return Err(error("OAuth state is invalid or expired")),
        }
    }
    let result = finish_google_exchange(&app, &provider_id, &credential_id, &code).await;
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
    app: &AppState,
    provider_id: &str,
    credential_id: &str,
    code: &str,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let settings = &app.settings;
    let client_id = agy::oauth_client_id(settings).await.map_err(error)?;
    let client_secret = agy::oauth_client_secret(settings).await.map_err(error)?;
    let client = reqwest::Client::new();
    let response = client
        .post(agy::OAUTH_TOKEN_URL)
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", agy::OAUTH_CALLBACK_URL),
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
    let email = match client
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
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
    let project_id = crate::provider::antigravity::login::fetch_or_onboard_project(
        &client,
        &token.access_token,
        email.as_deref(),
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
    Ok(Json(json!({"status":"complete"})))
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
#[derive(Deserialize)]
struct GooglePoll {
    /// OAuth state identifying the flow.
    id: String,
}

/// Returns pending status or consumes the completed result for a flow.
async fn agy_poll(
    State((app, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<GooglePoll>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    same_origin(&headers)?;
    require_credential(&app.settings, &provider_id, &credential_id, "agy").await?;
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
            Ok(()) => json!({"status":"complete"}),
            Err(message) => json!({"status":"failed", "error":message}),
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
            Ok(Json(json!({"status":"pending"})))
        }
        _ => Err(error("OAuth flow is invalid or expired")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn flow_is_bound_to_provider_and_credential() {
        let flow = Flow {
            provider_id: "google-one".into(),
            credential_id: "google-key".into(),
            created: Instant::now(),
            in_flight: false,
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

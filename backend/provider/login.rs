use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Html;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::provider::antigravity::auth::{self as agy, AuthManager, Credentials};
use crate::provider::codex::auth::{
    CODEX_CLIENT_ID, CODEX_DEVICE_AUTH_URL, CODEX_DEVICE_REDIRECT_URI, CODEX_DEVICE_TOKEN_URL,
    CODEX_DEVICE_USERCODE_URL, CodexAuthManager,
};
use crate::server::AppState;
use crate::util::urlencode;

type FlowResults = HashMap<String, (Instant, String, String, Result<(), String>)>;

#[derive(Debug)]
pub(crate) struct CredentialError {
    pub(crate) message: String,
    pub(crate) upstream_status: Option<reqwest::StatusCode>,
}

impl CredentialError {
    pub(crate) const fn upstream(status: reqwest::StatusCode, message: String) -> Self {
        Self {
            message,
            upstream_status: Some(status),
        }
    }
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CredentialError {}

impl From<String> for CredentialError {
    fn from(message: String) -> Self {
        Self {
            message,
            upstream_status: None,
        }
    }
}

static REFRESH_LOCKS: LazyLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) async fn credential_lock(id: &str) -> Arc<Mutex<()>> {
    let mut locks = REFRESH_LOCKS.lock().await;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(id.to_string(), Arc::downgrade(&lock));
    lock
}

#[derive(Clone, Default)]
struct OAuthFlows {
    pending: Arc<Mutex<HashMap<String, Flow>>>,
    results: Arc<Mutex<FlowResults>>,
    google_listener: Arc<Mutex<()>>,
}

enum Flow {
    Google {
        provider_id: String,
        credential_id: String,
        created: Instant,
        in_flight: bool,
        shutdown: Arc<tokio::sync::Notify>,
    },
    Codex {
        provider_id: String,
        credential_id: String,
        created: Instant,
        device_id: String,
        user_code: String,
        interval: Duration,
        next_poll: Instant,
        in_flight: bool,
    },
}

impl Flow {
    fn belongs_to(&self, provider: &str, credential: &str) -> bool {
        match self {
            Self::Google {
                provider_id,
                credential_id,
                ..
            }
            | Self::Codex {
                provider_id,
                credential_id,
                ..
            } => provider_id == provider && credential_id == credential,
        }
    }
}

type LoginState = (AppState, OAuthFlows);

pub(crate) fn router(app: AppState) -> Router {
    let state = (app, OAuthFlows::default());
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
        .route(
            "/api/providers/{id}/credentials/{credential_id}/codex/start",
            post(codex_start),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/codex/poll",
            post(codex_poll),
        )
        .with_state(state)
}

fn same_origin(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| (StatusCode::FORBIDDEN, "Origin required".into()))?;
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| (StatusCode::FORBIDDEN, "Host required".into()))?;
    let url = reqwest::Url::parse(origin)
        .map_err(|_| (StatusCode::FORBIDDEN, "Invalid origin".into()))?;
    let host_url = reqwest::Url::parse(&format!("{}://{host}/", url.scheme()))
        .map_err(|_| (StatusCode::FORBIDDEN, "Invalid host".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host() != host_url.host()
        || url.port_or_known_default() != host_url.port_or_known_default()
        || !url.username().is_empty()
        || url.password().is_some()
        || !host_url.username().is_empty()
        || host_url.password().is_some()
        || host_url.path() != "/"
        || host_url.query().is_some()
        || host_url.fragment().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err((StatusCode::FORBIDDEN, "Origin mismatch".into()));
    }
    Ok(())
}

fn error(message: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, message.into())
}

pub(crate) async fn require_provider(
    store: &crate::server::settings::SettingsStore,
    id: &str,
    kind: &str,
) -> Result<(), (StatusCode, String)> {
    let provider = crate::provider::registry::get(&store.database, id)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load provider".into(),
            )
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Provider not found".into()))?;
    if provider.kind != kind {
        return Err(error("Provider kind does not match login"));
    }
    Ok(())
}

pub(crate) async fn require_credential(
    store: &crate::server::settings::SettingsStore,
    provider_id: &str,
    credential_id: &str,
    kind: &str,
) -> Result<(), (StatusCode, String)> {
    require_provider(store, provider_id, kind).await?;
    let credential = crate::provider::credentials::get(store, provider_id, credential_id)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load credential".into(),
            )
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Credential not found".into()))?;
    if !credential.enabled {
        return Err(error("Credential is disabled"));
    }
    Ok(())
}

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
    pending.retain(|_, flow| match flow {
        Flow::Google { created, .. } | Flow::Codex { created, .. } => {
            created.elapsed() < Duration::from_secs(600)
        }
    });
    pending.insert(
        state.clone(),
        Flow::Google {
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

#[derive(Deserialize)]
struct GoogleFinish {
    callback_url: String,
}

#[derive(Deserialize)]
struct GoogleToken {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
}

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
            Some(Flow::Google {
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

async fn complete_google(flows: &OAuthFlows, state: &str, result: Result<(), String>) {
    let mut pending = flows.pending.lock().await;
    if let Some(Flow::Google {
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

#[derive(Deserialize)]
struct GooglePoll {
    id: String,
}

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
        Some(Flow::Google {
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

#[derive(Deserialize)]
struct DeviceStart {
    device_auth_id: String,
    user_code: String,
    interval: Option<serde_json::Value>,
}

async fn codex_start(
    State((app, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
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
    pending.retain(|_, flow| match flow {
        Flow::Google { created, .. } | Flow::Codex { created, .. } => {
            created.elapsed() < Duration::from_secs(600)
        }
    });
    pending.insert(
        id.clone(),
        Flow::Codex {
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
    Ok(Json(
        json!({"id":id,"url":CODEX_DEVICE_AUTH_URL,"user_code":user_code,"interval":seconds}),
    ))
}

#[derive(Deserialize)]
struct CodexPoll {
    id: String,
}
#[derive(Deserialize)]
struct DeviceToken {
    authorization_code: Option<String>,
    code_verifier: Option<String>,
}

// Keep the device polling state transition and token exchange together.
#[allow(clippy::too_many_lines)]
async fn codex_poll(
    State((app, flows)): State<LoginState>,
    Path((provider_id, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<CodexPoll>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
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
        let Some(Flow::Codex {
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
            return Ok(Json(json!({"status":"pending"})));
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
            if let Some(Flow::Codex { in_flight, .. }) =
                flows.pending.lock().await.get_mut(&input.id)
            {
                *in_flight = false;
            }
            return Err(error(format!("Device polling failed: {e}")));
        }
        Err(_) => {
            if let Some(Flow::Codex { in_flight, .. }) =
                flows.pending.lock().await.get_mut(&input.id)
            {
                *in_flight = false;
            }
            return Err(error("Device polling timed out"));
        }
    };
    if response.status() == StatusCode::FORBIDDEN || response.status() == StatusCode::NOT_FOUND {
        if let Some(Flow::Codex { in_flight, .. }) = flows.pending.lock().await.get_mut(&input.id) {
            *in_flight = false;
        }
        return Ok(Json(json!({"status":"pending"})));
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
    Ok(Json(json!({"status":"complete"})))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{Flow, OAuthFlows, complete_google, parse_google_callback, same_origin};
    use axum::http::{HeaderMap, HeaderValue, header};
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn flows_are_bound_to_provider_and_credential() {
        let google = Flow::Google {
            provider_id: "google-one".into(),
            credential_id: "google-key".into(),
            created: Instant::now(),
            in_flight: false,
            shutdown: Arc::new(tokio::sync::Notify::new()),
        };
        let codex = Flow::Codex {
            provider_id: "codex-one".into(),
            credential_id: "codex-key".into(),
            created: Instant::now(),
            device_id: "device".into(),
            user_code: "code".into(),
            interval: std::time::Duration::from_secs(5),
            next_poll: Instant::now(),
            in_flight: false,
        };
        assert!(google.belongs_to("google-one", "google-key"));
        assert!(!google.belongs_to("google-two", "google-key"));
        assert!(!google.belongs_to("google-one", "other-key"));
        assert!(codex.belongs_to("codex-one", "codex-key"));
        assert!(!codex.belongs_to("codex-two", "codex-key"));
        assert!(!codex.belongs_to("codex-one", "other-key"));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn credentials_are_isolated_and_cannot_be_resurrected() {
        use crate::provider::antigravity::auth::{AuthManager, Credentials};
        use crate::{database::Database, server::settings::SettingsStore};
        use axum::{
            body::{Body, to_bytes},
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;

        let database = Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .await
            .unwrap();
        let providers = crate::provider::registry::router(store.clone());
        for kind in ["agy", "codex"] {
            let request = Request::builder()
                .method("POST")
                .uri("/api/providers")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"kind":"{kind}"}}"#)))
                .unwrap();
            assert_eq!(
                providers.clone().oneshot(request).await.unwrap().status(),
                StatusCode::CREATED
            );
        }
        let credentials = crate::provider::credentials::router(store.clone());
        let mut ids = Vec::new();
        for name in ["first", "second"] {
            let request = Request::builder()
                .method("POST")
                .uri("/api/providers/agy/credentials")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"name":"{name}","weight":1,"enabled":true}}"#
                )))
                .unwrap();
            let response = credentials.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            ids.push(value["id"].as_str().unwrap().to_string());
            let creds = Credentials {
                access_token: name.into(),
                refresh_token: None,
                project_id: name.into(),
                expires_at: None,
                email: None,
            };
            AuthManager::save(&store, "agy", ids.last().unwrap(), &creds)
                .await
                .unwrap();
        }
        let first = AuthManager::load(store.clone(), "agy", &ids[0])
            .await
            .unwrap()
            .unwrap();
        let second = AuthManager::load(store.clone(), "agy", &ids[1])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.get_credentials().await.unwrap().access_token, "first");
        assert_eq!(
            second.get_credentials().await.unwrap().access_token,
            "second"
        );
        let replacement = Credentials {
            access_token: "replacement".into(),
            refresh_token: None,
            project_id: "replacement".into(),
            expires_at: None,
            email: None,
        };
        AuthManager::save(&store, "agy", &ids[0], &replacement)
            .await
            .unwrap();
        assert_eq!(
            first.get_credentials().await.unwrap().access_token,
            "replacement"
        );
        assert_eq!(
            second.get_credentials().await.unwrap().access_token,
            "second"
        );
        assert!(
            AuthManager::save(&store, "codex", &ids[0], &replacement)
                .await
                .is_err()
        );
        assert!(
            AuthManager::load(store.clone(), "codex", &ids[0])
                .await
                .is_err()
        );
        assert!(
            crate::provider::codex::auth::CodexAuthManager::load(store.clone(), "codex", &ids[0])
                .await
                .is_err()
        );

        let request = Request::builder()
            .method("PUT")
            .uri(format!("/api/providers/agy/credentials/{}", ids[0]))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"first","weight":1,"enabled":false}"#))
            .unwrap();
        assert!(
            credentials
                .clone()
                .oneshot(request)
                .await
                .unwrap()
                .status()
                .is_success()
        );
        let late = Credentials {
            access_token: "late".into(),
            refresh_token: None,
            project_id: "late".into(),
            expires_at: None,
            email: None,
        };
        assert!(first.get_credentials().await.is_err());
        assert!(
            AuthManager::save(&store, "agy", &ids[0], &late)
                .await
                .is_err()
        );

        let request = Request::builder()
            .method("DELETE")
            .uri(format!("/api/providers/agy/credentials/{}", ids[1]))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            credentials.oneshot(request).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert!(second.get_credentials().await.is_err());
        assert!(
            AuthManager::save(&store, "agy", &ids[1], &late)
                .await
                .is_err()
        );
        assert!(
            store
                .secret_get(&format!("credential.{}.credential", ids[1]))
                .await
                .unwrap()
                .is_none()
        );

        let request = Request::builder()
            .method("DELETE")
            .uri("/api/providers/agy")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            providers.oneshot(request).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert!(
            AuthManager::save(&store, "agy", &ids[0], &late)
                .await
                .is_err()
        );
        assert!(
            store
                .secret_get(&format!("credential.{}.credential", ids[0]))
                .await
                .unwrap()
                .is_none()
        );
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

    #[test]
    fn origin_checks_public_host_without_trusting_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://example.com"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("example.com:443"));
        assert!(same_origin(&headers).is_ok());
        headers.insert(header::HOST, HeaderValue::from_static("internal:8080"));
        headers.insert("x-forwarded-host", HeaderValue::from_static("example.com"));
        assert!(same_origin(&headers).is_err());
    }

    #[tokio::test]
    async fn simultaneous_google_flows_keep_results_bound_to_credentials() {
        let flows = OAuthFlows::default();
        for id in ["success", "failure"] {
            flows.pending.lock().await.insert(
                id.into(),
                Flow::Google {
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

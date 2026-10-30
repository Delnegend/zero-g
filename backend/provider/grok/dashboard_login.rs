use super::auth::{self, CLIENT_ID, DEVICE_URL, TokenResponse};
use crate::{
    provider::login::{credential_lock, error, require_credential, same_origin},
    server::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Pending device flows shared by dashboard requests.
#[derive(Clone, Default)]
struct Flows(
    /// Flow IDs and their pending state.
    Arc<Mutex<HashMap<String, Flow>>>,
);
/// A device authorization bound to one provider and credential.
struct Flow {
    /// Provider owning the pending authorization.
    provider: String,
    /// Credential populated on success.
    credential: String,
    /// Required provider kind.
    kind: &'static str,
    /// Confidential upstream device code.
    code: String,
    /// Validated upstream token endpoint.
    token_endpoint: String,
    /// Device authorization expiration.
    expires: Instant,
    /// Earliest permitted poll.
    next: Instant,
    /// Required interval between polls.
    interval: Duration,
    /// Whether a poll is currently running.
    in_flight: bool,
}
/// Application and device-flow state.
type LoginState = (AppState, Flows);
/// Releases the in-flight flag even if the requesting future is cancelled.
struct PollGuard {
    /// Shared pending device flows.
    flows: Flows,
    /// Identifier of the flow being polled.
    id: String,
}
impl Drop for PollGuard {
    fn drop(&mut self) {
        let mut flows = self
            .flows
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(flow) = flows.get_mut(&self.id) {
            flow.in_flight = false;
        }
    }
}

/// Builds both providers' device login endpoints.
pub(crate) fn router(app: AppState) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/grok/start",
            post(start_grok),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/grok/poll",
            post(poll_grok),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/grok-build/start",
            post(start_build),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/grok-build/poll",
            post(poll_build),
        )
        .with_state((app, Flows::default()))
}
/// Device-flow information displayed to the user.
#[derive(Serialize, utoipa::ToSchema)]
struct GrokStartResponse {
    /// Opaque flow identifier.
    id: String,
    /// xAI verification URL.
    url: String,
    /// Code entered at the verification URL.
    user_code: String,
    /// Minimum poll interval in seconds.
    interval: u64,
}
/// Identifies an existing pending device flow.
#[derive(Deserialize, utoipa::ToSchema)]
struct GrokPollInput {
    /// Opaque flow identifier.
    id: String,
}
/// State of a device authorization.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum GrokPollResponse {
    /// The user has not completed authorization.
    Pending,
    /// Authorization succeeded and credentials were saved.
    Complete,
}
/// Device authorization response from xAI.
#[derive(Deserialize)]
struct DeviceAuthorization {
    /// Confidential device code.
    device_code: String,
    /// Code shown to the user.
    user_code: String,
    /// URL for device verification.
    verification_uri: Option<String>,
    /// URL optionally containing the verification code.
    verification_uri_complete: Option<String>,
    /// Lifetime in seconds.
    expires_in: Option<u64>,
    /// Poll interval in seconds.
    interval: Option<u64>,
}
/// `OpenAPI` specification for both login flows.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(start_grok, poll_grok, start_build, poll_build),
    components(schemas(GrokStartResponse, GrokPollInput, GrokPollResponse))
)]
pub(crate) struct Api;

/// JSON response or dashboard error.
type ResultJson<T> = Result<Json<T>, (StatusCode, String)>;
/// Requests an xAI device authorization using the appropriate scope.
async fn request_device(
    client: &reqwest::Client,
    endpoint: &str,
    kind: &str,
) -> Result<DeviceAuthorization, (StatusCode, String)> {
    let scope = if kind == "grok-build" {
        "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write"
    } else {
        "openid profile email offline_access grok-cli:access api:access"
    };
    let response = client
        .post(endpoint)
        .form(&[("client_id", CLIENT_ID), ("scope", scope)])
        .send()
        .await
        .map_err(|_| error("xAI device authorization failed"))?;
    if !response.status().is_success() {
        return Err(error(format!(
            "xAI device authorization returned HTTP {}",
            response.status()
        )));
    }
    response
        .json()
        .await
        .map_err(|_| error("Invalid xAI device response"))
}
/// Polls the OAuth endpoint with the device code.
async fn request_token(
    client: &reqwest::Client,
    endpoint: &str,
    code: &str,
) -> Result<(StatusCode, serde_json::Value), String> {
    let response = client
        .post(endpoint)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", CLIENT_ID),
            ("device_code", code),
        ])
        .send()
        .await
        .map_err(|_| "xAI device polling failed".to_owned())?;
    let status = response.status();
    let value = response
        .json()
        .await
        .map_err(|_| "Invalid xAI device token response".to_owned())?;
    Ok((status, value))
}
/// Creates a provider-bound pending device flow.
async fn start(
    state: LoginState,
    ids: (String, String),
    headers: HeaderMap,
    kind: &'static str,
) -> ResultJson<GrokStartResponse> {
    same_origin(&headers)?;
    require_credential(&state.0.settings, &ids.0, &ids.1, kind).await?;
    let client = auth::client().map_err(error)?;
    let token_endpoint = auth::token_endpoint(&client).await.map_err(error)?;
    let device = request_device(&client, DEVICE_URL, kind).await?;
    let verification_uri = device
        .verification_uri
        .or(device.verification_uri_complete)
        .ok_or_else(|| error("Missing xAI verification URL"))?;
    let url = reqwest::Url::parse(&verification_uri)
        .map_err(|_| error("Invalid xAI verification URL"))?;
    let host = url.host_str().unwrap_or("");
    if url.scheme() != "https"
        || !(host == "x.ai" || host.ends_with(".x.ai"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(error("Invalid xAI verification URL"));
    }
    let expires_in = device.expires_in.unwrap_or(900);
    if device.device_code.is_empty() || device.user_code.is_empty() || expires_in == 0 {
        return Err(error("Invalid xAI device response"));
    }
    let interval = Duration::from_secs(device.interval.unwrap_or(5).max(1));
    let now = Instant::now();
    let expires = now
        .checked_add(Duration::from_secs(expires_in))
        .ok_or_else(|| error("Invalid device expiry"))?;
    let next = now
        .checked_add(interval)
        .ok_or_else(|| error("Invalid device interval"))?;
    let id = Uuid::new_v4().to_string();
    let mut flows = state
        .1
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    flows.retain(|_, f| f.expires > now);
    flows.insert(
        id.clone(),
        Flow {
            provider: ids.0,
            credential: ids.1,
            kind,
            code: device.device_code,
            token_endpoint,
            expires,
            next,
            interval,
            in_flight: false,
        },
    );
    drop(flows);
    Ok(Json(GrokStartResponse {
        id,
        url: verification_uri,
        user_code: device.user_code,
        interval: interval.as_secs(),
    }))
}
/// Polls and persists a completed device authorization.
async fn poll(
    state: LoginState,
    ids: (String, String),
    headers: HeaderMap,
    input: GrokPollInput,
    kind: &'static str,
) -> ResultJson<GrokPollResponse> {
    same_origin(&headers)?;
    require_credential(&state.0.settings, &ids.0, &ids.1, kind).await?;
    let (code, endpoint) = {
        let mut flows = state
            .1
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        flows.retain(|_, f| f.expires > now);
        let flow = flows
            .get_mut(&input.id)
            .filter(|f| f.provider == ids.0 && f.credential == ids.1 && f.kind == kind)
            .ok_or_else(|| error("Device flow is invalid or expired"))?;
        if flow.in_flight || now < flow.next {
            return Ok(Json(GrokPollResponse::Pending));
        }
        flow.next = now
            .checked_add(flow.interval)
            .ok_or_else(|| error("Invalid device interval"))?;
        flow.in_flight = true;
        let result = (flow.code.clone(), flow.token_endpoint.clone());
        drop(flows);
        result
    };
    let _poll_guard = PollGuard {
        flows: state.1.clone(),
        id: input.id.clone(),
    };
    let result = match auth::client() {
        Ok(client) => request_token(&client, &endpoint, &code).await,
        Err(err) => Err(err),
    };
    let value = {
        let mut flows = state
            .1
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let flow = flows
            .get_mut(&input.id)
            .ok_or_else(|| error("Device flow is invalid or expired"))?;
        let (status, value) = result.map_err(error)?;
        if !status.is_success() {
            match value.get("error").and_then(serde_json::Value::as_str) {
                Some("authorization_pending") => return Ok(Json(GrokPollResponse::Pending)),
                Some("slow_down") => {
                    flow.interval = flow
                        .interval
                        .checked_add(Duration::from_secs(5))
                        .ok_or_else(|| error("Invalid device interval"))?;
                    flow.next = Instant::now()
                        .checked_add(flow.interval)
                        .ok_or_else(|| error("Invalid device interval"))?;
                    return Ok(Json(GrokPollResponse::Pending));
                }
                _ => {
                    flows.remove(&input.id);
                    return Err(error(format!("xAI device polling returned HTTP {status}")));
                }
            }
        }
        if flow.expires <= Instant::now() {
            flows.remove(&input.id);
            return Err(error("Device flow expired"));
        }
        flows.remove(&input.id);
        value
    };
    let token: TokenResponse =
        serde_json::from_value(value).map_err(|_| error("Invalid xAI token response"))?;
    let creds = token.credentials(None).map_err(error)?;
    let json =
        serde_json::to_string(&creds).map_err(|_| error("Failed to serialize xAI credentials"))?;
    let lock = credential_lock(&ids.1).await;
    let _guard = lock.lock().await;
    crate::provider::credentials::save_oauth(&state.0.settings, &ids.0, &ids.1, kind, &json)
        .await
        .map_err(error)?;
    Ok(Json(GrokPollResponse::Complete))
}
/// Starts an xAI API device login.
#[utoipa::path(post, path="/api/providers/{id}/credentials/{credential_id}/grok/start", operation_id="startGrokLogin", params(("id"=String, Path), ("credential_id"=String, Path)), responses((status=200, body=GrokStartResponse)))]
async fn start_grok(
    State(s): State<LoginState>,
    Path(p): Path<(String, String)>,
    h: HeaderMap,
) -> ResultJson<GrokStartResponse> {
    start(s, p, h, "grok").await
}
/// Polls an xAI API device login.
#[utoipa::path(post, path="/api/providers/{id}/credentials/{credential_id}/grok/poll", operation_id="pollGrokLogin", params(("id"=String, Path), ("credential_id"=String, Path)), request_body=GrokPollInput, responses((status=200, body=GrokPollResponse)))]
async fn poll_grok(
    State(s): State<LoginState>,
    Path(p): Path<(String, String)>,
    h: HeaderMap,
    Json(i): Json<GrokPollInput>,
) -> ResultJson<GrokPollResponse> {
    poll(s, p, h, i, "grok").await
}
/// Starts a Grok Build device login.
#[utoipa::path(post, path="/api/providers/{id}/credentials/{credential_id}/grok-build/start", operation_id="startGrokBuildLogin", params(("id"=String, Path), ("credential_id"=String, Path)), responses((status=200, body=GrokStartResponse)))]
async fn start_build(
    State(s): State<LoginState>,
    Path(p): Path<(String, String)>,
    h: HeaderMap,
) -> ResultJson<GrokStartResponse> {
    start(s, p, h, "grok-build").await
}
/// Polls a Grok Build device login.
#[utoipa::path(post, path="/api/providers/{id}/credentials/{credential_id}/grok-build/poll", operation_id="pollGrokBuildLogin", params(("id"=String, Path), ("credential_id"=String, Path)), request_body=GrokPollInput, responses((status=200, body=GrokPollResponse)))]
async fn poll_build(
    State(s): State<LoginState>,
    Path(p): Path<(String, String)>,
    h: HeaderMap,
    Json(i): Json<GrokPollInput>,
) -> ResultJson<GrokPollResponse> {
    poll(s, p, h, i, "grok-build").await
}

/// Device login HTTP behavior tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{body::Bytes, routing::post};
    use tokio::sync::mpsc;

    #[tokio::test]
    /// Requests distinct scopes for the API and Build pools.
    async fn device_request_uses_the_selected_scope_and_public_client() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let app = Router::new().route("/", post(move |body: Bytes| {
            let tx = tx.clone();
            async move {
                tx.send(String::from_utf8(body.to_vec()).unwrap()).unwrap();
                Json(serde_json::json!({"device_code":"device","user_code":"code","verification_uri":"https://auth.x.ai/activate","expires_in":600}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = auth::client().unwrap();
        for kind in ["grok", "grok-build"] {
            let device = request_device(&client, &format!("http://{addr}/"), kind)
                .await
                .unwrap();
            assert_eq!(device.device_code, "device");
            let body = rx.recv().await.unwrap();
            let params: std::collections::HashMap<_, _> =
                reqwest::Url::parse(&format!("http://localhost/?{body}"))
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect();
            assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
            let scope = params.get("scope").unwrap();
            assert!(scope.contains("grok-cli:access"));
            assert_eq!(scope.contains("conversations:write"), kind == "grok-build");
        }
        server.abort();
    }

    /// Sends the device grant and preserves an upstream pending status.
    #[tokio::test]
    async fn device_poll_sends_code_and_preserves_pending_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let app = Router::new().route(
            "/",
            post(move |body: Bytes| {
                let tx = tx.clone();
                async move {
                    tx.send(String::from_utf8(body.to_vec()).unwrap()).unwrap();
                    (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error":"authorization_pending"})),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (status, value) = request_token(
            &auth::client().unwrap(),
            &format!("http://{addr}/"),
            "device-secret",
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["error"], "authorization_pending");
        let body = rx.recv().await.unwrap();
        let params: std::collections::HashMap<_, _> =
            reqwest::Url::parse(&format!("http://localhost/?{body}"))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
        assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
        assert_eq!(
            params.get("device_code").map(String::as_str),
            Some("device-secret")
        );
        assert_eq!(
            params.get("grant_type").map(String::as_str),
            Some("urn:ietf:params:oauth:grant-type:device_code")
        );
        server.abort();
    }

    /// Creates an application with one enabled Grok credential.
    async fn poll_test_state() -> AppState {
        use crate::storage::{ProviderCredential, ProviderRecord};
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let settings = crate::server::settings::SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        toasty::create!(ProviderRecord {
            id: "grok".to_string(),
            name: "Grok".to_string(),
            kind: "grok".to_string(),
            base_url: String::new(),
            deleted: false
        })
        .exec(&mut (*database.db).clone())
        .await
        .unwrap();
        toasty::create!(ProviderCredential {
            id: "credential".to_string(),
            provider_id: "grok".to_string(),
            name: "Account".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut (*database.db).clone())
        .await
        .unwrap();
        AppState {
            providers: crate::provider::Providers::new(),
            settings,
            logs: crate::server::observability::LogStore::new(database),
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        }
    }

    /// Cancelling an in-flight poll releases it so the next request reaches xAI.
    #[tokio::test]
    async fn cancelled_poll_can_be_retried() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let app = Router::new().route(
            "/",
            post({
                let calls = calls.clone();
                move || {
                    let calls = calls.clone();
                    let tx = tx.clone();
                    async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst);
                        tx.send(()).unwrap();
                        if n == 0 {
                            std::future::pending::<()>().await;
                        }
                        (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error":"authorization_pending"})),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let state = poll_test_state().await;
        let flows = Flows::default();
        let now = Instant::now();
        flows.0.lock().unwrap().insert(
            "flow".into(),
            Flow {
                provider: "grok".into(),
                credential: "credential".into(),
                kind: "grok",
                code: "code".into(),
                token_endpoint: format!("http://{addr}/"),
                expires: now + Duration::from_secs(60),
                next: now,
                interval: Duration::ZERO,
                in_flight: false,
            },
        );
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://localhost".parse().unwrap());
        headers.insert("host", "localhost".parse().unwrap());
        let first = tokio::spawn(poll(
            (state.clone(), flows.clone()),
            ("grok".into(), "credential".into()),
            headers.clone(),
            GrokPollInput { id: "flow".into() },
            "grok",
        ));
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        first.abort();
        let _ = first.await;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            poll(
                (state, flows),
                ("grok".into(), "credential".into()),
                headers,
                GrokPollInput { id: "flow".into() },
                "grok",
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result.0, GrokPollResponse::Pending));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }
}

use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;

use crate::openai::ChatCompletionRequest;
use crate::provider::common::proxy::{ProxyError, check_model, error, settings_error};
use crate::provider::{openai, registry::Provider, zen};
use crate::server::AppState;
use crate::server::settings::{self, Permissions, SettingsStore};

/// Official OpenAI-compatible Zen inference endpoint.
const PAID_BASE: &str = "https://opencode.ai/inference/openai/v1";
/// Official catalog of Zen model IDs.
pub(super) const MODELS_URL: &str = "https://opencode.ai/inference/v1/models";

/// Build authenticated Zen model and inference routes.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "zen"),
            settings::virtual_key_auth,
        ))
}

/// Fetch and filter the current Zen model list.
pub(super) async fn fetch_models(
    url: &str,
    enabled: bool,
    permissions: &Permissions,
) -> Result<Value, ProxyError> {
    let response = reqwest::Client::new().get(url).send().await.map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "Unable to fetch Zen models",
            "upstream_error",
            "bad_gateway",
        )
    })?;
    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Unable to fetch Zen models",
            "upstream_error",
            "bad_gateway",
        ));
    }
    let mut value: Value = response.json().await.map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "Invalid Zen model list",
            "upstream_error",
            "bad_gateway",
        )
    })?;
    let data = value
        .get_mut("data")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "Invalid Zen model list",
                "upstream_error",
                "bad_gateway",
            )
        })?;
    data.retain(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| permissions.allows_model(id) && (enabled || !zen::is_free_model(id)))
    });
    Ok(value)
}

/// Return models allowed by the virtual key and free-model setting.
async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
) -> Response {
    let Ok(enabled) = zen::free_models_enabled(&state.settings).await else {
        return settings_error().into_response();
    };
    match fetch_models(MODELS_URL, enabled, &permissions).await {
        Ok(models) => Json(models).into_response(),
        Err(error) => error.into_response(),
    }
}

/// Route free chat through the shim and pass other requests to Zen.
async fn inference(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
) -> Response {
    if let Err(response) = check_model(&body, &permissions) {
        return response.into_response();
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let use_shim = if zen::is_free_model(model) {
        match zen::free_models_enabled(&state.settings).await {
            Ok(enabled) => enabled,
            Err(_) => return settings_error().into_response(),
        }
    } else {
        false
    };
    if use_shim {
        if path != "chat/completions" {
            return error(
                StatusCode::BAD_REQUEST,
                "Free Zen models do not support the Responses API",
                "invalid_request_error",
                "unsupported_api",
            )
            .into_response();
        }
        let req: ChatCompletionRequest = match serde_json::from_value(value) {
            Ok(req) => req,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "Invalid chat completion request",
                    "invalid_request_error",
                    "invalid_request",
                )
                .into_response();
            }
        };
        return send(&state.settings, &provider, &req).await;
    }
    match send_paid(
        &state.settings,
        &provider,
        PAID_BASE,
        path,
        query.as_deref(),
        &headers,
        body,
    )
    .await
    {
        Ok(response) => openai::forward(response),
        Err(error) => error.into_response(),
    }
}

/// Handle an OpenAI-compatible chat request.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        query,
        headers,
        body,
        "chat/completions",
    )
    .await
}

/// Handle an OpenAI-compatible Responses request.
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        query,
        headers,
        body,
        "responses",
    )
    .await
}

/// Forward a request with weighted Zen credentials.
async fn send_paid(
    settings: &SettingsStore,
    provider: &Provider,
    base: &str,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<reqwest::Response, ProxyError> {
    let credentials = crate::provider::credentials::list(settings, &provider.id)
        .await
        .map_err(|_| settings_error())?;
    let mut last = None;
    for credential in credentials {
        let key = settings
            .secret_get(&format!("credential.{}.api_key", credential.id))
            .await
            .map_err(|_| settings_error())?
            .ok_or_else(settings_error)?;
        let response = openai::send_with_headers(
            base,
            &key,
            path,
            query,
            Method::POST,
            headers,
            Some(body.clone()),
            true,
        )
        .await
        .map_err(|message| {
            error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
        })?;
        if !matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            return Ok(response);
        }
        last = Some(response);
    }
    last.ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No connected Zen credentials",
            "upstream_error",
            "no_credentials",
        )
    })
}

/// Send free chat without forwarding the paid API key to the shim.
pub(crate) async fn send(
    _settings: &SettingsStore,
    provider: &Provider,
    req: &ChatCompletionRequest,
) -> Response {
    zen::handle_chat(req, zen::shim_url(&provider.base_url), "").await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn model_discovery_filters_free_and_permissions_without_losing_metadata() {
        let app = Router::new().route("/models", get(|| async {
            Json(json!({"object":"list","data":[
                {"id":"gpt-paid","extra":1}, {"id":"new-free"}, {"id":"big-pickle"}, {"id":"excluded"}
            ]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/models", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let permissions = Permissions {
            providers: vec![],
            models: vec!["gpt-paid".into(), "new-free".into(), "big-pickle".into()],
        };
        let disabled = fetch_models(&url, false, &permissions).await.unwrap();
        assert_eq!(disabled["data"], json!([{"id":"gpt-paid","extra":1}]));
        let enabled = fetch_models(&url, true, &permissions).await.unwrap();
        assert_eq!(
            enabled["data"],
            json!([{"id":"gpt-paid","extra":1}, {"id":"new-free"}, {"id":"big-pickle"}])
        );
        server.abort();
    }

    #[tokio::test]
    async fn paid_responses_pass_through_body_query_and_auth() {
        use crate::storage::{ProviderCredential, ProviderRecord, Secret};
        let store = SettingsStore::new(
            crate::database::Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        toasty::create!(ProviderRecord {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            kind: "zen".to_string(),
            base_url: String::new(),
            deleted: false
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        toasty::create!(ProviderCredential {
            id: "key".to_string(),
            provider_id: "zen".to_string(),
            name: "key".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        let name = "credential.key.api_key";
        Secret::upsert_by_name(name)
            .value(store.encrypt(name, "secret").unwrap())
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        let app = Router::new().route(
            "/v1/responses",
            post(
                |RawQuery(query): RawQuery, headers: HeaderMap, body: Bytes| async move {
                    assert_eq!(query.as_deref(), Some("x=1"));
                    assert_eq!(headers.get("authorization").unwrap(), "Bearer secret");
                    assert_eq!(headers.get("x-custom").unwrap(), "test");
                    assert!(!headers.contains_key("cookie"));
                    assert!(!headers.contains_key("x-api-key"));
                    assert_eq!(
                        body,
                        Bytes::from_static(
                            b"{\"model\":\"gpt-paid\",\"stream\":true,\"custom\":1}"
                        )
                    );
                    (StatusCode::OK, "data: [DONE]\n\n")
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = Provider {
            id: "zen".into(),
            name: "Zen".into(),
            kind: "zen".into(),
            base_url: "http://old-shim/v1".into(),
        };
        let body = Bytes::from_static(b"{\"model\":\"gpt-paid\",\"stream\":true,\"custom\":1}");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer virtual-key".parse().unwrap());
        headers.insert("cookie", "session=private".parse().unwrap());
        headers.insert("x-api-key", "private".parse().unwrap());
        headers.insert("x-custom", "test".parse().unwrap());
        let response = send_paid(
            &store,
            &provider,
            &base,
            "responses",
            Some("x=1"),
            &headers,
            body,
        )
        .await
        .unwrap();
        assert_eq!(response.text().await.unwrap(), "data: [DONE]\n\n");
        server.abort();
    }
}

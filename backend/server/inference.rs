use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::future::join_all;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::provider::{self, catalog, registry};
use crate::server::{AppState, settings::Permissions};

/// Inference request with provider-qualified model and provider-specific fields.
#[derive(serde::Deserialize, serde::Serialize)]
struct InferenceRequest {
    /// Model identifier in `provider/model` form.
    model: String,
    /// Remaining OpenAI-compatible request fields are passed through.
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

/// Discovery failure for an individual provider.
#[derive(serde::Serialize)]
struct InferenceDiscoveryError {
    /// Provider identifier.
    provider: String,
    /// Discovery failure message.
    message: String,
}

/// OpenAI-compatible model list with per-provider discovery errors.
#[derive(serde::Serialize)]
struct InferenceModelList {
    /// List object discriminator.
    object: String,
    /// Provider model records, with qualified IDs.
    data: Vec<Value>,
    /// Providers whose discovery failed.
    errors: Vec<InferenceDiscoveryError>,
}

/// Return a consistent OpenAI-compatible error.
fn error(status: StatusCode, message: &str, kind: &str, code: &str) -> Response {
    (status, Json(super::error_body(message, kind, Some(code)))).into_response()
}

/// Authenticate before consulting the registry or discovering any models.
async fn permissions(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<Permissions, crate::provider::common::proxy::ProxyError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty())
        .map(|(_, token)| token.trim());
    let Some(token) = token else {
        return Err(crate::provider::common::proxy::error(
            StatusCode::UNAUTHORIZED,
            "A virtual key is required",
            "authentication_error",
            "invalid_api_key",
        ));
    };
    match state.settings.permissions(token).await {
        Ok(Some(permissions)) => Ok(permissions),
        Ok(None) => Err(crate::provider::common::proxy::error(
            StatusCode::UNAUTHORIZED,
            "Invalid virtual key",
            "authentication_error",
            "invalid_api_key",
        )),
        Err(_) => Err(crate::provider::common::proxy::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load permissions",
            "server_error",
            "internal_error",
        )),
    }
}

/// Aggregate permitted model records, without exposing upstream errors.
async fn models(State(state): State<AppState>, request: Request) -> Response {
    let permissions = match permissions(&state, request.headers()).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let Ok(providers) = registry::list(&state.settings.database).await else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load providers",
            "server_error",
            "internal_error",
        );
    };
    let providers: Vec<_> = providers
        .into_iter()
        .filter(|p| permissions.providers.is_empty() || permissions.providers.contains(&p.id))
        .collect();
    let results = join_all(providers.iter().map(|provider| async {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            catalog::discover_permitted(&state, provider, &permissions),
        )
        .await
    }))
    .await;
    let mut data = Vec::new();
    let mut errors = Vec::new();
    let mut succeeded = false;
    for (provider, result) in providers.iter().zip(results) {
        match result {
            Ok(Ok(models)) => {
                succeeded = true;
                for mut model in models {
                    let Some(id) = model.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let id = format!("{}/{id}", provider.id);
                    if let Some(object) = model.as_object_mut() {
                        object.insert("id".into(), Value::String(id));
                        data.push(model);
                    }
                }
            }
            _ => errors.push(InferenceDiscoveryError {
                provider: provider.id.clone(),
                message: "Model discovery failed".into(),
            }),
        }
    }
    if !succeeded && !errors.is_empty() {
        let mut body = super::error_body(
            "Model discovery failed",
            "upstream_error",
            Some("discovery_failed"),
        );
        if let Some(object) = body.as_object_mut() {
            object.insert("errors".into(), json!(errors));
        }
        return (StatusCode::BAD_GATEWAY, Json(body)).into_response();
    }
    Json(InferenceModelList {
        object: "list".into(),
        data,
        errors,
    })
    .into_response()
}

/// Decode and validate the provider-qualified model without changing request fields.
fn parse_inference(
    body: &[u8],
) -> Result<(String, String, InferenceRequest), crate::provider::common::proxy::ProxyError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| {
        crate::provider::common::proxy::error(
            StatusCode::BAD_REQUEST,
            "Invalid JSON request",
            "invalid_request_error",
            "invalid_request",
        )
    })?;
    let invalid_model = || {
        crate::provider::common::proxy::error(
            StatusCode::BAD_REQUEST,
            "Model must be provider/model",
            "invalid_request_error",
            "invalid_model",
        )
    };
    let (id, model) = value
        .get("model")
        .and_then(Value::as_str)
        .and_then(|m| m.split_once('/'))
        .filter(|(id, model)| !id.is_empty() && !model.is_empty())
        .ok_or_else(invalid_model)?;
    let id = id.to_owned();
    let model = model.to_owned();
    let value = serde_json::from_value(value).map_err(|_| invalid_model())?;
    Ok((id, model, value))
}

/// Rewrite only the model and dispatch to the existing private provider router.
async fn inference(
    State(state): State<AppState>,
    axum::Extension(router): axum::Extension<Router>,
    request: Request,
) -> Response {
    let permissions = match permissions(&state, request.headers()).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let (mut parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, 2 * 1024 * 1024).await else {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request body too large",
            "invalid_request_error",
            "body_too_large",
        );
    };
    let (id, model, mut value) = match parse_inference(&body) {
        Ok(value) => value,
        Err(err) => return err.into_response(),
    };
    if !permissions.providers.is_empty() && !permissions.providers.contains(&id) {
        return error(
            StatusCode::FORBIDDEN,
            "Virtual key does not permit this provider",
            "permission_error",
            "provider_not_allowed",
        );
    }
    let provider = match registry::get(&state.settings.database, &id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => {
            return error(
                StatusCode::NOT_FOUND,
                "Provider not found",
                "invalid_request_error",
                "provider_not_found",
            );
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load provider",
                "server_error",
                "internal_error",
            );
        }
    };
    value.model = model;
    let path = parts
        .uri
        .path_and_query()
        .map_or("/v1/chat/completions", |p| p.as_str());
    let uri = format!("/{}/{id}{path}", provider.kind);
    let Ok(uri) = uri.parse() else {
        return error(
            StatusCode::BAD_REQUEST,
            "Invalid request URL",
            "invalid_request_error",
            "invalid_request",
        );
    };
    parts.uri = uri;
    parts.headers.remove(header::CONTENT_LENGTH);
    let Ok(body) = serde_json::to_vec(&value) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to serialize request",
            "server_error",
            "internal_error",
        );
    };
    match router
        .oneshot(Request::from_parts(parts, Body::from(body)))
        .await
    {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// Dispatch a unified Responses API request through the same inference handler.
async fn inference_responses(
    state: State<AppState>,
    router: axum::Extension<Router>,
    request: Request,
) -> Response {
    inference(state, router, request).await
}

/// The sole public inference API.
pub(super) fn router(state: &AppState) -> Router<AppState> {
    let internal: Router = provider::router(&state.settings).with_state(state.clone());
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(inference))
        .route("/v1/responses", post(inference_responses))
        .layer(axum::Extension(internal))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        database::Database,
        server::{observability::LogStore, settings},
    };
    use axum::{
        body::to_bytes,
        http::{Request, header},
        routing::post,
    };

    /// Read a JSON response body.
    async fn json_body(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    /// Start a local OpenAI-compatible server that exposes its received request.
    async fn upstream() -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/v1/models", get(|| async {
                Json(json!({"data":[{"id":"same","object":"model"},{"id":"org/nested","object":"model"},{"id":"denied","object":"model"}]}))
            }))
            .route("/v1/chat/completions", post(|Json(body): Json<Value>| async move {
                if body["stream"] == true {
                    ([(header::CONTENT_TYPE, "text/event-stream")], format!("data: {body}\n\ndata: [DONE]\n\n")).into_response()
                } else {
                    ([(header::CONTENT_TYPE, "application/json"), ("x-upstream".parse().unwrap(), "present")], body.to_string()).into_response()
                }
            }))
            .route("/v1/responses", post(|Json(body): Json<Value>| async move { Json(body) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, task)
    }

    /// Build a gateway with two healthy providers and one unconfigured provider.
    async fn fixture() -> (
        Router,
        String,
        String,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let (first, first_task) = upstream().await;
        let (second, second_task) = upstream().await;
        let database = Database::open(":memory:").await.unwrap();
        let store = settings::SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings: store.clone(),
            logs: LogStore::new(database),
            dashboard_auth: crate::server::dashboard_auth::DashboardAuth::test().unwrap(),
        };
        for (id, base) in [("first", first), ("second", second)] {
            let response = registry::router(store.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/providers")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({"id":id,"name":id,"kind":"openai","base_url":base}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
            let response = crate::provider::credentials::router(store.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/providers/{id}/credentials"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({"name":"key","weight":1,"enabled":true,"api_key":"secret"})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }
        let offline = registry::router(store.clone()).oneshot(Request::builder().method("POST")
            .uri("/api/providers").header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"id":"offline","name":"offline","kind":"openai","base_url":"http://127.0.0.1:1/v1"}).to_string())).unwrap()).await.unwrap();
        assert_eq!(offline.status(), StatusCode::CREATED);
        let keys = Router::new()
            .route("/keys", post(settings::create_key))
            .with_state(store);
        let make_key = |providers: Value, models: Value| {
            let keys = keys.clone();
            async move {
                let response = keys
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/keys")
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(
                                json!({"name":"test","providers":providers,"models":models})
                                    .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::CREATED);
                json_body(response).await["key"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
        };
        let all = make_key(json!([]), json!([])).await;
        let restricted = make_key(json!(["first"]), json!(["org/nested"])).await;
        let app = crate::server::create_router(state);
        (app, all, restricted, first_task, second_task)
    }

    /// Check discovery and routing for both inference APIs.
    async fn check_discovery_and_routing(app: &Router, all: &str, restricted: &str) {
        let send = |method: &str, path: &str, key: &str, body: Value| {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            app.clone().oneshot(request)
        };
        let models = json_body(send("GET", "/v1/models", all, Value::Null).await.unwrap()).await;
        assert_eq!(models["errors"][0]["provider"], "offline");
        assert_eq!(models["errors"][0]["message"], "Model discovery failed");
        let ids: Vec<_> = models["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"first/same") && ids.contains(&"second/same"));
        assert!(ids.contains(&"first/org/nested"));
        let models = json_body(
            send("GET", "/v1/models", restricted, Value::Null)
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(models["data"].as_array().unwrap().len(), 1);
        assert_eq!(models["data"][0]["id"], "first/org/nested");
        for path in ["/v1/chat/completions", "/v1/responses"] {
            let response = send(
                "POST",
                path,
                restricted,
                json!({"model":"first/org/nested","custom":{"nested":1}}),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;
            assert_eq!(body["model"], "org/nested");
            assert_eq!(body["custom"]["nested"], 1);
        }
    }

    /// Check streaming and denied or removed routes.
    async fn check_stream_and_denials(app: &Router, all: &str, restricted: &str) {
        let send = |method: &str, path: &str, key: &str, body: Value| {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            app.clone().oneshot(request)
        };
        let stream = send(
            "POST",
            "/v1/chat/completions",
            all,
            json!({"model":"second/same","stream":true}),
        )
        .await
        .unwrap();
        assert_eq!(stream.headers()[header::CONTENT_TYPE], "text/event-stream");
        let body = to_bytes(stream.into_body(), usize::MAX).await.unwrap();
        assert!(
            std::str::from_utf8(&body)
                .unwrap()
                .contains("\"model\":\"same\"")
        );
        assert_eq!(
            send(
                "POST",
                "/v1/chat/completions",
                restricted,
                json!({"model":"second/org/nested"})
            )
            .await
            .unwrap()
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(
                "POST",
                "/v1/chat/completions",
                restricted,
                json!({"model":"first/denied"})
            )
            .await
            .unwrap()
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(
                "POST",
                "/v1/chat/completions",
                all,
                json!({"model":"missing/same"})
            )
            .await
            .unwrap()
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send("POST", "/v1/chat/completions", all, json!({"model":"same"}))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_ne!(
            send(
                "POST",
                "/openai/first/v1/chat/completions",
                all,
                json!({"model":"same"})
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
    }

    /// Exercise routing, filtering, streaming, and removed paths.
    #[tokio::test]
    async fn unified_api_routes_and_enforces_permissions() {
        let (app, all, restricted, first_task, second_task) = fixture().await;
        check_discovery_and_routing(&app, &all, &restricted).await;
        check_stream_and_denials(&app, &all, &restricted).await;
        first_task.abort();
        second_task.abort();
    }
}

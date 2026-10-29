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

use crate::openai::Model;
use crate::provider::{self, catalog, registry};
use crate::server::{AppState, settings::Permissions};

/// Inference request with provider-qualified model and provider-specific fields.
#[derive(serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
struct InferenceRequest {
    /// Model identifier in `provider/model` form.
    model: String,
    /// Remaining OpenAI-compatible request fields are passed through.
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

/// Discovery failure for an individual provider.
#[derive(serde::Serialize, utoipa::ToSchema)]
struct InferenceDiscoveryError {
    /// Provider identifier.
    provider: String,
    /// Discovery failure message.
    message: String,
}

/// OpenAI-compatible model list with per-provider discovery errors.
#[derive(serde::Serialize, utoipa::ToSchema)]
struct InferenceModelList {
    /// List object discriminator.
    object: String,
    /// Provider model records, with qualified IDs.
    data: Vec<Model>,
    /// Providers whose discovery failed.
    errors: Vec<InferenceDiscoveryError>,
}

/// Public unified inference operations.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(models, inference, inference_responses),
    components(schemas(InferenceRequest, InferenceModelList, InferenceDiscoveryError, Model))
)]
pub(crate) struct Api;

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
#[utoipa::path(get, path = "/v1/models", operation_id = "listInferenceModels",
    params(("Authorization" = String, Header, description = "Bearer virtual key")),
    responses((status = 200, body = InferenceModelList),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 500, description = "Registry or permissions error", body = Value),
        (status = 502, description = "All provider discoveries failed", body = Value)))]
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
                    model.id = format!("{}/{}", provider.id, model.id);
                    data.push(model);
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
#[utoipa::path(post, path = "/v1/chat/completions", operation_id = "createChatCompletion",
    params(("Authorization" = String, Header, description = "Bearer virtual key")),
    request_body = InferenceRequest,
    responses((status = 200, description = "Provider JSON or server-sent event stream",
        content((Value = "application/json"), (String = "text/event-stream"))),
        (status = 400, description = "Invalid request", body = Value),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 403, description = "Key does not permit provider or model", body = Value),
        (status = 404, description = "Provider not found", body = Value),
        (status = 413, description = "Request body too large", body = Value),
        (status = 500, description = "Gateway error", body = Value),
        (status = 502, description = "Provider error", body = Value)))]
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
#[utoipa::path(post, path = "/v1/responses", operation_id = "createResponse",
    params(("Authorization" = String, Header, description = "Bearer virtual key")),
    request_body = InferenceRequest,
    responses((status = 200, description = "Provider JSON or server-sent event stream",
        content((Value = "application/json"), (String = "text/event-stream"))),
        (status = 400, description = "Invalid request", body = Value),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 403, description = "Key does not permit provider or model", body = Value),
        (status = 404, description = "Provider not found", body = Value),
        (status = 413, description = "Request body too large", body = Value),
        (status = 500, description = "Gateway error", body = Value),
        (status = 502, description = "Provider error", body = Value)))]
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// Read a JSON response body.
    async fn json_body(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    /// Start a local OpenAI-compatible server that exposes its received request.
    async fn upstream(requests: Arc<AtomicUsize>) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/v1/models", get(|| async {
                Json(json!({"data":[{"id":"same","object":"model","provider_metadata":{"nested":42}},{"id":"org/nested","object":"model"},{"id":"denied","object":"model"}]}))
            }))
            .route("/v1/chat/completions", post(|Json(body): Json<Value>| async move {
                if body["stream"] == true {
                    ([(header::CONTENT_TYPE, "text/event-stream")], format!("data: {body}\n\ndata: [DONE]\n\n")).into_response()
                } else {
                    ([(header::CONTENT_TYPE, "application/json"), ("x-upstream".parse().unwrap(), "present")], body.to_string()).into_response()
                }
            }))
            .route("/v1/responses", post(|Json(body): Json<Value>| async move { Json(body) }))
            .layer(axum::middleware::from_fn(move |request: Request<Body>, next: axum::middleware::Next| {
                let requests = requests.clone();
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    next.run(request).await
                }
            }));
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
        Arc<AtomicUsize>,
    ) {
        let requests = Arc::new(AtomicUsize::new(0));
        let (first, first_task) = upstream(requests.clone()).await;
        let (second, second_task) = upstream(requests.clone()).await;
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
        (app, all, restricted, first_task, second_task, requests)
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
        let first = models["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["id"] == "first/same")
            .unwrap();
        assert_eq!(first["provider_metadata"]["nested"], 42);
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
        let (app, all, restricted, first_task, second_task, _) = fixture().await;
        check_discovery_and_routing(&app, &all, &restricted).await;
        check_stream_and_denials(&app, &all, &restricted).await;
        first_task.abort();
        second_task.abort();
    }

    /// Reject invalid virtual keys before contacting any provider.
    #[tokio::test]
    async fn unified_api_authenticates_before_discovery_or_inference() {
        let (app, valid_key, _, first_task, second_task, requests) = fixture().await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {valid_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(requests.load(Ordering::SeqCst) > 0);
        requests.store(0, Ordering::SeqCst);

        for (method, path) in [
            ("GET", "/v1/models"),
            ("POST", "/v1/chat/completions"),
            ("POST", "/v1/responses"),
        ] {
            for authorization in [
                None,
                Some("Basic malformed"),
                Some("Bearer unknown-virtual-key"),
            ] {
                let mut request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json");
                if let Some(authorization) = authorization {
                    request = request.header(header::AUTHORIZATION, authorization);
                }
                let response = app
                    .clone()
                    .oneshot(
                        request
                            .body(Body::from(
                                json!({"model":"first/same","messages":[],"input":"hello"})
                                    .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{method} {path} {authorization:?}"
                );
                assert_eq!(
                    json_body(response).await["error"]["code"],
                    "invalid_api_key"
                );
                assert_eq!(
                    requests.load(Ordering::SeqCst),
                    0,
                    "{method} {path} {authorization:?}"
                );
            }
        }
        first_task.abort();
        second_task.abort();
    }
}

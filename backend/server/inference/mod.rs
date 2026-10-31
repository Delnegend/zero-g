use crate::openai::Model;
use crate::provider;
use crate::server::{AppState, settings::Permissions};
use axum::{
    Json, Router,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::Value;

/// Chat Completions endpoint.
mod chat_completions;
/// Decisions endpoint.
mod decisions;
/// Shared request validation and provider dispatch.
mod dispatch;
/// Model discovery endpoint.
mod models;
/// Responses endpoint.
mod responses;

use chat_completions::{__path_inference, inference};
use decisions::{__path_inference_decisions, inference_decisions};
use models::{__path_models, models};
use responses::{__path_inference_responses, inference_responses};

/// Maximum size in bytes of a unified inference JSON request.
pub(super) const INFERENCE_BODY_LIMIT: usize = 20_000_000;

/// Inference request with provider-qualified model and provider-specific fields.
#[derive(serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
struct InferenceRequest {
    /// Model identifier in `provider/model` form.
    model: String,
    /// Remaining provider-specific request fields are passed through.
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
    paths(models, inference, inference_responses, inference_decisions),
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
    identity: Option<&crate::provider::common::request_identity::RequestIdentity>,
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
    match state.settings.permissions(token, identity).await {
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

/// The sole public inference API.
pub(super) fn router(state: &AppState) -> Router<AppState> {
    let internal: Router = provider::router(&state.settings)
        .layer(axum::extract::DefaultBodyLimit::max(INFERENCE_BODY_LIMIT))
        .with_state(state.clone());
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(inference))
        .route("/v1/responses", post(inference_responses))
        .route("/v1/decisions", post(inference_decisions))
        .layer(axum::Extension(internal))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::provider::registry;
    use crate::{
        database::Database,
        server::{observability::LogStore, settings},
    };
    use axum::body::Body;
    use axum::{
        body::to_bytes,
        http::{Request, header},
        routing::post,
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tower::ServiceExt;

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
            .layer(axum::extract::DefaultBodyLimit::max(INFERENCE_BODY_LIMIT))
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
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
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

    /// Large inline media reaches both handlers, while the byte limit is enforced.
    #[tokio::test]
    async fn unified_inference_body_limit() {
        let (app, key, _, first_task, second_task, requests) = fixture().await;
        for path in ["/v1/chat/completions", "/v1/responses"] {
            let media = "A".repeat(2 * 1024 * 1024 + 1024);
            let body = if path.ends_with("completions") {
                json!({"model":"first/same","messages":[{"role":"user","content":[
                    {"type":"video_url","video_url":{"url":format!("data:video/mp4;base64,{media}")}}
                ]}]}).to_string()
            } else {
                json!({"model":"first/same","input":[{"role":"user","content":[
                    {"type":"input_file","filename":"document.pdf","file_data":format!("data:application/pdf;base64,{media}")}
                ]}]}).to_string()
            };
            assert!(body.len() > 2 * 1024 * 1024);
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {key}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let received = json_body(response).await;
            assert_eq!(received["model"], "same");
            assert!(received.to_string().contains(&media));
        }

        let prefix = r#"{"model":"first/same","input":""#;
        let suffix = r#""}"#;
        let at_limit = format!(
            "{prefix}{}{suffix}",
            "A".repeat(INFERENCE_BODY_LIMIT - prefix.len() - suffix.len())
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(header::AUTHORIZATION, format!("Bearer {key}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(at_limit))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);

        let before = requests.load(Ordering::SeqCst);
        for path in ["/v1/chat/completions", "/v1/responses"] {
            let prefix = r#"{"model":"first/same","input":""#;
            let suffix = r#""}"#;
            let body = format!(
                "{prefix}{}{suffix}",
                "A".repeat(INFERENCE_BODY_LIMIT + 1 - prefix.len() - suffix.len())
            );
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {key}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "{path}");
        }
        assert_eq!(requests.load(Ordering::SeqCst), before);
        first_task.abort();
        second_task.abort();
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

    /// Decisions preserve provider responses and enforce provider and model permissions.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn unified_decisions_dispatch() {
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
            dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        };
        for body in [
            json!({"kind":"openrouter"}),
            json!({"id":"other","name":"other","kind":"openai","base_url":"http://127.0.0.1:1/v1"}),
        ] {
            let response = registry::router(store.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/providers")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }
        let keys = Router::new()
            .route("/keys", post(settings::create_key))
            .with_state(store);
        let make_key = |providers: Value| {
            let keys = keys.clone();
            async move {
                let response = keys.oneshot(Request::builder().method("POST").uri("/keys")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"name":"decisions","providers":providers,"models":["cloudflare/clef"]}).to_string())).unwrap()
                ).await.unwrap();
                assert_eq!(response.status(), StatusCode::CREATED);
                json_body(response).await["key"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }
        };
        let key = make_key(json!(["openrouter"])).await;
        let unrestricted_provider = make_key(json!([])).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let internal: Router = Router::new().route(
            "/openrouter/openrouter/v1/decisions",
            post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    let status = if body["fail"] == true {
                        StatusCode::TOO_MANY_REQUESTS
                    } else {
                        StatusCode::OK
                    };
                    (status, [("x-decision", "preserved")], Json(body))
                }
            }),
        );
        let app = Router::new()
            .route("/v1/decisions", post(inference_decisions))
            .layer(axum::Extension(internal))
            .with_state(state);
        let send = |key: &str, body: Value| {
            app.clone().oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/decisions")
                    .header(header::AUTHORIZATION, format!("Bearer {key}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
        };
        let payload = json!({
            "model":"openrouter/cloudflare/clef",
            "state":[{"type":"image","url":"data:image/png;base64,AAAA"},{"nested":[1, {"x":true}]}],
            "questions":[{"name":"choice","options":["a","b"]}],
            "custom":{"preserve":true}
        });
        for fail in [false, true] {
            let mut request = payload.clone();
            request["fail"] = json!(fail);
            let response = send(&key, request.clone()).await.unwrap();
            assert_eq!(
                response.status(),
                if fail {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::OK
                }
            );
            assert_eq!(response.headers()["x-decision"], "preserved");
            request["model"] = json!("cloudflare/clef");
            assert_eq!(json_body(response).await, request);
        }
        for model in ["other/cloudflare/clef", "openrouter/denied"] {
            assert_eq!(
                send(&key, json!({"model":model})).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        let response = send(
            &unrestricted_provider,
            json!({"model":"other/cloudflare/clef"}),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
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
            ("POST", "/v1/decisions"),
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

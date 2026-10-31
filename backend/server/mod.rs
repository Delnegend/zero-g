/// Dashboard authentication environment configuration.
pub(crate) mod dashboard_auth_config;
/// Health probe endpoint.
mod health;
/// Unified public inference API.
pub(crate) mod inference;
/// Cached provider model discovery.
pub(crate) mod model_catalog;
/// Model metadata settings and synchronization.
pub(crate) mod model_setting;
/// Request logging and dashboard metrics.
pub(crate) mod observability;
/// `OpenAPI` document endpoint.
mod openapi;
/// API reference endpoint.
mod redoc;
/// Persistent settings and API keys.
pub(crate) mod settings;
/// Embedded dashboard asset serving.
mod web;

use axum::Router;
use axum::routing::get;
use health::__path_health_handler;
use openapi::__path_openapi;
use redoc::__path_redoc;
use tower_http::cors::{Any, CorsLayer};

use crate::provider::{self, registry};

/// Shared services used by server routes.
#[derive(Clone)]
pub(crate) struct AppState {
    /// Configured provider clients.
    pub(crate) providers: provider::Providers,
    /// Persistent settings and credentials.
    pub(crate) settings: settings::SettingsStore,
    /// Request log storage.
    pub(crate) logs: observability::LogStore,
    /// Dashboard access protection.
    pub(crate) dashboard_auth: axum_auth_gate::Auth,
}

/// Health and API reference operations.
#[derive(utoipa::OpenApi)]
#[openapi(paths(health_handler, redoc, openapi))]
pub(crate) struct Api;

/// Assemble public provider routes and protected dashboard routes.
pub(crate) fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_private_network(true);

    let dashboard = Router::new()
        .route("/", get(web::index))
        .route("/redoc", get(redoc::redoc))
        .route("/api/openapi.json", get(openapi::openapi))
        .route(
            "/api/observability",
            get(observability::dashboard).with_state(state.logs.clone()),
        )
        .route(
            "/api/settings",
            get(settings::get_settings)
                .put(settings::put_settings)
                .with_state(state.settings.clone()),
        )
        .route(
            "/api/keys",
            get(settings::list_keys)
                .post(settings::create_key)
                .with_state(state.settings.clone()),
        )
        .route(
            "/api/keys/{id}",
            axum::routing::patch(settings::update_key).with_state(state.settings.clone()),
        )
        .route(
            "/api/keys/{id}/revoke",
            axum::routing::post(settings::revoke_key).with_state(state.settings.clone()),
        )
        .merge(registry::router(state.settings.clone()))
        .merge(provider::dashboard_router(&state))
        .merge(model_catalog::router(state.clone()))
        .merge(model_setting::router(state.clone()))
        .fallback(get(web::fallback));
    let dashboard = state.dashboard_auth.protect(dashboard);

    Router::new()
        .route("/health", get(health::health_handler))
        .merge(inference::router(&state))
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(
            state.logs.clone(),
            observability::capture,
        ))
        .with_state(state)
        .merge(dashboard)
        .layer(axum::middleware::from_fn(
            |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let method = req.method().clone();
                let uri = req.uri().path().to_owned();
                tracing::info!("--> {method} {uri}");
                let resp = next.run(req).await;
                tracing::info!("<-- {method} {uri} {}", resp.status());
                resp
            },
        ))
}

/// Construct an OpenAI-compatible error object.
pub(crate) fn error_body(message: &str, error_type: &str, code: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": null,
            "code": code
        }
    })
}

/// API reference access and contract checks.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    /// The reference and its contract require dashboard authentication.
    #[tokio::test]
    async fn api_reference_requires_dashboard_authentication() {
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let settings = settings::SettingsStore::new(
            database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let app = create_router(AppState {
            providers: provider::Providers::new(),
            settings,
            logs: observability::LogStore::new(database),
            dashboard_auth: dashboard_auth_config::test_auth().await,
        });
        for path in ["/redoc", "/api/openapi.json", "/redoc.standalone.js"] {
            let request = Request::builder()
                .uri(path)
                .header(header::HOST, "127.0.0.1:8080")
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let request = Request::builder()
            .uri("/api/openapi.json")
            .header(header::HOST, "127.0.0.1:8080")
            .header(header::AUTHORIZATION, "Basic dGVzdDp0ZXN0")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            document.pointer("/paths/~1api~1settings/get/operationId"),
            Some(&serde_json::json!("getSettings"))
        );
        assert_eq!(
            document.pointer("/paths/~1v1~1models/get/operationId"),
            Some(&serde_json::json!("listInferenceModels"))
        );
    }
}

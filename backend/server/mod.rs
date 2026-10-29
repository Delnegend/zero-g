/// Dashboard authentication and session handling.
pub(crate) mod dashboard_auth;
/// Unified public inference API.
mod inference;
/// Cached provider model discovery.
pub(crate) mod model_catalog;
/// Model metadata settings and synchronization.
pub(crate) mod model_settings;
/// Request logging and dashboard metrics.
pub(crate) mod observability;
/// Persistent settings and API keys.
pub(crate) mod settings;
/// Embedded dashboard asset serving.
mod web;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
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
    pub(crate) dashboard_auth: dashboard_auth::DashboardAuth,
}

/// Assemble public provider routes and protected dashboard routes.
pub(crate) fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_private_network(true);

    let dashboard = Router::new()
        .route("/", get(web::index))
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
            axum::routing::delete(settings::revoke_key).with_state(state.settings.clone()),
        )
        .merge(registry::router(state.settings.clone()))
        .merge(provider::dashboard_router(&state))
        .merge(model_catalog::router(state.clone()))
        .merge(model_settings::router(state.clone()))
        .fallback(get(web::fallback));
    let dashboard = state.dashboard_auth.protect(dashboard);

    Router::new()
        .route("/health", get(health_handler))
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

/// Return a successful health probe response.
async fn health_handler() -> impl IntoResponse {
    StatusCode::OK
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

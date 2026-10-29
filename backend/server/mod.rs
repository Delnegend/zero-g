pub(crate) mod dashboard_auth;
pub(crate) mod model_catalog;
pub(crate) mod model_settings;
pub(crate) mod observability;
pub(crate) mod settings;
mod web;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use tower_http::cors::{Any, CorsLayer};

use crate::provider::antigravity::AntigravityClient;
use crate::provider::codex::CodexClient;
use crate::provider::{go, login as provider_login, openai, registry, zen};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) client: Arc<AntigravityClient>,
    pub(crate) codex_client: Arc<CodexClient>,
    pub(crate) settings: settings::SettingsStore,
    pub(crate) logs: observability::LogStore,
    pub(crate) dashboard_auth: dashboard_auth::DashboardAuth,
}

pub(crate) fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_private_network(true);

    let agy_routes = crate::provider::antigravity::routes::router(&state.settings);
    let codex_routes = crate::provider::codex::routes::router(&state.settings);
    let zen_routes = zen::routes::router(&state.settings);
    let go_routes = go::router(&state.settings);
    let openai_routes = openai::routes::router(&state.settings);

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
        .merge(provider_login::router(state.clone()))
        .merge(model_catalog::router(state.clone()))
        .merge(model_settings::router(state.clone()))
        .fallback(get(web::fallback));
    let dashboard = state.dashboard_auth.protect(dashboard);

    Router::new()
        .route("/health", get(health_handler))
        .nest("/agy/{id}", agy_routes)
        .nest("/codex/{id}", codex_routes)
        .nest("/zen/{id}", zen_routes)
        .nest("/go/{id}", go_routes)
        .nest("/openai/{id}", openai_routes)
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

async fn health_handler() -> impl IntoResponse {
    StatusCode::OK
}

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

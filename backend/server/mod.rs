mod agy;
pub(crate) mod codex;
pub(crate) mod dashboard_auth;
pub(crate) mod observability;
pub(crate) mod provider_login;
pub(crate) mod settings;
pub(crate) mod sse;
mod web;
mod zen;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use tower_http::cors::{Any, CorsLayer};

use crate::antigravity::{AntigravityClient, AuthManager};
use crate::codex::{CodexAuthManager, CodexClient};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) auth: Arc<tokio::sync::RwLock<Option<AuthManager>>>,
    pub(crate) client: Arc<AntigravityClient>,
    pub(crate) codex_auth: Arc<tokio::sync::RwLock<Option<CodexAuthManager>>>,
    pub(crate) codex_client: Arc<CodexClient>,
    pub(crate) settings: settings::SettingsStore,
    pub(crate) logs: observability::LogStore,
    pub(crate) dashboard_auth: dashboard_auth::DashboardAuth,
}

// Route registration is kept together so the authentication boundaries are visible.
#[allow(clippy::too_many_lines)]
pub(crate) fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_private_network(true);

    let agy_routes = Router::new()
        .route("/v1/models", get(agy::models_handler))
        .route("/v1/models/", get(agy::models_handler))
        .route("/models", get(agy::models_handler))
        .route("/models/", get(agy::models_handler))
        .route("/v1/chat/completions", post(agy::chat_handler))
        .route("/v1/chat/completions/", post(agy::chat_handler))
        .route("/chat/completions", post(agy::chat_handler))
        .route("/chat/completions/", post(agy::chat_handler))
        .route("/v1/responses", post(agy::responses_handler))
        .route("/v1/responses/", post(agy::responses_handler))
        .route("/responses", post(agy::responses_handler))
        .route("/responses/", post(agy::responses_handler))
        .layer(axum::middleware::from_fn_with_state(
            (state.settings.clone(), "agy"),
            settings::virtual_key_auth,
        ));

    let codex_routes = Router::new()
        .route("/v1/models", get(codex::models_handler))
        .route("/v1/models/", get(codex::models_handler))
        .route("/models", get(codex::models_handler))
        .route("/models/", get(codex::models_handler))
        .route("/v1/chat/completions", post(codex::chat_handler))
        .route("/v1/chat/completions/", post(codex::chat_handler))
        .route("/chat/completions", post(codex::chat_handler))
        .route("/chat/completions/", post(codex::chat_handler))
        .route("/v1/responses", post(codex::responses_handler))
        .route("/v1/responses/", post(codex::responses_handler))
        .route("/responses", post(codex::responses_handler))
        .route("/responses/", post(codex::responses_handler))
        .layer(axum::middleware::from_fn_with_state(
            (state.settings.clone(), "codex"),
            settings::virtual_key_auth,
        ));

    let zen_routes = Router::new()
        .route("/v1/models", get(zen::models_handler))
        .route("/v1/models/", get(zen::models_handler))
        .route("/models", get(zen::models_handler))
        .route("/models/", get(zen::models_handler))
        .route("/v1/chat/completions", post(zen::chat_handler))
        .route("/v1/chat/completions/", post(zen::chat_handler))
        .route("/chat/completions", post(zen::chat_handler))
        .route("/chat/completions/", post(zen::chat_handler))
        .layer(axum::middleware::from_fn_with_state(
            (state.settings.clone(), "zen"),
            settings::virtual_key_auth,
        ));

    let login_state = (state.clone(), provider_login::OAuthFlows::default());
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
            "/api/providers/status",
            get(provider_login::status).with_state(login_state.clone()),
        )
        .route(
            "/api/providers/agy/start",
            post(provider_login::agy_start).with_state(login_state.clone()),
        )
        .route(
            "/api/providers/agy/finish",
            post(provider_login::agy_finish).with_state(login_state.clone()),
        )
        .route(
            "/api/providers/agy/poll",
            post(provider_login::agy_poll).with_state(login_state.clone()),
        )
        .route(
            "/api/providers/codex/start",
            post(provider_login::codex_start).with_state(login_state.clone()),
        )
        .route(
            "/api/providers/codex/poll",
            post(provider_login::codex_poll).with_state(login_state),
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
        .fallback(get(web::fallback));
    let dashboard = state.dashboard_auth.protect(dashboard);

    Router::new()
        .route("/health", get(health_handler))
        .nest("/agy", agy_routes)
        .nest("/codex", codex_routes)
        .nest("/zen", zen_routes)
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

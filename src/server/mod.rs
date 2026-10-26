mod agy;
pub(crate) mod auth;
pub(crate) mod codex;
pub(crate) mod sse;
mod zen;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};

use crate::antigravity::{AntigravityClient, AuthManager};
use crate::codex::{CodexAuthManager, CodexClient};

#[derive(Debug, Clone, Default)]
pub(crate) struct ProviderTokens {
    pub(crate) agy: Option<String>,
    pub(crate) codex: Option<String>,
    pub(crate) zen: Option<String>,
}

impl ProviderTokens {
    pub(crate) fn from_env() -> Self {
        let read_token = |keys: &[&str]| -> Option<String> {
            for key in keys {
                if let Ok(val) = std::env::var(key)
                    && !val.trim().is_empty()
                {
                    return Some(val.trim().to_string());
                }
            }
            None
        };

        Self {
            agy: read_token(&["ZERO_G_API_TOKEN_AGY", "ZERO_G_TOKEN_AGY"]),
            codex: read_token(&["ZERO_G_API_TOKEN_CODEX", "ZERO_G_TOKEN_CODEX"]),
            zen: read_token(&["ZERO_G_API_TOKEN_ZEN", "ZERO_G_TOKEN_ZEN"]),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) auth: Option<AuthManager>,
    pub(crate) client: Arc<AntigravityClient>,
    pub(crate) codex_auth: Option<CodexAuthManager>,
    pub(crate) codex_client: Arc<CodexClient>,
    pub(crate) tokens: Arc<ProviderTokens>,
}

pub(crate) fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

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
            state.tokens.agy.clone(),
            auth::auth_middleware,
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
            state.tokens.codex.clone(),
            auth::auth_middleware,
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
            state.tokens.zen.clone(),
            auth::auth_middleware,
        ));

    Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_handler))
        .nest("/agy", agy_routes)
        .nest("/codex", codex_routes)
        .nest("/zen", zen_routes)
        .layer(cors)
        .with_state(state)
}

async fn root_handler() -> impl IntoResponse {
    Json(json!({
        "service": "zero-g",
        "description": "Multi-provider AI proxy (Google Antigravity, OpenAI Codex, OpenCode Zen)",
        "prefixes": [
            "/agy/v1 (Google Antigravity)",
            "/codex/v1 (OpenAI Codex)",
            "/zen/v1 (OpenCode Zen)"
        ]
    }))
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

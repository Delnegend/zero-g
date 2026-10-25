mod chat;
mod models;
mod responses;
mod sse;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};

use crate::antigravity::AntigravityClient;
use crate::auth::AuthManager;
use chat::chat_completions_handler;
use models::list_models_handler;
use responses::responses_handler;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) auth: AuthManager,
    pub(crate) client: Arc<AntigravityClient>,
}

pub(crate) fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_handler))
        // Antigravity (default + /ag prefix)
        .route("/v1/models", get(list_models_handler))
        .route("/models", get(list_models_handler))
        .route("/ag/v1/models", get(list_models_handler))
        .route("/ag/models", get(list_models_handler))
        .route("/v1/chat/completions", post(chat_completions_handler))
        .route("/chat/completions", post(chat_completions_handler))
        .route("/ag/v1/chat/completions", post(chat_completions_handler))
        .route("/ag/chat/completions", post(chat_completions_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/responses", post(responses_handler))
        .route("/ag/v1/responses", post(responses_handler))
        .route("/ag/responses", post(responses_handler))
        // OpenCode Zen free tier
        .route("/zen/v1/models", get(crate::zen::models_handler))
        .route("/zen/models", get(crate::zen::models_handler))
        .route("/zen/v1/chat/completions", post(crate::zen::chat_handler))
        .route("/zen/chat/completions", post(crate::zen::chat_handler))
        .layer(cors)
        .with_state(state)
}

async fn root_handler() -> impl IntoResponse {
    Json(json!({
        "service": "zero-g",
        "description": "Google Antigravity to OpenAI-compatible Proxy",
        "endpoints": [
            "/v1/models",
            "/v1/chat/completions",
            "/v1/responses"
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

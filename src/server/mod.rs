mod chat;
mod models;
mod responses;

use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
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
    pub(crate) api_key: Option<String>,
}

pub(crate) fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_handler))
        .route("/v1/models", get(list_models_handler))
        .route("/models", get(list_models_handler))
        .route("/v1/chat/completions", post(chat_completions_handler))
        .route("/chat/completions", post(chat_completions_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/responses", post(responses_handler))
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

pub(crate) fn check_auth(headers: &HeaderMap, expected_api_key: Option<&str>) -> bool {
    let expected = match expected_api_key {
        Some(k) if !k.is_empty() => k,
        _ => return true,
    };

    let auth_header = headers.get("Authorization").and_then(|h| h.to_str().ok());

    match auth_header {
        Some(h) if h.starts_with("Bearer ") => {
            let token = h["Bearer ".len()..].trim();
            token == expected
        }
        _ => false,
    }
}

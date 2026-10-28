use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;

use crate::openai::ChatCompletionRequest;
use crate::server::AppState;
use crate::server::settings::Permissions;

pub(super) async fn models_handler(Extension(permissions): Extension<Permissions>) -> Json<Value> {
    let mut models = crate::provider::zen::models_json();
    if let Some(data) = models.get_mut("data").and_then(Value::as_array_mut) {
        data.retain(|model| {
            model
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| permissions.allows_model(id))
        });
    }
    Json(models)
}

pub(super) async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    if !permissions.allows_model(&req.model) {
        return (
            StatusCode::FORBIDDEN,
            Json(crate::server::error_body(
                "Model is not allowed for this virtual key",
                "permission_error",
                Some("model_not_allowed"),
            )),
        )
            .into_response();
    }

    let url = match state.settings.setting_get("zen_url").await {
        Ok(value) => value
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:8090/v1".to_string()),
        Err(e) => return settings_error(&e),
    };
    let key = match state.settings.secret_get("zen_api_key").await {
        Ok(value) => value.unwrap_or_else(|| "public".to_string()),
        Err(e) => return settings_error(&e),
    };
    crate::provider::zen::handle_chat(req, &url, &key).await
}

fn settings_error(error: &str) -> Response {
    tracing::error!("Failed to load Zen settings: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(crate::server::error_body(
            "Failed to load Zen settings",
            "server_error",
            Some("internal_error"),
        )),
    )
        .into_response()
}

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;

use crate::openai::ChatCompletionRequest;
use crate::provider::registry::Provider;
use crate::server::AppState;
use crate::server::settings::{self, Permissions, SettingsStore};

/// Build authenticated Zen model and chat routes.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "zen"),
            settings::virtual_key_auth,
        ))
}

/// Return Zen models filtered by virtual-key permissions.
async fn models_handler(Extension(permissions): Extension<Permissions>) -> Json<Value> {
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

/// Check model permissions and forward a Zen chat request.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
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

    send(&state.settings, &provider, &req).await
}

/// Resolve a Zen credential and send a chat request.
pub(crate) async fn send(
    settings: &SettingsStore,
    provider: &Provider,
    req: &ChatCompletionRequest,
) -> Response {
    let credentials = match crate::provider::credentials::list(settings, &provider.id).await {
        Ok(credentials) => credentials,
        Err(e) => return settings_error(&e),
    };
    let mut last = None;
    for credential in credentials {
        let key = match settings
            .secret_get(&format!("credential.{}.api_key", credential.id))
            .await
        {
            Ok(Some(key)) => key,
            Ok(None) => return settings_error("Credential secret is missing"),
            Err(e) => return settings_error(&e),
        };
        let response = crate::provider::zen::handle_chat(req, &provider.base_url, &key).await;
        if !matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            return response;
        }
        last = Some(response);
    }
    last.unwrap_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(crate::server::error_body(
                "No connected Zen credentials",
                "upstream_error",
                Some("no_credentials"),
            )),
        )
            .into_response()
    })
}

/// Convert a settings error into an HTTP response.
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

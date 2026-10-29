use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;

use crate::server::settings::Permissions;

#[derive(Debug)]
pub(crate) struct ProxyError(StatusCode, &'static str, &'static str, &'static str);

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::server::error_body(self.1, self.2, Some(self.3))),
        )
            .into_response()
    }
}

pub(crate) const fn error(
    status: StatusCode,
    message: &'static str,
    kind: &'static str,
    code: &'static str,
) -> ProxyError {
    ProxyError(status, message, kind, code)
}

pub(crate) const fn settings_error() -> ProxyError {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to load OpenAI-compatible provider settings",
        "server_error",
        "internal_error",
    )
}

pub(crate) fn check_model(body: &[u8], permissions: &Permissions) -> Result<(), ProxyError> {
    let value = serde_json::from_slice::<Value>(body).ok();
    let model = value
        .as_ref()
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str);
    let Some(model) = model.filter(|model| !model.is_empty()) else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "A model is required",
            "invalid_request_error",
            "invalid_model",
        ));
    };
    if !permissions.allows_model(model) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
            "permission_error",
            "model_not_allowed",
        ));
    }
    Ok(())
}

pub(crate) fn filter_models(value: &mut Value, permissions: &Permissions) -> bool {
    let Some(data) = value.get_mut("data").and_then(Value::as_array_mut) else {
        return false;
    };
    data.retain(|model| {
        model
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| permissions.allows_model(id))
    });
    true
}

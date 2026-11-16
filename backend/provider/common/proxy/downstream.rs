use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::{error, filter_models};
use crate::server::settings::Permissions;

/// Forward unrestricted model responses unchanged, filtering successful restricted lists.
pub(crate) async fn model_response(
    response: reqwest::Response,
    permissions: &Permissions,
) -> Response {
    if !response.status().is_success() || permissions.models.is_empty() {
        return crate::provider::openai::forward(response);
    }
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let Ok(mut value) = response.json::<Value>().await else {
        return invalid_model_list();
    };
    if !filter_models(&mut value, permissions) {
        return invalid_model_list();
    }
    let mut result = Response::new(Body::from(value.to_string()));
    *result.status_mut() = status;
    if let Some(value) = content_type {
        result.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    result
}

/// Reports an upstream model response that cannot be filtered.
fn invalid_model_list() -> Response {
    error(
        StatusCode::BAD_GATEWAY,
        "Invalid upstream model list",
        "upstream_error",
        "bad_gateway",
    )
    .into_response()
}

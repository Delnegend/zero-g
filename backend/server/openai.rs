use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;

use super::{AppState, settings::Permissions};
use crate::provider::openai;

async fn config(state: &AppState) -> Result<(String, String), ProxyError> {
    let base = state
        .settings
        .setting_get("openai_base_url")
        .await
        .map_err(|_| settings_error())?;
    let Some(base) = base.filter(|value| !value.trim().is_empty()) else {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OpenAI-compatible provider is not configured",
            "server_error",
            "provider_not_configured",
        ));
    };
    let key = state
        .settings
        .secret_get("openai_api_key")
        .await
        .map_err(|_| settings_error())?
        .unwrap_or_default();
    Ok((base, key))
}

#[derive(Debug)]
struct ProxyError(StatusCode, &'static str, &'static str, &'static str);

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(super::error_body(self.1, self.2, Some(self.3))),
        )
            .into_response()
    }
}

const fn error(
    status: StatusCode,
    message: &'static str,
    kind: &'static str,
    code: &'static str,
) -> ProxyError {
    ProxyError(status, message, kind, code)
}

const fn settings_error() -> ProxyError {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to load OpenAI-compatible provider settings",
        "server_error",
        "internal_error",
    )
}

fn check_model(body: &[u8], permissions: &Permissions) -> Result<(), ProxyError> {
    let model = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let Some(model) = model.filter(|model| !model.is_empty()) else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "A model is required",
            "invalid_request_error",
            "invalid_model",
        ));
    };
    if !permissions.allows_model(&model) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
            "permission_error",
            "model_not_allowed",
        ));
    }
    Ok(())
}

fn filter_models(value: &mut Value, permissions: &Permissions) -> bool {
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

pub(super) async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let (base, key) = match config(&state).await {
        Ok(config) => config,
        Err(response) => return response.into_response(),
    };
    let response = match openai::send(
        &base,
        &key,
        "models",
        query.as_deref(),
        Method::GET,
        &headers,
        None,
    )
    .await
    {
        Ok(response) => response,
        Err(message) => {
            return error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
            .into_response();
        }
    };
    if !response.status().is_success() || permissions.models.is_empty() {
        return openai::forward(response);
    }
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let Ok(mut value) = response.json::<Value>().await else {
        return error(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream model list",
            "upstream_error",
            "bad_gateway",
        )
        .into_response();
    };
    if !filter_models(&mut value, &permissions) {
        return error(
            StatusCode::BAD_GATEWAY,
            "Invalid upstream model list",
            "upstream_error",
            "bad_gateway",
        )
        .into_response();
    }
    let mut result = Response::new(Body::from(value.to_string()));
    *result.status_mut() = status;
    if let Some(value) = content_type {
        result.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    result
}

async fn inference(
    state: AppState,
    permissions: Permissions,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
) -> Response {
    if let Err(response) = check_model(&body, &permissions) {
        return response.into_response();
    }
    let (base, key) = match config(&state).await {
        Ok(config) => config,
        Err(response) => return response.into_response(),
    };
    match openai::send(
        &base,
        &key,
        path,
        query.as_deref(),
        Method::POST,
        &headers,
        Some(body),
    )
    .await
    {
        Ok(response) => openai::forward(response),
        Err(message) => error(
            StatusCode::BAD_GATEWAY,
            message,
            "upstream_error",
            "bad_gateway",
        )
        .into_response(),
    }
}

pub(super) async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(state, permissions, query, headers, body, "chat/completions").await
}

pub(super) async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(state, permissions, query, headers, body, "responses").await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policy_checks_model_for_streaming_and_non_streaming_requests() {
        let permissions = Permissions {
            providers: vec![],
            models: vec!["allowed".into()],
        };
        for stream in [true, false] {
            assert!(
                check_model(
                    json!({"model":"allowed", "stream":stream})
                        .to_string()
                        .as_bytes(),
                    &permissions
                )
                .is_ok()
            );
            assert_eq!(
                check_model(
                    json!({"model":"other", "stream":stream})
                        .to_string()
                        .as_bytes(),
                    &permissions
                )
                .unwrap_err()
                .into_response()
                .status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            check_model(br#"{"stream":true}"#, &permissions)
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn model_listing_preserves_metadata_and_filters_unknown_models() {
        let permissions = Permissions {
            providers: vec![],
            models: vec!["allowed".into()],
        };
        let mut value = json!({"object":"list", "data":[{"id":"allowed","extra":1},{"id":"other"},{"name":"missing"}], "next":"page"});
        assert!(filter_models(&mut value, &permissions));
        assert_eq!(
            value,
            json!({"object":"list", "data":[{"id":"allowed","extra":1}], "next":"page"})
        );
        assert!(!filter_models(&mut json!({"unexpected":[]}), &permissions));
    }
}

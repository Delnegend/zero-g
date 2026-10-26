use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

use crate::codex::models::fetch_codex_models;
use crate::openai::ChatCompletionRequest;
use crate::responses::ResponseCreateRequest;
use crate::server::AppState;

pub(crate) fn strip_codex_prefix(model: &str) -> String {
    if let Some(rest) = model.strip_prefix("openai-codex/") {
        rest.to_string()
    } else if let Some(rest) = model.strip_prefix("codex/") {
        rest.to_string()
    } else {
        model.to_string()
    }
}

pub(crate) async fn models_handler(State(state): State<AppState>) -> Response {
    let Some(codex_auth) = &state.codex_auth else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "OpenAI Codex is not configured. Run `zero-g login codex` to authenticate.",
                "server_error",
                Some("codex_not_configured"),
            )),
        )
            .into_response();
    };

    match fetch_codex_models(codex_auth).await {
        Ok(models) => Json(models).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(crate::server::error_body(
                &e,
                "upstream_error",
                Some("bad_gateway"),
            )),
        )
            .into_response(),
    }
}

pub(crate) async fn chat_handler(
    State(state): State<AppState>,
    Json(mut req): Json<ChatCompletionRequest>,
) -> Response {
    let Some(codex_auth) = &state.codex_auth else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "OpenAI Codex is not configured. Run `zero-g login codex` to authenticate.",
                "server_error",
                Some("codex_not_configured"),
            )),
        )
            .into_response();
    };

    let creds = match codex_auth.get_credentials().await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::server::error_body(
                    &e,
                    "server_error",
                    Some("internal_error"),
                )),
            )
                .into_response();
        }
    };

    req.model = strip_codex_prefix(&req.model);
    let is_streaming = req.stream.unwrap_or(false);

    if is_streaming {
        state.codex_client.stream_chat(&creds, &req).await
    } else {
        match state
            .codex_client
            .complete_chat_non_streaming(&creds, &req)
            .await
        {
            Ok(resp) => Json(resp).into_response(),
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(crate::server::error_body(
                    &e,
                    "upstream_error",
                    Some("bad_gateway"),
                )),
            )
                .into_response(),
        }
    }
}

pub(crate) async fn responses_handler(
    State(state): State<AppState>,
    Json(mut req): Json<ResponseCreateRequest>,
) -> Response {
    let Some(codex_auth) = &state.codex_auth else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "OpenAI Codex is not configured. Run `zero-g login codex` to authenticate.",
                "server_error",
                Some("codex_not_configured"),
            )),
        )
            .into_response();
    };

    let creds = match codex_auth.get_credentials().await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::server::error_body(
                    &e,
                    "server_error",
                    Some("internal_error"),
                )),
            )
                .into_response();
        }
    };

    req.model = strip_codex_prefix(&req.model);
    let is_streaming = req.stream.unwrap_or(false);

    if is_streaming {
        state.codex_client.stream_responses(&creds, &req).await
    } else {
        match state
            .codex_client
            .complete_responses_non_streaming(&creds, &req)
            .await
        {
            Ok(resp) => Json(resp).into_response(),
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(crate::server::error_body(
                    &e,
                    "upstream_error",
                    Some("bad_gateway"),
                )),
            )
                .into_response(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_codex_prefix() {
        assert_eq!(strip_codex_prefix("openai-codex/gpt-5.5"), "gpt-5.5");
        assert_eq!(strip_codex_prefix("codex/gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(strip_codex_prefix("gpt-5.5"), "gpt-5.5");
    }
}

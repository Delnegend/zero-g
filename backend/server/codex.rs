use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

use crate::provider::codex::models::fetch_codex_models;
use crate::openai::ChatCompletionRequest;
use crate::responses::ResponseCreateRequest;
use crate::server::AppState;
use crate::server::settings::Permissions;

fn model_forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(crate::server::error_body(
            "Model is not allowed for this virtual key",
            "permission_error",
            Some("model_not_allowed"),
        )),
    )
        .into_response()
}

pub(crate) fn strip_codex_prefix(model: &str) -> String {
    model
        .strip_prefix("openai-codex/")
        .or_else(|| model.strip_prefix("codex/"))
        .unwrap_or(model)
        .to_string()
}

pub(crate) async fn models_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
) -> Response {
    let auth_guard = state.codex_auth.read().await;
    let Some(codex_auth) = auth_guard.as_ref() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "OpenAI Codex is not configured. Authenticate on the dashboard Providers page.",
                "server_error",
                Some("codex_not_configured"),
            )),
        )
            .into_response();
    };

    let result = fetch_codex_models(codex_auth).await;
    drop(auth_guard);
    match result {
        Ok(mut models) => {
            models
                .data
                .retain(|model| permissions.allows_model(&strip_codex_prefix(&model.id)));
            Json(models).into_response()
        }
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
    Extension(permissions): Extension<Permissions>,
    Json(mut req): Json<ChatCompletionRequest>,
) -> Response {
    req.model = strip_codex_prefix(&req.model);
    if !permissions.allows_model(&req.model) {
        return model_forbidden();
    }

    let auth_guard = state.codex_auth.read().await;
    let Some(codex_auth) = auth_guard.as_ref() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "OpenAI Codex is not configured. Authenticate on the dashboard Providers page.",
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
    drop(auth_guard);

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
    Extension(permissions): Extension<Permissions>,
    Json(mut req): Json<ResponseCreateRequest>,
) -> Response {
    req.model = strip_codex_prefix(&req.model);
    if !permissions.allows_model(&req.model) {
        return model_forbidden();
    }

    let auth_guard = state.codex_auth.read().await;
    let Some(codex_auth) = auth_guard.as_ref() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                "OpenAI Codex is not configured. Authenticate on the dashboard Providers page.",
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
    drop(auth_guard);

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
    fn policy_checks_effective_model_for_all_aliases() {
        let permissions = Permissions {
            providers: vec![],
            models: vec!["gpt-5.5".into()],
        };
        for model in ["gpt-5.5", "codex/gpt-5.5", "openai-codex/gpt-5.5"] {
            assert!(permissions.allows_model(&strip_codex_prefix(model)));
        }
        assert!(!permissions.allows_model(&strip_codex_prefix("codex/gpt-5.6")));
    }

    #[test]
    fn test_strip_codex_prefix() {
        assert_eq!(strip_codex_prefix("openai-codex/gpt-5.5"), "gpt-5.5");
        assert_eq!(strip_codex_prefix("codex/gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(strip_codex_prefix("gpt-5.5"), "gpt-5.5");
    }
}

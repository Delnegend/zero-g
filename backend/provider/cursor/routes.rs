use axum::{
    Extension, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};

use crate::{
    openai::{ChatCompletionRequest, ModelListResponse},
    provider::{common::request_identity::RequestIdentity, registry::Provider},
    responses::ResponseCreateRequest,
    server::{
        AppState,
        settings::{self, Permissions, SettingsStore},
    },
};

/// Cursor's virtual-key protected OpenAI-compatible endpoints.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/models/", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/chat/completions/", post(chat))
        .route("/v1/responses", post(responses))
        .route("/v1/responses/", post(responses))
        .layer(axum::middleware::from_fn_with_state(
            false,
            crate::openai::validate_translated_request,
        ))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "cursor"),
            settings::virtual_key_auth,
        ))
}

/// Builds an OpenAI-compatible error response.
fn error(status: StatusCode, message: &str, kind: &str, code: &str) -> Response {
    (
        status,
        Json(crate::server::error_body(message, kind, Some(code))),
    )
        .into_response()
}

/// Removes the optional Cursor provider prefix.
fn strip_prefix(model: &str) -> &str {
    model.strip_prefix("cursor/").unwrap_or(model)
}

/// Lists discovered models permitted by the virtual key.
async fn models(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
) -> Response {
    match super::models::fetch_catalog_pool(
        &state,
        &provider,
        identity.as_ref().map(|Extension(identity)| identity),
    )
    .await
    {
        Ok(mut data) => {
            data.retain(|model| permissions.allows_model(strip_prefix(&model.id)));
            Json(ModelListResponse {
                object: "list".into(),
                data,
            })
            .into_response()
        }
        Err(message) => error(
            StatusCode::BAD_GATEWAY,
            &message,
            "upstream_error",
            "bad_gateway",
        ),
    }
}

/// Handles an `OpenAI` chat completion.
async fn chat(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    complete(
        state,
        permissions,
        provider,
        identity.map(|Extension(identity)| identity),
        req,
        false,
    )
    .await
}

/// Converts a Responses request and delegates to the shared completion path.
async fn responses(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    identity: Option<Extension<RequestIdentity>>,
    Json(req): Json<ResponseCreateRequest>,
) -> Response {
    complete(
        state,
        permissions,
        provider,
        identity.map(|Extension(identity)| identity),
        req.into_chat_request(),
        true,
    )
    .await
}

/// Authorizes the model and tries eligible Cursor credentials.
async fn complete(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    identity: Option<RequestIdentity>,
    mut req: ChatCompletionRequest,
    responses: bool,
) -> Response {
    req.model = strip_prefix(&req.model).to_string();
    if !permissions.allows_model(&req.model) {
        return error(
            StatusCode::FORBIDDEN,
            "Model is not allowed for this virtual key",
            "permission_error",
            "model_not_allowed",
        );
    }
    // Validate before opening a stream or attempting credentials.
    let mut replay = match super::replay::build(&req) {
        Ok(replay) => replay,
        Err(message) => {
            return error(
                StatusCode::BAD_REQUEST,
                &message,
                "invalid_request_error",
                "invalid_request",
            );
        }
    };
    let credentials = match crate::provider::credentials::list(&state.settings, &provider.id).await
    {
        Ok(credentials) => credentials,
        Err(message) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &message,
                "server_error",
                "internal_error",
            );
        }
    };
    let mut last =
        "Cursor is not configured. Authenticate on the dashboard Providers page.".to_string();
    for credential in credentials {
        let token =
            match super::auth::access_token(&state.settings, &provider.id, &credential.id).await {
                Ok(token) => token,
                Err(message) => {
                    last = message;
                    continue;
                }
            };
        match super::transport::start(&provider.base_url, &token, replay).await {
            Ok(rx) => {
                let response = super::translation::respond(
                    rx,
                    req.model,
                    req.stream.unwrap_or(false),
                    responses,
                    req.stream_options
                        .as_ref()
                        .and_then(|options| options.include_usage)
                        .unwrap_or(false),
                )
                .await;
                if response.status().is_success()
                    && let Some(identity) = &identity
                {
                    identity.set_credential(&credential.id, &credential.name);
                }
                return response;
            }
            Err(failure) => {
                let retry = failure.retryable();
                last = failure.message;
                if !retry {
                    break;
                }
                replay = match super::replay::build(&req) {
                    Ok(replay) => replay,
                    Err(message) => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            &message,
                            "invalid_request_error",
                            "invalid_request",
                        );
                    }
                };
            }
        }
    }
    error(
        StatusCode::BAD_GATEWAY,
        &last,
        "upstream_error",
        "bad_gateway",
    )
}

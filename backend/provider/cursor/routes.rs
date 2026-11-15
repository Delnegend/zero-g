use axum::{
    Extension, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};

use crate::provider::common::cooldown;
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
    Box::pin(complete(
        state,
        permissions,
        provider,
        identity.map(|Extension(identity)| identity),
        req,
        false,
    ))
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
    Box::pin(complete(
        state,
        permissions,
        provider,
        identity.map(|Extension(identity)| identity),
        req.into_chat_request(),
        true,
    ))
    .await
}

/// Authorizes the model and tries eligible Cursor credentials.
#[allow(clippy::too_many_lines)]
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
    let credentials = match crate::provider::credentials::list(&state.settings, &provider).await {
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
    let mut earliest = None;
    let mut attempted = false;
    let mut headers = reqwest::header::HeaderMap::new();
    let mut limited = false;
    for credential in credentials {
        if let Some(wait) = cooldown::remaining(&credential.id) {
            earliest = Some(earliest.map_or(wait, |old: std::time::Duration| old.min(wait)));
            continue;
        }
        attempted = true;
        let token =
            match super::auth::access_token(&state.settings, &provider.id, &credential.id).await {
                Ok(token) => token,
                Err(message) => {
                    headers.clear();
                    limited = false;
                    last = message;
                    continue;
                }
            };
        match super::transport::start(&provider.base_url, &token, replay).await {
            Ok(rx) => {
                let response = crate::provider::common::translation::respond(
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
                if response.status().is_success() {
                    cooldown::clear(&credential.id);
                }
                if response.status().is_success()
                    && let Some(identity) = &identity
                {
                    identity.set_credential(&credential.id, &credential.name);
                }
                return response;
            }
            Err(mut failure) => {
                let retry = failure.retryable();
                limited = failure.status == Some(StatusCode::TOO_MANY_REQUESTS);
                if limited {
                    let wait = cooldown::wait_for(&failure.headers);
                    cooldown::start(&credential.id, wait);
                    if !failure
                        .headers
                        .contains_key(axum::http::header::RETRY_AFTER)
                    {
                        let value = cooldown::retry_after_secs(wait).to_string();
                        if let Ok(value) = value.parse() {
                            failure
                                .headers
                                .insert(axum::http::header::RETRY_AFTER, value);
                        }
                    }
                }
                headers = *failure.headers;
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
    if !attempted && let Some(wait) = earliest {
        let mut response = error(
            StatusCode::TOO_MANY_REQUESTS,
            "All Cursor accounts are rate limited",
            "rate_limit_error",
            "rate_limited",
        );
        if let Ok(value) = cooldown::retry_after_secs(wait).to_string().parse() {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value);
        }
        return response;
    }
    let (status, kind, code) = if limited {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "rate_limited",
        )
    } else {
        (StatusCode::BAD_GATEWAY, "upstream_error", "bad_gateway")
    };
    let mut response = error(status, &last, kind, code);
    cooldown::copy_rate_limit_headers(&headers, response.headers_mut());
    response
}

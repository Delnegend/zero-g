use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};

use super::INFERENCE_BODY_LIMIT;
use crate::provider::claude::passthrough;
use crate::provider::common::{request_identity::RequestIdentity, session::RequestSession};
use crate::server::{
    AppState,
    settings::{Permissions, virtual_key},
};

/// Forward an Anthropic Messages request to the Claude provider unchanged where possible.
#[utoipa::path(post, path = "/v1/messages", operation_id = "createMessage",
    params(("x-api-key" = Option<String>, Header, description = "Virtual key; `Authorization: Bearer` is also accepted")),
    request_body(content = Value, description = "Anthropic Messages request; the model may be plain or prefixed with `claude/`"),
    responses((status = 200, description = "Anthropic Messages response or event stream, passed through unchanged", body = Value),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 403, description = "Provider or model not allowed for this key", body = Value),
        (status = 404, description = "Claude provider not configured", body = Value),
        (status = 413, description = "Request body too large", body = Value)))]
pub(super) async fn messages(State(state): State<AppState>, request: Request) -> Response {
    anthropic(state, request).await
}

/// Forward an Anthropic token-counting request to the Claude provider.
#[utoipa::path(post, path = "/v1/messages/count_tokens", operation_id = "countMessageTokens",
    params(("x-api-key" = Option<String>, Header, description = "Virtual key; `Authorization: Bearer` is also accepted")),
    request_body(content = Value, description = "Anthropic token-counting request"),
    responses((status = 200, description = "Anthropic token count, passed through unchanged", body = Value),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 403, description = "Provider or model not allowed for this key", body = Value),
        (status = 404, description = "Claude provider not configured", body = Value)))]
pub(super) async fn count_tokens(State(state): State<AppState>, request: Request) -> Response {
    anthropic(state, request).await
}

/// Hand an authenticated request and its bounded body to the pass-through.
async fn anthropic(state: AppState, request: Request) -> Response {
    let Some(permissions) = request.extensions().get::<Permissions>().cloned() else {
        return passthrough::error(StatusCode::UNAUTHORIZED, "A virtual key is required");
    };
    let identity = request.extensions().get::<RequestIdentity>().cloned();
    let session = virtual_key(request.headers())
        .map(|token| RequestSession::new(request.headers(), token, "claude").id)
        .unwrap_or_default();
    let path = request.uri().path_and_query().map_or_else(
        || request.uri().path().to_owned(),
        |p| p.as_str().to_owned(),
    );
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, INFERENCE_BODY_LIMIT).await else {
        return passthrough::error(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
    };
    passthrough::messages(
        &state,
        &permissions,
        identity.as_ref(),
        &session,
        &parts.headers,
        &path,
        body,
    )
    .await
}

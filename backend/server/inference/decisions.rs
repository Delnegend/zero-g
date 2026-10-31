use super::{InferenceRequest, dispatch::dispatch};
use crate::server::AppState;
use axum::{
    Router,
    extract::{Request, State},
    response::Response,
};

/// Dispatch an `OpenRouter` Decisions API request.
#[utoipa::path(post, path = "/v1/decisions", operation_id = "createDecision",
    params(("Authorization" = String, Header, description = "Bearer virtual key")),
    request_body = InferenceRequest,
    responses((status = 200, description = "Provider decision response", body = Value),
        (status = 400, description = "Invalid request", body = Value),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 403, description = "Key does not permit provider or model", body = Value),
        (status = 404, description = "Provider not found or does not support Decisions"),
        (status = 413, description = "Request body too large", body = Value),
        (status = 500, description = "Gateway error", body = Value),
        (status = 502, description = "Provider error", body = Value)))]
pub(super) async fn inference_decisions(
    state: State<AppState>,
    router: axum::Extension<Router>,
    request: Request,
) -> Response {
    dispatch(state, router, request).await
}

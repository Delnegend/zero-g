use axum::{http::StatusCode, response::IntoResponse};

/// Return a successful health probe response.
#[utoipa::path(get, path = "/health", operation_id = "health",
    responses((status = 200, description = "The server is running")))]
pub(super) async fn health_handler() -> impl IntoResponse {
    StatusCode::OK
}

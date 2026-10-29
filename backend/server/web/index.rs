use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Serve the dashboard entry point or report a missing build.
pub(in crate::server) async fn index() -> Response {
    super::serve_asset("dist/index.html").unwrap_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Build the dashboard first with `just frontend-build`.",
        )
            .into_response()
    })
}

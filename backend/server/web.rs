use axum::body::Bytes;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;
use std::borrow::Cow;

/// Dashboard build files embedded in the executable.
#[derive(Embed)]
#[folder = "frontend/"]
#[include = "dist/**"]
struct DashboardAssets;

/// Serve the dashboard entry point or report a missing build.
pub(super) async fn index() -> Response {
    serve_asset("dist/index.html").unwrap_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Build the dashboard first with `just frontend-build`.",
        )
            .into_response()
    })
}

/// Serve static files or the dashboard for client-side routes.
pub(super) async fn fallback(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if crate::provider::is_provider_path(uri.path())
        || path == "health"
        || path.starts_with("health/")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(response) = serve_asset(&format!("dist/{path}")) {
        return response;
    }
    if path.starts_with("assets/")
        || path
            .rsplit('/')
            .next()
            .is_some_and(|part| part.contains('.'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    index().await
}

/// Return an embedded asset with its inferred content type.
fn serve_asset(path: &str) -> Option<Response> {
    let asset = DashboardAssets::get(path)?;
    let content_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let data = match asset.data {
        Cow::Borrowed(bytes) => Bytes::from_static(bytes),
        Cow::Owned(bytes) => Bytes::from(bytes),
    };
    Some(([(header::CONTENT_TYPE, content_type)], data).into_response())
}

use axum::{
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
};

/// Serve static files or the dashboard for client-side routes.
pub(in crate::server) async fn fallback(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if crate::provider::is_provider_path(uri.path())
        || path == "v1"
        || path.starts_with("v1/")
        || path == "health"
        || path.starts_with("health/")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(response) = super::serve_asset(&format!("dist/{path}")) {
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
    super::index().await
}

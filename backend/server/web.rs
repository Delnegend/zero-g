use axum::body::Bytes;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;
use std::borrow::Cow;

#[derive(Embed)]
#[folder = "frontend/"]
#[include = "dist/**"]
struct DashboardAssets;

pub(crate) async fn index() -> Response {
    serve_asset("dist/index.html").unwrap_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Build the dashboard first with `just frontend-build`.",
        )
            .into_response()
    })
}

pub(crate) async fn fallback(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if ["agy", "codex", "zen", "health"]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
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

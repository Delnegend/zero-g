use axum::body::Bytes;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;
use std::borrow::Cow;

/// Client-side route fallback.
mod fallback;
/// Dashboard entry point.
mod index;

pub(super) use fallback::fallback;
pub(super) use index::index;

/// Dashboard build files embedded in the executable.
#[derive(Embed)]
#[folder = "frontend/"]
#[include = "dist/**"]
struct DashboardAssets;

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

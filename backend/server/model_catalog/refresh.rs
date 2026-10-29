use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::{AppState, ModelCatalog, normalize_catalog, refresh_catalog};

/// Refresh the catalog on demand and return the result.
#[utoipa::path(post, path = "/api/model-catalog/refresh", operation_id = "refreshModelCatalog",
    responses((status = 200, body = ModelCatalog),
        (status = 500, description = "Catalog refresh failed", body = String, content_type = "text/plain")))]
pub(super) async fn post_refresh(State(state): State<AppState>) -> Response {
    let result = match refresh_catalog(&state).await {
        Ok(catalog) => Ok(normalize_catalog(&state.settings, catalog).await),
        Err(error) => Err(error),
    };
    result.map_or_else(
        |_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to refresh model catalog",
            )
                .into_response()
        },
        |catalog| Json(catalog).into_response(),
    )
}

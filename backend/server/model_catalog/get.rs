use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::{AppState, ModelCatalog, load, normalize_catalog};

/// Return the current reconciled catalog.
#[utoipa::path(get, path = "/api/model-catalog", operation_id = "getModelCatalog",
    responses((status = 200, body = ModelCatalog),
        (status = 500, description = "Catalog load failed", body = String, content_type = "text/plain")))]
pub(super) async fn get_catalog(State(state): State<AppState>) -> Response {
    let result = match load(&state.settings).await {
        Ok(catalog) => Ok(normalize_catalog(&state.settings, catalog).await),
        Err(error) => Err(error),
    };
    result.map_or_else(
        |_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load model catalog",
            )
                .into_response()
        },
        |catalog| Json(catalog).into_response(),
    )
}

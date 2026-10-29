/// Serve the same `OpenAPI` contract used to generate the frontend client.
#[utoipa::path(get, path = "/api/openapi.json", operation_id = "getOpenApi",
    responses((status = 200, description = "OpenAPI document", body = serde_json::Value),
        (status = 401, description = "Dashboard authentication required")))]
pub(super) async fn openapi() -> axum::Json<utoipa::openapi::OpenApi> {
    axum::Json(crate::contract::openapi())
}

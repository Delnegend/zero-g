/// Serve the API reference using the locally bundled `ReDoc` script.
#[utoipa::path(get, path = "/redoc", operation_id = "apiReference",
    responses((status = 200, description = "ReDoc HTML", body = String, content_type = "text/html"),
        (status = 401, description = "Dashboard authentication required")))]
pub(super) async fn redoc() -> axum::response::Html<&'static str> {
    axum::response::Html(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Zero-G API</title></head><body style="margin:0"><redoc spec-url="/api/openapi.json"></redoc><script src="/redoc.standalone.js"></script></body></html>"#,
    )
}

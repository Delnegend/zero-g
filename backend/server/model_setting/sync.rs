use super::datasheet::{SETTINGS_CHANGED, sync_datasheets};
use super::{ModelSettingsError, ModelSyncResult, error};
use crate::server::AppState;
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Handle an on-demand datasheet synchronization.
#[utoipa::path(post, path = "/api/model-settings/sync", operation_id = "syncModelSettings",
    responses((status = 200, body = ModelSyncResult), (status = 409, body = ModelSettingsError),
        (status = 502, body = ModelSettingsError)))]
pub(super) async fn sync(State(state): State<AppState>) -> Response {
    match sync_datasheets(&state.settings).await {
        Ok(()) => Json(ModelSyncResult { ok: true }).into_response(),
        Err(message) if message == SETTINGS_CHANGED => error(StatusCode::CONFLICT, &message),
        Err(message) => error(StatusCode::BAD_GATEWAY, &message),
    }
}

/// HTTP behavior for synchronizing datasheets.
#[cfg(test)]
mod tests {
    use super::super::{router, test_support};
    use axum::{
        Json, Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::get,
    };
    use serde_json::json;
    use tower::ServiceExt;

    /// A successful sync saves both sheets; an upstream failure keeps them.
    #[tokio::test]
    async fn sync_saves_sheets_and_preserves_them_on_failure() {
        let upstream = Router::new().route(
            "/data",
            get(|| async { Json(json!({"model":{"price":1}})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let state = test_support::state().await;
        let app = router(state.clone());
        let put = |parameters_url: String| {
            Request::builder().method("PUT").uri("/api/model-settings")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"pricing_url":format!("http://{address}/data"),"parameters_url":parameters_url,"pricing_interval_hours":0,"discovery_interval_minutes":0}).to_string())).unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(put(format!("http://{address}/data")))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/api/model-settings/sync")
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::OK
        );
        let pricing = state
            .settings
            .setting_get("model_pricing_datasheet")
            .await
            .unwrap()
            .unwrap();
        let parameters = state
            .settings
            .setting_get("model_parameters_datasheet")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pricing).unwrap(),
            json!({"model":{"price":1}})
        );
        assert_eq!(parameters, pricing);
        assert_eq!(
            app.clone()
                .oneshot(put(format!("http://{address}/missing")))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.oneshot(request()).await.unwrap().status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            state
                .settings
                .setting_get("model_pricing_datasheet")
                .await
                .unwrap()
                .unwrap(),
            pricing
        );
        assert_eq!(
            state
                .settings
                .setting_get("model_parameters_datasheet")
                .await
                .unwrap()
                .unwrap(),
            parameters
        );
        server.abort();
    }
}

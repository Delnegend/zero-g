use super::{ModelSettings, ModelSettingsError, error, load_settings};
use crate::server::AppState;
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Return the current model synchronization settings.
#[utoipa::path(get, path = "/api/model-settings", operation_id = "getModelSettings",
    responses((status = 200, body = ModelSettings), (status = 500, body = ModelSettingsError)))]
pub(super) async fn get_settings(State(state): State<AppState>) -> Response {
    load_settings(&state.settings).await.map_or_else(
        |_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load model settings",
            )
        },
        |settings| Json(settings).into_response(),
    )
}

/// HTTP behavior for reading model settings.
#[cfg(test)]
mod tests {
    use super::super::{CONFIG_KEY, router, test_support};
    use crate::storage::Setting;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    /// A missing configuration uses defaults; a corrupt one reports an error.
    #[tokio::test]
    async fn reads_defaults_and_reports_corrupt_settings() {
        let state = test_support::state().await;
        let app = router(state.clone());
        let request = || {
            Request::builder()
                .uri("/api/model-settings")
                .body(Body::empty())
                .unwrap()
        };
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["pricing_interval_hours"], 24);
        assert_eq!(body["discovery_interval_minutes"], 60);
        Setting::upsert_by_name(CONFIG_KEY)
            .value("{invalid")
            .exec(&mut (*state.settings.database.db).clone())
            .await
            .unwrap();
        let response = app.oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}

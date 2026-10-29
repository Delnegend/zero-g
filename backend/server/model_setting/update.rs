use super::{CONFIG_KEY, ModelSettings, ModelSettingsError, error};
use crate::server::AppState;
use crate::storage::Setting;
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Validate and persist model synchronization settings.
#[utoipa::path(put, path = "/api/model-settings", operation_id = "updateModelSettings",
    request_body = ModelSettings,
    responses((status = 200, body = ModelSettings), (status = 400, body = ModelSettingsError),
        (status = 500, body = ModelSettingsError)))]
pub(super) async fn put_settings(
    State(state): State<AppState>,
    Json(mut settings): Json<ModelSettings>,
) -> Response {
    let defaults = ModelSettings::default();
    if settings.pricing_url.trim().is_empty() {
        settings.pricing_url = defaults.pricing_url;
    }
    if settings.parameters_url.trim().is_empty() {
        settings.parameters_url = defaults.parameters_url;
    }
    if let Err(message) = settings.validate() {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let Ok(value) = serde_json::to_string(&settings) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to serialize model settings",
        );
    };
    let result = state
        .settings
        .database
        .retry(|| async {
            let mut db = (*state.settings.database.db).clone();
            let mut tx = db.transaction().await?;
            Setting::upsert_by_name(CONFIG_KEY)
                .value(value.as_str())
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .await;
    if result.is_err() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to save model settings",
        );
    }
    Json(settings).into_response()
}

/// HTTP behavior for updating model settings.
#[cfg(test)]
mod tests {
    use super::super::{router, test_support};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Invalid updates are rejected without replacing the last valid settings.
    #[tokio::test]
    async fn validates_and_persists_updates() {
        let app = router(test_support::state().await);
        let put = |body: Value| {
            Request::builder()
                .method("PUT")
                .uri("/api/model-settings")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let valid = json!({"pricing_url":"", "parameters_url":"", "pricing_interval_hours":0, "discovery_interval_minutes":5});
        let response = app.clone().oneshot(put(valid.clone())).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let saved: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(saved["pricing_url"], "https://getbifrost.ai/datasheet");
        assert_eq!(
            saved["parameters_url"],
            "https://getbifrost.ai/datasheet/model-parameters"
        );
        for bad_url in [
            "ftp://example.com",
            "https://user:secret@example.com",
            "https://example.com/#fragment",
        ] {
            let mut invalid = valid.clone();
            invalid["pricing_url"] = json!(bad_url);
            assert_eq!(
                app.clone().oneshot(put(invalid)).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
        let mut invalid = valid;
        invalid["pricing_interval_hours"] = json!(u64::MAX);
        assert_eq!(
            app.clone().oneshot(put(invalid)).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/model-settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let current: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(current, saved);
    }
}

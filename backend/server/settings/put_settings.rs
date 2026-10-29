use super::{SettingsResponse, SettingsStore, UpdateSettings, get_settings, internal_error};
use crate::database::transaction_with_retry;
use crate::storage::{Secret, Setting};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

/// Validates and atomically applies supplied settings.
#[utoipa::path(
    put,
    path = "/api/settings",
    operation_id = "updateSettings",
    request_body = UpdateSettings,
    responses(
        (status = 200, description = "Updated settings", body = SettingsResponse),
        (status = 400, description = "Invalid log retention", body = String, content_type = "text/plain"),
        (status = 500, description = "Settings store error")
    )
)]
pub(crate) async fn put_settings(
    State(store): State<SettingsStore>,
    Json(input): Json<UpdateSettings>,
) -> Response {
    if input
        .log_retention_days
        .is_some_and(|days| !(1..=36500).contains(&days))
    {
        return (StatusCode::BAD_REQUEST, "Invalid log retention").into_response();
    }
    let values = [
        ("oauth_client_id", input.oauth_client_id),
        (
            "log_retention_days",
            input.log_retention_days.map(|v| v.to_string()),
        ),
    ];
    let secret = match input.oauth_client_secret {
        Some(value) if value.is_empty() => Some(None),
        Some(value) => match store.encrypt("oauth_client_secret", &value) {
            Ok(encrypted) => Some(Some(encrypted)),
            Err(err) => return internal_error(&err),
        },
        None => None,
    };
    let result = transaction_with_retry!(store.database, |mut tx| {
        for (name, value) in &values {
            if let Some(value) = value {
                Setting::upsert_by_name(*name)
                    .value(value.as_str())
                    .exec(&mut tx)
                    .await?;
            }
        }
        if let Some(value) = &secret {
            if let Some(encrypted) = value {
                Secret::upsert_by_name("oauth_client_secret")
                    .value(encrypted.clone())
                    .exec(&mut tx)
                    .await?;
            } else {
                Secret::delete_by_name(&mut tx, "oauth_client_secret").await?;
            }
        }
        tx.commit().await?;
        Ok(())
    });
    if let Err(err) = result {
        return internal_error(&err.to_string());
    }
    get_settings(State(store)).await
}

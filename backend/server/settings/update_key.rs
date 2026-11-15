use super::{SettingsStore, UpdateKey, internal_error, providers_exist, unix_now};
use crate::database::transaction_with_retry;
use crate::storage::VirtualKey;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

/// Changes configuration or expiration of a non-revoked key.
#[utoipa::path(patch, path = "/api/keys/{id}", operation_id = "updateKey",
    params(("id" = String, Path, description = "Virtual key ID")),
    request_body = UpdateKey,
    responses((status = 204, description = "Key updated"),
        (status = 400, description = "Invalid key configuration or past expiration"),
        (status = 404, description = "Key not found"),
        (status = 409, description = "Key is revoked"),
        (status = 500, description = "Settings store error")))]
pub(in crate::server) async fn update_key(
    State(store): State<SettingsStore>,
    Path(id): Path<String>,
    Json(input): Json<UpdateKey>,
) -> Response {
    let result = transaction_with_retry!(store.database, |mut tx| {
        let Some(mut row) = VirtualKey::filter_by_id(id.as_str())
            .first()
            .exec(&mut tx)
            .await?
        else {
            return Ok(StatusCode::NOT_FOUND);
        };
        if row.revoked_at.is_some() {
            return Ok(StatusCode::CONFLICT);
        }
        let name = input.name.as_deref().map(str::trim);
        if name.is_some_and(str::is_empty)
            || input
                .expires_at
                .flatten()
                .is_some_and(|expires| expires <= unix_now())
        {
            return Ok(StatusCode::BAD_REQUEST);
        }
        if let Some(providers) = &input.providers
            && !providers_exist(&mut tx, providers).await?
        {
            return Ok(StatusCode::BAD_REQUEST);
        }
        if input
            .models
            .as_ref()
            .is_some_and(|models| models.iter().any(|m| m.trim().is_empty()))
        {
            return Ok(StatusCode::BAD_REQUEST);
        }
        let (Ok(providers), Ok(models)) = (
            input
                .providers
                .as_ref()
                .map(serde_json::to_string)
                .transpose(),
            input.models.as_ref().map(serde_json::to_string).transpose(),
        ) else {
            return Ok(StatusCode::BAD_REQUEST);
        };
        // Only supplied fields are written; Toasty rejects an update with no
        // assignments, so an empty patch writes nothing.
        if name.is_none() && input.expires_at.is_none() && providers.is_none() && models.is_none() {
            return Ok(StatusCode::NO_CONTENT);
        }
        let mut update = row.update();
        if let Some(name) = name {
            update.set_name(name.to_owned());
        }
        if let Some(expires_at) = input.expires_at {
            update.set_expires_at(expires_at);
        }
        if let Some(providers) = providers {
            update.set_providers(providers);
        }
        if let Some(models) = models {
            update.set_models(models);
        }
        update.exec(&mut tx).await?;
        tx.commit().await?;
        Ok(StatusCode::NO_CONTENT)
    });
    match result {
        Ok(status) => status.into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

use super::{SettingsStore, UpdateKey, internal_error, unix_now};
use crate::database::transaction_with_retry;
use crate::storage::VirtualKey;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

/// Changes or clears the expiration of a non-revoked key.
#[utoipa::path(patch, path = "/api/keys/{id}", operation_id = "updateKey",
    params(("id" = String, Path, description = "Virtual key ID")),
    request_body = UpdateKey,
    responses((status = 204, description = "Expiration updated"),
        (status = 400, description = "Expiration must be in the future"),
        (status = 404, description = "Key not found"),
        (status = 409, description = "Key is revoked"),
        (status = 500, description = "Settings store error")))]
pub(in crate::server) async fn update_key(
    State(store): State<SettingsStore>,
    Path(id): Path<String>,
    Json(input): Json<UpdateKey>,
) -> Response {
    let result = transaction_with_retry!(store.database, |mut tx| {
        if input
            .expires_at
            .is_some_and(|expires| expires <= unix_now())
        {
            return Ok(StatusCode::BAD_REQUEST);
        }
        let Some(row) = VirtualKey::filter_by_id(id.as_str())
            .first()
            .exec(&mut tx)
            .await?
        else {
            return Ok(StatusCode::NOT_FOUND);
        };
        if row.revoked_at.is_some() {
            return Ok(StatusCode::CONFLICT);
        }
        VirtualKey::upsert_by_id(row.id.as_str())
            .name(row.name)
            .key_hash(row.key_hash)
            .providers(row.providers)
            .models(row.models)
            .created_at(row.created_at)
            .expires_at(input.expires_at)
            .revoked_at(row.revoked_at)
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(StatusCode::NO_CONTENT)
    });
    match result {
        Ok(status) => status.into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

use super::{SettingsStore, internal_error};
use crate::database::transaction_with_retry;
use crate::storage::VirtualKey;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Deletes a virtual key by ID.
#[utoipa::path(delete, path = "/api/keys/{id}", operation_id = "revokeKey",
    params(("id" = String, Path, description = "Virtual key ID")),
    responses((status = 204, description = "Key revoked"),
        (status = 404, description = "Key not found"), (status = 500, description = "Settings store error")))]
pub(in crate::server) async fn revoke_key(
    State(store): State<SettingsStore>,
    Path(id): Path<String>,
) -> Response {
    let result = transaction_with_retry!(store.database, |mut tx| {
        let Some(row) = VirtualKey::filter_by_id(id.as_str())
            .first()
            .exec(&mut tx)
            .await?
        else {
            return Ok(false);
        };
        row.delete().exec(&mut tx).await?;
        tx.commit().await?;
        Ok(true)
    });
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

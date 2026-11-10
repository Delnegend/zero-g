use super::{SettingsStore, UpdateKey, internal_error, unix_now};
use crate::database::transaction_with_retry;
use crate::storage::{ProviderRecord, VirtualKey};
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

        let new_name = match &input.name {
            Some(name) => {
                let trimmed = name.trim().to_string();
                if trimmed.is_empty() {
                    return Ok(StatusCode::BAD_REQUEST);
                }
                trimmed
            }
            None => row.name,
        };

        let new_expires_at = match input.expires_at {
            Some(Some(expires)) => {
                if expires <= unix_now() {
                    return Ok(StatusCode::BAD_REQUEST);
                }
                Some(expires)
            }
            Some(None) => None,
            None => row.expires_at,
        };

        let new_providers = match &input.providers {
            Some(providers) => {
                for provider_id in providers {
                    let provider_row = ProviderRecord::filter_by_id(provider_id.as_str())
                        .first()
                        .exec(&mut tx)
                        .await?;
                    if provider_row.is_none() {
                        return Ok(StatusCode::BAD_REQUEST);
                    }
                }
                match serde_json::to_string(providers) {
                    Ok(v) => v,
                    Err(_) => return Ok(StatusCode::BAD_REQUEST),
                }
            }
            None => row.providers,
        };

        let new_models = match &input.models {
            Some(models) => {
                if models.iter().any(|m| m.trim().is_empty()) {
                    return Ok(StatusCode::BAD_REQUEST);
                }
                match serde_json::to_string(models) {
                    Ok(v) => v,
                    Err(_) => return Ok(StatusCode::BAD_REQUEST),
                }
            }
            None => row.models,
        };

        VirtualKey::upsert_by_id(row.id.as_str())
            .name(new_name)
            .key_hash(row.key_hash)
            .providers(new_providers)
            .models(new_models)
            .created_at(row.created_at)
            .expires_at(new_expires_at)
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

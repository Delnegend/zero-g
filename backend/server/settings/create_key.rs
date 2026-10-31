use super::{CreateKey, CreatedKey, SettingsStore, internal_error};
use crate::database::transaction_with_retry;
use crate::storage::{ProviderRecord, VirtualKey};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use base64::Engine as _;
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};

/// Creates a virtual key and returns its token once.
#[utoipa::path(post, path = "/api/keys", operation_id = "createKey",
    request_body = CreateKey,
    responses((status = 201, body = CreatedKey),
        (status = 400, description = "Invalid key", body = String, content_type = "text/plain"),
        (status = 500, description = "Settings store error")))]
pub(in crate::server) async fn create_key(
    State(store): State<SettingsStore>,
    Json(input): Json<CreateKey>,
) -> Response {
    if input.name.trim().is_empty() || input.models.iter().any(|m| m.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid key name, provider, or model",
        )
            .into_response();
    }
    let mut random = [0u8; 32];
    if SystemRandom::new().fill(&mut random).is_err() {
        return internal_error("Secure randomness unavailable");
    }
    let token = format!(
        "zg-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
    );
    let hash = digest::digest(&digest::SHA256, token.as_bytes());
    let id = uuid::Uuid::new_v4().to_string();
    let created_at = super::unix_now();
    if input
        .expires_at
        .is_some_and(|expires| expires <= created_at)
    {
        return (StatusCode::BAD_REQUEST, "Expiration must be in the future").into_response();
    }
    let providers = match serde_json::to_string(&input.providers) {
        Ok(v) => v,
        Err(e) => return internal_error(&e.to_string()),
    };
    let models = match serde_json::to_string(&input.models) {
        Ok(v) => v,
        Err(e) => return internal_error(&e.to_string()),
    };
    let result = transaction_with_retry!(store.database, |mut tx| {
        if input
            .expires_at
            .is_some_and(|expires| expires <= super::unix_now())
        {
            return Ok(Some("Expiration must be in the future"));
        }
        for provider_id in &input.providers {
            let row = ProviderRecord::filter_by_id(provider_id.as_str())
                .first()
                .exec(&mut tx)
                .await?;
            if !matches!(row, Some(row) if !row.deleted) {
                return Ok(Some("Invalid key provider"));
            }
        }
        toasty::create!(VirtualKey {
            id: id.clone(),
            name: input.name.clone(),
            key_hash: hash.as_ref().to_vec(),
            providers: providers.clone(),
            models: models.clone(),
            created_at,
            expires_at: input.expires_at,
            revoked_at: None
        })
        .exec(&mut tx)
        .await?;
        tx.commit().await?;
        Ok(None)
    });
    match result {
        Ok(None) => (
            StatusCode::CREATED,
            Json(CreatedKey {
                id,
                name: input.name,
                providers: input.providers,
                models: input.models,
                created_at,
                expires_at: input.expires_at,
                revoked_at: None,
                key: token,
            }),
        )
            .into_response(),
        Ok(Some(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

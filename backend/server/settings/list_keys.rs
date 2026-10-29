use super::{KeyInfo, KeyList, SettingsStore, internal_error};
use crate::storage::VirtualKey;
use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};

/// Lists virtual key metadata without exposing key tokens.
#[utoipa::path(get, path = "/api/keys", operation_id = "listKeys",
    responses((status = 200, body = KeyList), (status = 500, description = "Settings store error")))]
pub(in crate::server) async fn list_keys(State(store): State<SettingsStore>) -> Response {
    /// Loads and decodes stored virtual key metadata.
    async fn list(store: &SettingsStore) -> Result<Vec<KeyInfo>, String> {
        let rows = VirtualKey::all()
            .order_by((
                VirtualKey::fields().created_at().desc(),
                VirtualKey::fields().id().asc(),
            ))
            .exec(&mut (*store.database.db).clone())
            .await
            .map_err(|e| e.to_string())?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(KeyInfo {
                id: row.id,
                name: row.name,
                providers: serde_json::from_str(&row.providers).map_err(|e| e.to_string())?,
                models: serde_json::from_str(&row.models).map_err(|e| e.to_string())?,
                created_at: row.created_at,
            });
        }
        Ok(keys)
    }
    match list(&store).await {
        Ok(keys) => Json(KeyList { keys }).into_response(),
        Err(err) => internal_error(&err),
    }
}

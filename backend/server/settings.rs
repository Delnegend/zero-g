use crate::storage::{ProviderRecord, Secret, Setting, VirtualKey};
use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{
    aead, digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use utoipa::ToSchema;

use crate::database::Database;

/// Reserved secret name for checking the configured encryption key.
const SENTINEL: &str = "__zero_g_encryption_key_check";
/// Expected plaintext of the encryption-key sentinel.
const CHECK: &str = "zero-g encryption key check v1";

/// Database-backed settings and secret encryption context.
#[derive(Clone)]
pub(crate) struct SettingsStore {
    /// Database used for settings, secrets, and virtual keys.
    pub(crate) database: Database,
    /// AES-256-GCM key used to protect stored secrets.
    encryption_key: [u8; 32],
}

/// Provider and model restrictions associated with a virtual key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Permissions {
    /// Allowed provider IDs; an empty list allows all providers.
    pub(crate) providers: Vec<String>,
    /// Allowed model IDs; an empty list allows all models.
    pub(crate) models: Vec<String>,
}

impl Permissions {
    /// Returns whether the model is allowed by this key's restrictions.
    pub(crate) fn allows_model(&self, model: &str) -> bool {
        self.models.is_empty() || self.models.iter().any(|allowed| allowed == model)
    }
}

/// Public metadata for a virtual key, excluding its secret token.
#[derive(Serialize)]
struct KeyInfo {
    /// Stable identifier used to revoke the key.
    id: String,
    /// User-assigned key name.
    name: String,
    /// Provider IDs allowed by the key.
    providers: Vec<String>,
    /// Model IDs allowed by the key.
    models: Vec<String>,
    /// Creation time in Unix seconds.
    created_at: i64,
}

/// Request to create a named virtual key with optional restrictions.
#[derive(Deserialize)]
pub(super) struct CreateKey {
    /// User-assigned name for the new key.
    name: String,
    /// Allowed provider IDs; empty permits every provider.
    #[serde(default)]
    providers: Vec<String>,
    /// Allowed model IDs; empty permits every model.
    #[serde(default)]
    models: Vec<String>,
}

impl SettingsStore {
    /// Initializes the store, validates its encryption key, and migrates legacy data.
    pub(crate) async fn new(database: Database, encryption_key: &str) -> Result<Self, String> {
        let key = STANDARD
            .decode(encryption_key.trim())
            .map_err(|_| "ZERO_G_ENCRYPTION_KEY must be base64-encoded 32 bytes".to_string())?;
        let encryption_key: [u8; 32] = key
            .try_into()
            .map_err(|_| "ZERO_G_ENCRYPTION_KEY must be base64-encoded 32 bytes".to_string())?;
        let store = Self {
            database,
            encryption_key,
        };
        store
            .database
            .retry(|| async {
                let mut db = (*store.database.db).clone();
                let mut tx = db.transaction().await?;
                let rows = Secret::all().exec(&mut tx).await?;
                if let Some(row) = rows.iter().find(|row| row.name == SENTINEL) {
                    match store.decrypt(SENTINEL, &row.value) {
                        Ok(value) if value == CHECK => {}
                        Ok(_) => return Ok(Err("Invalid encryption key check".to_string())),
                        Err(err) => return Ok(Err(err)),
                    }
                } else {
                    // Validate any pre-existing secrets before establishing the key check.
                    for row in &rows {
                        if let Err(err) = store.decrypt(&row.name, &row.value) {
                            return Ok(Err(err));
                        }
                    }
                    let encrypted = match store.encrypt(SENTINEL, CHECK) {
                        Ok(value) => value,
                        Err(err) => return Ok(Err(err)),
                    };
                    Secret::upsert_by_name(SENTINEL)
                        .value(encrypted)
                        .exec(&mut tx)
                        .await?;
                }
                tx.commit().await?;
                Ok(Ok(()))
            })
            .await
            .map_err(|e| e.to_string())??;
        crate::provider::credentials::migrate_legacy(&store).await?;
        crate::provider::registry::migrate_fixed_identities(&store).await?;
        Ok(store)
    }

    /// Loads an unencrypted setting by name, if present.
    pub(crate) async fn setting_get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(Setting::filter_by_name(name)
            .first()
            .exec(&mut (*self.database.db).clone())
            .await
            .map_err(|e| e.to_string())?
            .map(|row| row.value))
    }

    /// Loads and decrypts a named secret, if present.
    pub(crate) async fn secret_get(&self, name: &str) -> Result<Option<String>, String> {
        let Some(row) = Secret::filter_by_name(name)
            .first()
            .exec(&mut (*self.database.db).clone())
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        self.decrypt(name, &row.value).map(Some)
    }

    /// Decrypts nonce-prefixed AES-GCM data using the secret name as associated data.
    fn decrypt(&self, name: &str, data: &[u8]) -> Result<String, String> {
        let Some((nonce, ciphertext)) = data.split_at_checked(12) else {
            return Err("Invalid encrypted secret".into());
        };
        if ciphertext.len() < aead::AES_256_GCM.tag_len() {
            return Err("Invalid encrypted secret".into());
        }
        let nonce: [u8; 12] = nonce.try_into().map_err(|_| "Invalid nonce")?;
        let mut ciphertext = ciphertext.to_vec();
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.encryption_key)
                .map_err(|_| "Invalid encryption key")?,
        );
        let plaintext = key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(name.as_bytes()),
                &mut ciphertext,
            )
            .map_err(|_| "Unable to decrypt secret (wrong key or corrupted data)")?;
        String::from_utf8(plaintext.to_vec()).map_err(|e| e.to_string())
    }

    /// Encrypts a secret with a random nonce and its name as associated data.
    pub(crate) fn encrypt(&self, name: &str, value: &str) -> Result<Vec<u8>, String> {
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "Secure randomness unavailable")?;
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.encryption_key)
                .map_err(|_| "Invalid encryption key")?,
        );
        let mut encrypted = value.as_bytes().to_vec();
        key.seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(name.as_bytes()),
            &mut encrypted,
        )
        .map_err(|_| "Unable to encrypt secret")?;
        let mut stored = nonce.to_vec();
        stored.extend_from_slice(&encrypted);
        Ok(stored)
    }

    /// Stores an encrypted secret for tests.
    #[cfg(test)]
    async fn secret_put(&self, name: &str, value: &str) -> Result<(), String> {
        let stored = self.encrypt(name, value)?;
        self.database
            .retry(|| async {
                let mut db = (*self.database.db).clone();
                let mut tx = db.transaction().await?;
                Secret::upsert_by_name(name)
                    .value(stored.clone())
                    .exec(&mut tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Resolves a token's stored hash to its provider and model restrictions.
    async fn permissions(&self, token: &str) -> Result<Option<Permissions>, String> {
        let hash = digest::digest(&digest::SHA256, token.as_bytes());
        let Some(row) = VirtualKey::filter_by_key_hash(hash.as_ref().to_vec())
            .first()
            .exec(&mut (*self.database.db).clone())
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let providers = row.providers;
        let models = row.models;
        Ok(Some(Permissions {
            providers: serde_json::from_str(&providers).map_err(|e| e.to_string())?,
            models: serde_json::from_str(&models).map_err(|e| e.to_string())?,
        }))
    }
}

/// Logs a settings error and returns a generic server error response.
fn internal_error(err: &str) -> Response {
    tracing::error!("Settings store error: {err}");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// Dashboard-visible settings without the OAuth client secret.
#[derive(Serialize, ToSchema)]
pub(crate) struct SettingsResponse {
    /// Number of days to retain logs.
    #[schema(format = Int64, minimum = 0, maximum = 4_294_967_295_u64)]
    log_retention_days: u32,
    /// Configured OAuth client ID.
    oauth_client_id: String,
    /// Whether an OAuth client secret is stored.
    oauth_client_secret_set: bool,
}

/// Returns current settings and whether an OAuth secret is configured.
#[utoipa::path(
    get,
    path = "/api/settings",
    operation_id = "getSettings",
    responses(
        (status = 200, description = "Current settings", body = SettingsResponse),
        (status = 500, description = "Settings store error")
    )
)]
pub(crate) async fn get_settings(State(store): State<SettingsStore>) -> Response {
    /// Loads the settings and validates any stored OAuth secret.
    async fn load(store: &SettingsStore) -> Result<SettingsResponse, String> {
        store
            .database
            .retry(|| async {
                let mut db = (*store.database.db).clone();
                let mut tx = db.transaction().await?;
                let retention = Setting::filter_by_name("log_retention_days")
                    .first()
                    .exec(&mut tx)
                    .await?
                    .and_then(|row| row.value.parse::<u32>().ok())
                    .unwrap_or(30);
                let client_id = Setting::filter_by_name("oauth_client_id")
                    .first()
                    .exec(&mut tx)
                    .await?
                    .map_or_else(String::new, |row| row.value);
                let secret = Secret::filter_by_name("oauth_client_secret")
                    .first()
                    .exec(&mut tx)
                    .await?;
                if let Some(row) = &secret
                    && let Err(err) = store.decrypt("oauth_client_secret", &row.value)
                {
                    return Ok(Err(err));
                }
                tx.commit().await?;
                Ok(Ok(SettingsResponse {
                    log_retention_days: retention,
                    oauth_client_id: client_id,
                    oauth_client_secret_set: secret.is_some(),
                }))
            })
            .await
            .map_err(|e| e.to_string())?
    }
    match load(&store).await {
        Ok(value) => Json(value).into_response(),
        Err(err) => internal_error(&err),
    }
}

/// Partial settings update supplied by the dashboard.
#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateSettings {
    /// Optional log retention in days.
    #[schema(format = Int64, minimum = 0, maximum = 4_294_967_295_u64)]
    log_retention_days: Option<u32>,
    /// Optional replacement OAuth client ID.
    oauth_client_id: Option<String>,
    /// Optional replacement secret; an empty string clears it.
    oauth_client_secret: Option<String>,
}

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
    let result = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
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
        })
        .await;
    if let Err(err) = result {
        return internal_error(&err.to_string());
    }
    get_settings(State(store)).await
}

/// Lists virtual key metadata without exposing key tokens.
pub(super) async fn list_keys(State(store): State<SettingsStore>) -> Response {
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
        Ok(keys) => Json(json!({"keys":keys})).into_response(),
        Err(err) => internal_error(&err),
    }
}

/// Creates a virtual key and returns its token once.
pub(super) async fn create_key(
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
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let providers = match serde_json::to_string(&input.providers) {
        Ok(v) => v,
        Err(e) => return internal_error(&e.to_string()),
    };
    let models = match serde_json::to_string(&input.models) {
        Ok(v) => v,
        Err(e) => return internal_error(&e.to_string()),
    };
    let result = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
            for provider_id in &input.providers {
                let row = ProviderRecord::filter_by_id(provider_id.as_str())
                    .first()
                    .exec(&mut tx)
                    .await?;
                if !matches!(row, Some(row) if !row.deleted) {
                    return Ok(false);
                }
            }
            toasty::create!(VirtualKey {
                id: id.clone(),
                name: input.name.clone(),
                key_hash: hash.as_ref().to_vec(),
                providers: providers.clone(),
                models: models.clone(),
                created_at
            })
            .exec(&mut tx)
            .await?;
            tx.commit().await?;
            Ok(true)
        })
        .await;
    match result {
        Ok(true) => (StatusCode::CREATED, Json(json!({"id":id,"name":input.name,"providers":input.providers,"models":input.models,"created_at":created_at,"key":token}))).into_response(),
        Ok(false) => (StatusCode::BAD_REQUEST, "Invalid key provider").into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// Deletes a virtual key by ID.
pub(super) async fn revoke_key(
    State(store): State<SettingsStore>,
    Path(id): Path<String>,
) -> Response {
    let result = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
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
        })
        .await;
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// Authenticates a virtual key and attaches provider and permission context.
pub(crate) async fn virtual_key_auth(
    State((store, kind)): State<(SettingsStore, &'static str)>,
    Path(id): Path<String>,
    mut req: Request,
    next: Next,
) -> Response {
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
    let provider = match crate::provider::registry::get(&store.database, &id).await {
        Ok(Some(provider)) if provider.kind == kind => provider,
        Ok(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(super::error_body(
                    "Provider not found",
                    "invalid_request_error",
                    Some("provider_not_found"),
                )),
            )
                .into_response();
        }
        Err(err) => return internal_error(&err),
    };
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty())
        .map(|(_, token)| token.trim());
    let Some(token) = token else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(super::error_body(
                "A virtual key is required",
                "authentication_error",
                None,
            )),
        )
            .into_response();
    };
    match store.permissions(token).await {
        Ok(Some(permissions))
            if permissions.providers.is_empty()
                || permissions.providers.iter().any(|p| p == &id) =>
        {
            req.extensions_mut().insert(provider);
            req.extensions_mut().insert(permissions);
            next.run(req).await
        }
        Ok(Some(_)) => (
            StatusCode::FORBIDDEN,
            Json(super::error_body(
                "Virtual key does not permit this provider",
                "permission_error",
                None,
            )),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(super::error_body(
                "Invalid virtual key",
                "authentication_error",
                None,
            )),
        )
            .into_response(),
        Err(err) => internal_error(&err),
    }
}

#[cfg(test)]
/// Exercises settings persistence, encryption, and virtual key authorization.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::ProviderRecord;
    use axum::{
        Extension, Router,
        body::{Body, to_bytes},
        routing::{get, post},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    /// Generates a unique temporary database path.
    fn path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("zero-g-settings-{}.db", uuid::Uuid::new_v4()))
    }

    /// Encodes a deterministic 32-byte encryption key for tests.
    fn key(byte: u8) -> String {
        STANDARD.encode([byte; 32])
    }

    /// Parses a response body as JSON.
    async fn json_body(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Settings endpoints retain their response and partial-update behavior.
    #[tokio::test]
    async fn settings_endpoints_preserve_response_and_update_behavior() {
        let path = path();
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = SettingsStore::new(database, &key(1)).await.unwrap();

        let input = serde_json::from_value(json!({
            "log_retention_days": 7,
            "oauth_client_id": "client",
            "oauth_client_secret": "secret"
        }))
        .unwrap();
        let response = put_settings(State(store.clone()), Json(input)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["log_retention_days"], 7);
        assert_eq!(body["oauth_client_id"], "client");
        assert_eq!(body["oauth_client_secret_set"], true);
        assert!(body.get("oauth_client_secret").is_none());
        let input = serde_json::from_value(json!({"oauth_client_secret": ""})).unwrap();
        let body = json_body(put_settings(State(store.clone()), Json(input)).await).await;
        assert_eq!(body["oauth_client_secret_set"], false);

        let input = serde_json::from_value(json!({"log_retention_days": 0})).unwrap();
        let response = put_settings(State(store.clone()), Json(input)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Concurrent settings writes leave complete atomic updates.
    #[tokio::test]
    async fn concurrent_settings_updates_remain_atomic() {
        let path = path();
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = SettingsStore::new(database, &key(1)).await.unwrap();
        let first = UpdateSettings {
            log_retention_days: Some(7),
            oauth_client_id: Some("first".into()),
            oauth_client_secret: Some("secret".into()),
        };
        let second = UpdateSettings {
            log_retention_days: Some(14),
            oauth_client_id: Some("second".into()),
            oauth_client_secret: Some(String::new()),
        };
        let (first, second) = tokio::join!(
            put_settings(State(store.clone()), Json(first)),
            put_settings(State(store.clone()), Json(second)),
        );
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        let body = json_body(get_settings(State(store.clone())).await).await;
        assert!(
            (body["log_retention_days"] == 7
                && body["oauth_client_id"] == "first"
                && body["oauth_client_secret_set"] == true)
                || (body["log_retention_days"] == 14
                    && body["oauth_client_id"] == "second"
                    && body["oauth_client_secret_set"] == false)
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Concurrent initialization establishes only one encryption key.
    #[tokio::test]
    async fn concurrent_initialization_accepts_only_one_encryption_key() {
        let path = path();
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let first_key = key(1);
        let second_key = key(2);
        let (first, second) = tokio::join!(
            SettingsStore::new(database.clone(), &first_key),
            SettingsStore::new(database.clone(), &second_key),
        );
        assert_ne!(first.is_ok(), second.is_ok());
        let (store, accepted, rejected) = if let Ok(store) = first {
            (store, first_key, second_key)
        } else {
            (second.unwrap(), second_key, first_key)
        };
        store.secret_put("test", "value").await.unwrap();
        assert!(
            SettingsStore::new(database.clone(), &rejected)
                .await
                .is_err()
        );
        let reopened = SettingsStore::new(database.clone(), &accepted)
            .await
            .unwrap();
        assert_eq!(
            reopened.secret_get("test").await.unwrap().as_deref(),
            Some("value")
        );
        drop((store, reopened, database));
        std::fs::remove_file(path).unwrap();
    }

    /// Concurrent revocations allow only one successful deletion.
    #[tokio::test]
    async fn concurrent_key_revocations_have_one_winner() {
        let path = path();
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = SettingsStore::new(database, &key(1)).await.unwrap();
        let response = create_key(
            State(store.clone()),
            Json(CreateKey {
                name: "test".into(),
                providers: vec![],
                models: vec![],
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let id = json_body(response).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (first, second) = tokio::join!(
            revoke_key(State(store.clone()), Path(id.clone())),
            revoke_key(State(store.clone()), Path(id)),
        );
        assert!(
            (first.status() == StatusCode::NO_CONTENT && second.status() == StatusCode::NOT_FOUND)
                || (second.status() == StatusCode::NO_CONTENT
                    && first.status() == StatusCode::NOT_FOUND)
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// Test handler checks model access using attached permissions.
    async fn model(
        Extension(permissions): Extension<Permissions>,
        uri: axum::http::Uri,
    ) -> StatusCode {
        if permissions.allows_model(uri.path().rsplit('/').next().unwrap_or_default()) {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        }
    }

    /// Encrypted secrets and key validation persist across database reopen.
    #[tokio::test]
    async fn encrypted_secrets_and_key_check_survive_reopen() {
        let path = path();
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = SettingsStore::new(database, &key(1)).await.unwrap();
        assert!(
            SettingsStore::new(store.database.clone(), &key(2))
                .await
                .is_err()
        );
        store
            .secret_put("provider.codex", "private credential")
            .await
            .unwrap();
        let row = Secret::filter_by_name("provider.codex")
            .first()
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !row.value
                .windows("private credential".len())
                .any(|w| w == b"private credential")
        );
        drop(store);

        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        assert!(SettingsStore::new(database.clone(), &key(2)).await.is_err());
        let store = SettingsStore::new(database, &key(1)).await.unwrap();
        assert_eq!(
            store.secret_get("provider.codex").await.unwrap().as_deref(),
            Some("private credential")
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    // Axum route captures use braces; this exercises the full key lifecycle.
    #[allow(clippy::too_many_lines, clippy::literal_string_with_formatting_args)]
    /// Virtual keys persist, enforce restrictions, and stop working after revocation.
    #[tokio::test]
    async fn virtual_keys_persist_enforce_permissions_and_revoke() {
        let path = path();
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = SettingsStore::new(database, &key(3)).await.unwrap();
        for (id, kind) in [
            ("account-a", "codex"),
            ("account-b", "codex"),
            ("google", "agy"),
        ] {
            toasty::create!(ProviderRecord {
                id: id.to_string(),
                name: id.to_string(),
                kind: kind.to_string(),
                base_url: String::new(),
                deleted: false
            })
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        }
        assert_eq!(
            create_key(
                State(store.clone()),
                Json(CreateKey {
                    name: "invalid".into(),
                    providers: vec!["missing".into()],
                    models: vec![],
                })
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        let response = create_key(
            State(store.clone()),
            Json(CreateKey {
                name: "limited".into(),
                providers: vec!["account-a".into()],
                models: vec!["allowed-model".into()],
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = json_body(response).await;
        let token = created["key"].as_str().unwrap().to_owned();
        let id = created["id"].as_str().unwrap().to_owned();
        let row = VirtualKey::filter_by_id(id.clone())
            .first()
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.key_hash,
            digest::digest(&digest::SHA256, token.as_bytes()).as_ref()
        );
        let listed = json_body(list_keys(State(store.clone())).await).await;
        assert!(listed["keys"][0].get("key").is_none());
        drop(store);

        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = SettingsStore::new(database, &key(3)).await.unwrap();
        let app = Router::new()
            .route("/{id}/models/allowed-model", get(model))
            .route("/{id}/models/other-model", get(model))
            .layer(axum::middleware::from_fn_with_state(
                (store.clone(), "codex"),
                virtual_key_auth,
            ))
            .route("/keys", post(create_key).with_state(store.clone()));
        let request = |model: &str, token: &str| {
            Request::builder()
                .uri(format!("/account-a/models/{model}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(request("allowed-model", &token))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(request("other-model", &token))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        for id in ["google", "missing"] {
            let req = Request::builder()
                .uri(format!("/{id}/models/allowed-model"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app.clone().oneshot(req).await.unwrap().status(),
                StatusCode::NOT_FOUND
            );
        }
        let other_provider = Router::new()
            .route("/{id}/models/allowed-model", get(model))
            .layer(axum::middleware::from_fn_with_state(
                (store.clone(), "codex"),
                virtual_key_auth,
            ));
        assert_eq!(
            other_provider
                .oneshot(
                    Request::builder()
                        .uri("/account-b/models/allowed-model")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            revoke_key(State(store.clone()), Path(id)).await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            app.oneshot(request("allowed-model", &token))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}

use crate::storage::{Secret, Setting, VirtualKey};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{
    aead, digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::database::{Database, transaction_with_retry};

/// Create a virtual key.
mod create_key;
/// Read dashboard settings.
mod get_settings;
/// List virtual keys.
mod list_keys;
/// Update dashboard settings.
mod put_settings;
/// Revoke a virtual key.
mod revoke_key;
/// Change a virtual key's expiration.
mod update_key;
/// Authenticate virtual key requests.
mod virtual_key_auth;

use create_key::__path_create_key;
pub(super) use create_key::create_key;
use get_settings::__path_get_settings;
pub(crate) use get_settings::get_settings;
use list_keys::__path_list_keys;
pub(super) use list_keys::list_keys;
use put_settings::__path_put_settings;
pub(crate) use put_settings::put_settings;
use revoke_key::__path_revoke_key;
use revoke_key::__path_unrevoke_key;
pub(super) use revoke_key::{revoke_key, unrevoke_key};
use update_key::__path_update_key;
pub(super) use update_key::update_key;
pub(crate) use virtual_key_auth::{virtual_key, virtual_key_auth};

/// Reserved secret name for checking the configured encryption key.
const SENTINEL: &str = "__zero_g_encryption_key_check";
/// Expected plaintext of the encryption-key sentinel.
const CHECK: &str = "zero-g encryption key check v1";

/// Validated AES-256-GCM key for encrypting stored secrets.
#[derive(Clone)]
struct EncryptionKey([u8; 32]);

impl EncryptionKey {
    /// Decodes the configured base64 key and requires exactly 32 bytes.
    fn parse(value: &str) -> Result<Self, String> {
        let invalid = || "ZERO_G_ENCRYPTION_KEY must be base64-encoded 32 bytes".to_string();
        let bytes = STANDARD.decode(value.trim()).map_err(|_| invalid())?;
        let key = bytes.try_into().map_err(|_| invalid())?;
        Ok(Self(key))
    }
}

/// Database-backed settings and secret encryption context.
#[derive(Clone)]
pub(crate) struct SettingsStore {
    /// Database used for settings, secrets, and virtual keys.
    pub(crate) database: Database,
    /// AES-256-GCM key used to protect stored secrets.
    encryption_key: EncryptionKey,
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
#[derive(Serialize, ToSchema)]
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
    /// Expiration time in Unix seconds, if set.
    #[schema(required = true)]
    expires_at: Option<i64>,
    /// Revocation time in Unix seconds, if revoked.
    #[schema(required = true)]
    revoked_at: Option<i64>,
}

/// Request to create a named virtual key with optional restrictions.
#[derive(Deserialize, ToSchema)]
pub(super) struct CreateKey {
    /// User-assigned name for the new key.
    name: String,
    /// Allowed provider IDs; empty permits every provider.
    #[serde(default)]
    providers: Vec<String>,
    /// Allowed model IDs; empty permits every model.
    #[serde(default)]
    models: Vec<String>,
    /// Optional expiration time in Unix seconds.
    expires_at: Option<i64>,
}

/// Replacement configuration for a virtual key.
#[derive(Deserialize, ToSchema, Default)]
#[allow(clippy::option_option)]
pub(super) struct UpdateKey {
    /// Human-friendly name.
    #[schema(min_length = 1)]
    name: Option<String>,
    /// List of provider IDs permitted for this key.
    #[serde(default)]
    providers: Option<Vec<String>>,
    /// List of model IDs permitted for this key.
    #[serde(default)]
    models: Option<Vec<String>>,
    /// Expiration time in Unix seconds, or null to clear it.
    #[serde(default, deserialize_with = "deserialize_optional_expiration")]
    expires_at: Option<Option<i64>>,
}

/// Allow omitting the field, explicit null, or an integer.
#[allow(clippy::option_option)]
fn deserialize_optional_expiration<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<i64>>, D::Error> {
    Ok(Some(Option::<i64>::deserialize(deserializer)?))
}
/// Current Unix time in seconds.
pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Virtual keys returned without their tokens.
#[derive(Serialize, ToSchema)]
struct KeyList {
    /// Registered keys.
    keys: Vec<KeyInfo>,
}

/// Newly created key including its one-time token.
#[derive(Serialize, ToSchema)]
struct CreatedKey {
    /// Stable identifier.
    id: String,
    /// User-assigned name.
    name: String,
    /// Allowed provider IDs.
    providers: Vec<String>,
    /// Allowed model IDs.
    models: Vec<String>,
    /// Creation time in Unix seconds.
    created_at: i64,
    /// Expiration time in Unix seconds, if set.
    #[schema(required = true)]
    expires_at: Option<i64>,
    /// Revocation time in Unix seconds, initially null.
    #[schema(required = true)]
    revoked_at: Option<i64>,
    /// Secret token, returned only on creation.
    key: String,
}

/// Settings and virtual key dashboard operations.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        get_settings,
        put_settings,
        list_keys,
        create_key,
        revoke_key,
        unrevoke_key,
        update_key
    ),
    components(schemas(
        SettingsResponse,
        UpdateSettings,
        KeyInfo,
        CreateKey,
        UpdateKey,
        KeyList,
        CreatedKey
    ))
)]
pub(crate) struct Api;

impl SettingsStore {
    /// Initializes the store, validates its encryption key, and migrates legacy data.
    pub(crate) async fn new(database: Database, encryption_key: &str) -> Result<Self, String> {
        let encryption_key = EncryptionKey::parse(encryption_key)?;
        let store = Self {
            database,
            encryption_key,
        };
        transaction_with_retry!(store.database, |mut tx| {
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
    pub(crate) fn decrypt(&self, name: &str, data: &[u8]) -> Result<String, String> {
        let Some((nonce, ciphertext)) = data.split_at_checked(12) else {
            return Err("Invalid encrypted secret".into());
        };
        if ciphertext.len() < aead::AES_256_GCM.tag_len() {
            return Err("Invalid encrypted secret".into());
        }
        let nonce: [u8; 12] = nonce.try_into().map_err(|_| "Invalid nonce")?;
        let mut ciphertext = ciphertext.to_vec();
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.encryption_key.0)
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
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.encryption_key.0)
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
        transaction_with_retry!(self.database, |mut tx| {
            Secret::upsert_by_name(name)
                .value(stored.clone())
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .map_err(|e| e.to_string())
    }

    /// Resolves a token's stored hash to its provider and model restrictions.
    pub(crate) async fn permissions(
        &self,
        token: &str,
        identity: Option<&crate::provider::common::request_identity::RequestIdentity>,
    ) -> Result<Option<Permissions>, String> {
        let hash = digest::digest(&digest::SHA256, token.as_bytes());
        let Some(row) = VirtualKey::filter_by_key_hash(hash.as_ref().to_vec())
            .first()
            .exec(&mut (*self.database.db).clone())
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        if row.revoked_at.is_some() || row.expires_at.is_some_and(|expires| expires <= unix_now()) {
            return Ok(None);
        }
        if let Some(identity) = identity {
            identity.set_virtual_key(&row.id, &row.name);
        }
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
/// Dashboard-visible global settings.
#[derive(Serialize, ToSchema)]
pub(crate) struct SettingsResponse {
    /// Number of days to retain logs.
    #[schema(format = Int64, minimum = 0, maximum = 4_294_967_295_u64)]
    log_retention_days: u32,
    /// Datasheet refresh interval in hours; zero disables refresh.
    #[schema(format = Int64, minimum = 0)]
    pricing_interval_hours: u64,
    /// Model discovery interval in minutes; zero disables discovery.
    #[schema(format = Int64, minimum = 0)]
    discovery_interval_minutes: u64,
}

/// Partial settings update supplied by the dashboard.
#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateSettings {
    /// Optional log retention in days.
    #[schema(format = Int64, minimum = 0, maximum = 4_294_967_295_u64)]
    log_retention_days: Option<u32>,
    /// Optional datasheet refresh interval in hours.
    #[schema(format = Int64, minimum = 0)]
    pricing_interval_hours: Option<u64>,
    /// Optional model discovery interval in minutes.
    #[schema(format = Int64, minimum = 0)]
    discovery_interval_minutes: Option<u64>,
}

#[cfg(test)]
/// Exercises settings persistence, encryption, and virtual key authorization.
#[allow(clippy::unwrap_used)]
mod tests;

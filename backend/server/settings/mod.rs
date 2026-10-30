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
pub(super) use revoke_key::revoke_key;
pub(crate) use virtual_key_auth::virtual_key_auth;

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
    /// Secret token, returned only on creation.
    key: String,
}

/// Settings and virtual key dashboard operations.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(get_settings, put_settings, list_keys, create_key, revoke_key),
    components(schemas(
        SettingsResponse,
        UpdateSettings,
        KeyInfo,
        CreateKey,
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
    pub(crate) async fn permissions(&self, token: &str) -> Result<Option<Permissions>, String> {
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
/// Dashboard-visible global settings.
#[derive(Serialize, ToSchema)]
pub(crate) struct SettingsResponse {
    /// Number of days to retain logs.
    #[schema(format = Int64, minimum = 0, maximum = 4_294_967_295_u64)]
    log_retention_days: u32,
}

/// Partial settings update supplied by the dashboard.
#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateSettings {
    /// Optional log retention in days.
    #[schema(format = Int64, minimum = 0, maximum = 4_294_967_295_u64)]
    log_retention_days: Option<u32>,
}

#[cfg(test)]
/// Exercises settings persistence, encryption, and virtual key authorization.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::ProviderRecord;
    use axum::extract::{Path, Request, State};
    use axum::http::header;
    use axum::response::Json;
    use axum::{
        Extension, Router,
        body::{Body, to_bytes},
        routing::{get, post},
    };
    use serde_json::{Value, json};
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

        let input = serde_json::from_value(json!({"log_retention_days": 7})).unwrap();
        let response = put_settings(State(store.clone()), Json(input)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["log_retention_days"], 7);
        assert!(body.get("oauth_client_id").is_none());
        assert!(body.get("oauth_client_secret_set").is_none());

        let input = serde_json::from_value(json!({"log_retention_days": 0})).unwrap();
        let response = put_settings(State(store.clone()), Json(input)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
    /// State shared by the isolated affinity cache and its leak checks.
    type AffinityCacheState = (
        std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
        Vec<String>,
    );

    /// Isolated upstream reports cache transitions without disclosing the key.
    async fn affinity_cache(
        State((seen, tokens)): State<AffinityCacheState>,
        headers: axum::http::HeaderMap,
    ) -> &'static str {
        let Some(id) = headers.get("x-affinity").and_then(|v| v.to_str().ok()) else {
            return "missing";
        };
        if id.contains("session-123") || tokens.iter().any(|token| id.contains(token)) {
            return "leaked";
        }
        if seen.lock().await.insert(id.to_owned()) {
            "miss"
        } else {
            "hit"
        }
    }

    /// Sends middleware-provided affinity to the isolated upstream.
    async fn affinity_proxy(
        Extension(session): Extension<crate::provider::common::session::RequestSession>,
        Extension(upstream): Extension<String>,
    ) -> String {
        reqwest::Client::new()
            .get(upstream)
            .header("x-affinity", session.id)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    }

    /// Authenticated cache affinity is isolated by key and provider.
    #[allow(clippy::too_many_lines, clippy::literal_string_with_formatting_args)]
    #[tokio::test]
    async fn authenticated_session_cache_is_isolated() {
        let path = path();
        let database = Database::open(path.to_str().unwrap()).await.unwrap();
        let store = SettingsStore::new(database, &key(9)).await.unwrap();
        for id in ["account-a", "account-b"] {
            toasty::create!(ProviderRecord {
                id: id.to_string(),
                name: id.to_string(),
                kind: "codex".to_string(),
                base_url: String::new(),
                deleted: false
            })
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        }
        let mut tokens = Vec::new();
        for name in ["first", "second"] {
            let response = create_key(
                State(store.clone()),
                Json(CreateKey {
                    name: name.into(),
                    providers: vec![],
                    models: vec![],
                }),
            )
            .await;
            assert_eq!(response.status(), StatusCode::CREATED);
            tokens.push(
                json_body(response).await["key"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        let cache = Router::new().route("/", get(affinity_cache)).with_state((
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
            tokens.clone(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, cache).await.unwrap() });
        let app = Router::new()
            .route("/{id}/v1/responses", get(affinity_proxy))
            .layer(Extension(upstream))
            .layer(axum::middleware::from_fn_with_state(
                (store.clone(), "codex"),
                virtual_key_auth,
            ));
        let send = |provider: &str, token: &str, headers: &[(&str, &str)]| {
            let mut builder = Request::builder()
                .uri(format!("/{provider}/v1/responses"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
            for (name, value) in headers {
                builder = builder.header(*name, *value);
            }
            app.clone().oneshot(builder.body(Body::empty()).unwrap())
        };
        let outcome = |response: Response| async move {
            assert_eq!(response.status(), StatusCode::OK);
            String::from_utf8(to_bytes(response.into_body(), 1024).await.unwrap().to_vec()).unwrap()
        };
        let first = tokens.first().unwrap();
        let second = tokens.get(1).unwrap();
        let session = [("session_id", "session-123")];
        assert_eq!(
            outcome(send("account-a", first, &session).await.unwrap()).await,
            "miss"
        );
        assert_eq!(
            outcome(send("account-a", first, &session).await.unwrap()).await,
            "hit"
        );
        assert_eq!(
            outcome(send("account-a", second, &session).await.unwrap()).await,
            "miss"
        );
        assert_eq!(
            outcome(send("account-b", first, &session).await.unwrap()).await,
            "miss"
        );
        for headers in [
            vec![],
            vec![("session_id", "")],
            vec![("session_id", "invalid session")],
            vec![("session_id", "duplicate"), ("session_id", "duplicate")],
            vec![
                ("session_id", "invalid session"),
                ("x-session-id", "session-123"),
            ],
        ] {
            for _ in 0..2 {
                assert_eq!(
                    outcome(send("account-a", first, &headers).await.unwrap()).await,
                    "miss"
                );
            }
        }
        let maximum = "a".repeat(128);
        let oversized = "a".repeat(129);
        for (value, expected) in [(&maximum, "hit"), (&oversized, "miss")] {
            let headers = [("session_id", value.as_str())];
            assert_eq!(
                outcome(send("account-a", first, &headers).await.unwrap()).await,
                "miss"
            );
            assert_eq!(
                outcome(send("account-a", first, &headers).await.unwrap()).await,
                expected
            );
        }
        for alias in [
            "session-id",
            "x-session-id",
            "x-opencode-session",
            "conversation-id",
        ] {
            let headers = [(alias, "session-123")];
            assert_eq!(
                outcome(send("account-a", first, &headers).await.unwrap()).await,
                "hit"
            );
        }
        let preferred = [
            ("x-bf-eh-session_id", "preferred"),
            ("session_id", "session-123"),
        ];
        assert_eq!(
            outcome(send("account-a", first, &preferred).await.unwrap()).await,
            "miss"
        );
        assert_eq!(
            outcome(send("account-a", first, &preferred).await.unwrap()).await,
            "hit"
        );
        let conversation = [("conversation_id", "conversation")];
        assert_eq!(
            outcome(send("account-a", first, &conversation).await.unwrap()).await,
            "miss"
        );
        assert_eq!(
            outcome(send("account-a", first, &conversation).await.unwrap()).await,
            "hit"
        );
        assert_eq!(
            send("account-a", "invalid-key", &session)
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        server.abort();
        drop((app, store));
        std::fs::remove_file(path).unwrap();
    }
}

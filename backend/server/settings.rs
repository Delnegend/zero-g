use crate::storage::{Secret, Setting, VirtualKey};
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

use super::observability::LogStore;

const SENTINEL: &str = "__zero_g_encryption_key_check";
const CHECK: &str = "zero-g encryption key check v1";

#[derive(Clone)]
pub(crate) struct SettingsStore {
    logs: LogStore,
    encryption_key: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Permissions {
    pub(super) providers: Vec<String>,
    pub(super) models: Vec<String>,
}

impl Permissions {
    pub(super) fn allows_model(&self, model: &str) -> bool {
        self.models.is_empty() || self.models.iter().any(|allowed| allowed == model)
    }
}

#[derive(Serialize)]
struct KeyInfo {
    id: String,
    name: String,
    providers: Vec<String>,
    models: Vec<String>,
    created_at: i64,
}

#[derive(Deserialize)]
pub(super) struct CreateKey {
    name: String,
    #[serde(default)]
    providers: Vec<String>,
    #[serde(default)]
    models: Vec<String>,
}

impl SettingsStore {
    pub(crate) async fn new(logs: LogStore, encryption_key: &str) -> Result<Self, String> {
        let key = STANDARD
            .decode(encryption_key.trim())
            .map_err(|_| "ZERO_G_ENCRYPTION_KEY must be base64-encoded 32 bytes".to_string())?;
        let encryption_key: [u8; 32] = key
            .try_into()
            .map_err(|_| "ZERO_G_ENCRYPTION_KEY must be base64-encoded 32 bytes".to_string())?;
        let store = Self {
            logs,
            encryption_key,
        };
        {
            let _guard = store.logs.lock.lock().await;
            let rows = Secret::all()
                .exec(&mut (*store.logs.db).clone())
                .await
                .map_err(|e| e.to_string())?;
            if let Some(row) = rows.iter().find(|row| row.name == SENTINEL) {
                if store.decrypt(SENTINEL, &row.value)? != CHECK {
                    return Err("Invalid encryption key check".into());
                }
            } else {
                // Validate any pre-existing secrets before establishing the key check.
                for row in &rows {
                    store.decrypt(&row.name, &row.value)?;
                }
                let encrypted = store.encrypt(SENTINEL, CHECK)?;
                Secret::upsert_by_name(SENTINEL)
                    .value(encrypted)
                    .exec(&mut (*store.logs.db).clone())
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(store)
    }

    pub(crate) async fn setting_get(&self, name: &str) -> Result<Option<String>, String> {
        let _guard = self.logs.lock.lock().await;
        Ok(Setting::filter_by_name(name)
            .first()
            .exec(&mut (*self.logs.db).clone())
            .await
            .map_err(|e| e.to_string())?
            .map(|row| row.value))
    }

    pub(crate) async fn setting_put(&self, name: &str, value: &str) -> Result<(), String> {
        let _guard = self.logs.lock.lock().await;
        Setting::upsert_by_name(name)
            .value(value)
            .exec(&mut (*self.logs.db).clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(crate) async fn secret_get(&self, name: &str) -> Result<Option<String>, String> {
        let _guard = self.logs.lock.lock().await;
        let Some(row) = Secret::filter_by_name(name)
            .first()
            .exec(&mut (*self.logs.db).clone())
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        self.decrypt(name, &row.value).map(Some)
    }

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

    fn encrypt(&self, name: &str, value: &str) -> Result<Vec<u8>, String> {
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

    pub(crate) async fn secret_put(&self, name: &str, value: &str) -> Result<(), String> {
        let stored = self.encrypt(name, value)?;
        let _guard = self.logs.lock.lock().await;
        Secret::upsert_by_name(name)
            .value(stored)
            .exec(&mut (*self.logs.db).clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn secret_delete(&self, name: &str) -> Result<(), String> {
        let _guard = self.logs.lock.lock().await;
        Secret::delete_by_name(&mut (*self.logs.db).clone(), name)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn permissions(&self, token: &str) -> Result<Option<Permissions>, String> {
        let hash = digest::digest(&digest::SHA256, token.as_bytes());
        let _guard = self.logs.lock.lock().await;
        let Some(row) = VirtualKey::filter_by_key_hash(hash.as_ref().to_vec())
            .first()
            .exec(&mut (*self.logs.db).clone())
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

fn internal_error(err: &str) -> Response {
    tracing::error!("Settings store error: {err}");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

#[derive(Serialize, ToSchema)]
pub(crate) struct SettingsResponse {
    #[schema(format = Int64, minimum = 0, maximum = 4_294_967_295_u64)]
    log_retention_days: u32,
    zen_url: String,
    openai_base_url: String,
    oauth_client_id: String,
    zen_api_key_set: bool,
    openai_api_key_set: bool,
    oauth_client_secret_set: bool,
}

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
    async fn load(store: &SettingsStore) -> Result<SettingsResponse, String> {
        let retention = store
            .setting_get("log_retention_days")
            .await?
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(30);
        Ok(SettingsResponse {
            log_retention_days: retention,
            zen_url: store.setting_get("zen_url").await?.unwrap_or_default(),
            openai_base_url: store
                .setting_get("openai_base_url")
                .await?
                .unwrap_or_default(),
            oauth_client_id: store
                .setting_get("oauth_client_id")
                .await?
                .unwrap_or_default(),
            zen_api_key_set: store.secret_get("zen_api_key").await?.is_some(),
            openai_api_key_set: store.secret_get("openai_api_key").await?.is_some(),
            oauth_client_secret_set: store.secret_get("oauth_client_secret").await?.is_some(),
        })
    }
    match load(&store).await {
        Ok(value) => Json(value).into_response(),
        Err(err) => internal_error(&err),
    }
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateSettings {
    #[schema(format = Int64, minimum = 0, maximum = 4_294_967_295_u64)]
    log_retention_days: Option<u32>,
    zen_url: Option<String>,
    openai_base_url: Option<String>,
    oauth_client_id: Option<String>,
    zen_api_key: Option<String>,
    openai_api_key: Option<String>,
    oauth_client_secret: Option<String>,
}

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
        ("zen_url", input.zen_url),
        ("openai_base_url", input.openai_base_url),
        ("oauth_client_id", input.oauth_client_id),
        (
            "log_retention_days",
            input.log_retention_days.map(|v| v.to_string()),
        ),
    ];
    for (name, value) in values {
        if let Some(value) = value
            && let Err(err) = store.setting_put(name, &value).await
        {
            return internal_error(&err);
        }
    }
    for (name, value) in [
        ("zen_api_key", input.zen_api_key),
        ("openai_api_key", input.openai_api_key),
        ("oauth_client_secret", input.oauth_client_secret),
    ] {
        if let Some(value) = value {
            let result = if value.is_empty() {
                store.secret_delete(name).await
            } else {
                store.secret_put(name, &value).await
            };
            if let Err(err) = result {
                return internal_error(&err);
            }
        }
    }
    get_settings(State(store)).await
}

pub(super) async fn list_keys(State(store): State<SettingsStore>) -> Response {
    async fn list(store: &SettingsStore) -> Result<Vec<KeyInfo>, String> {
        let _guard = store.logs.lock.lock().await;
        let rows = VirtualKey::all()
            .order_by((
                VirtualKey::fields().created_at().desc(),
                VirtualKey::fields().id().asc(),
            ))
            .exec(&mut (*store.logs.db).clone())
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

pub(super) async fn create_key(
    State(store): State<SettingsStore>,
    Json(input): Json<CreateKey>,
) -> Response {
    if input.name.trim().is_empty()
        || input
            .providers
            .iter()
            .any(|p| !matches!(p.as_str(), "agy" | "codex" | "zen" | "openai"))
        || input.models.iter().any(|m| m.trim().is_empty())
    {
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
    let result = async {
        let _guard = store.logs.lock.lock().await;
        toasty::create!(VirtualKey {
            id: id.clone(),
            name: input.name.clone(),
            key_hash: hash.as_ref().to_vec(),
            providers,
            models,
            created_at
        })
        .exec(&mut (*store.logs.db).clone())
        .await
        .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
    .await;
    match result {
        Ok(()) => (StatusCode::CREATED, Json(json!({"id":id,"name":input.name,"providers":input.providers,"models":input.models,"created_at":created_at,"key":token}))).into_response(),
        Err(err) => internal_error(&err),
    }
}

pub(super) async fn revoke_key(
    State(store): State<SettingsStore>,
    Path(id): Path<String>,
) -> Response {
    let _guard = store.logs.lock.lock().await;
    let row = match VirtualKey::filter_by_id(id)
        .first()
        .exec(&mut (*store.logs.db).clone())
        .await
    {
        Ok(row) => row,
        Err(err) => return internal_error(&err.to_string()),
    };
    let Some(row) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match row.delete().exec(&mut (*store.logs.db).clone()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

pub(super) async fn virtual_key_auth(
    State((store, provider)): State<(SettingsStore, &'static str)>,
    mut req: Request,
    next: Next,
) -> Response {
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }
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
                || permissions.providers.iter().any(|p| p == provider) =>
        {
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{
        Extension, Router,
        body::{Body, to_bytes},
        routing::{get, post},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    fn path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("zero-g-settings-{}.db", uuid::Uuid::new_v4()))
    }

    fn key(byte: u8) -> String {
        STANDARD.encode([byte; 32])
    }

    async fn json_body(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn settings_endpoints_preserve_response_and_update_behavior() {
        let path = path();
        let logs = LogStore::open_unpruned(path.to_str().unwrap())
            .await
            .unwrap();
        let store = SettingsStore::new(logs, &key(1)).await.unwrap();

        let input = serde_json::from_value(json!({
            "log_retention_days": 7,
            "zen_api_key": "secret",
            "openai_base_url": "https://example.com/v1",
            "openai_api_key": "openai-secret"
        }))
        .unwrap();
        let response = put_settings(State(store.clone()), Json(input)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["log_retention_days"], 7);
        assert_eq!(body["zen_api_key_set"], true);
        assert!(body.get("zen_api_key").is_none());
        assert_eq!(body["openai_base_url"], "https://example.com/v1");
        assert_eq!(body["openai_api_key_set"], true);
        assert!(body.get("openai_api_key").is_none());

        let input = serde_json::from_value(json!({"openai_base_url": ""})).unwrap();
        let body = json_body(put_settings(State(store.clone()), Json(input)).await).await;
        assert_eq!(body["openai_base_url"], "");
        assert_eq!(body["openai_api_key_set"], true);
        let input = serde_json::from_value(json!({"openai_api_key": ""})).unwrap();
        let body = json_body(put_settings(State(store.clone()), Json(input)).await).await;
        assert_eq!(body["openai_api_key_set"], false);

        let input = serde_json::from_value(json!({"zen_api_key": ""})).unwrap();
        let response = put_settings(State(store.clone()), Json(input)).await;
        let body = json_body(response).await;
        assert_eq!(body["zen_api_key_set"], false);

        let input = serde_json::from_value(json!({"log_retention_days": 0})).unwrap();
        let response = put_settings(State(store.clone()), Json(input)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    async fn model(
        Extension(permissions): Extension<Permissions>,
        Path(model): Path<String>,
    ) -> StatusCode {
        if permissions.allows_model(&model) {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        }
    }

    #[tokio::test]
    async fn encrypted_secrets_and_key_check_survive_reopen() {
        let path = path();
        let logs = LogStore::open_unpruned(path.to_str().unwrap())
            .await
            .unwrap();
        let store = SettingsStore::new(logs, &key(1)).await.unwrap();
        assert!(
            SettingsStore::new(store.logs.clone(), &key(2))
                .await
                .is_err()
        );
        store
            .secret_put("provider.codex", "private credential")
            .await
            .unwrap();
        let row = Secret::filter_by_name("provider.codex")
            .first()
            .exec(&mut (*store.logs.db).clone())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !row.value
                .windows("private credential".len())
                .any(|w| w == b"private credential")
        );
        drop(store);

        let logs = LogStore::open_unpruned(path.to_str().unwrap())
            .await
            .unwrap();
        assert!(SettingsStore::new(logs.clone(), &key(2)).await.is_err());
        let store = SettingsStore::new(logs, &key(1)).await.unwrap();
        assert_eq!(
            store.secret_get("provider.codex").await.unwrap().as_deref(),
            Some("private credential")
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn virtual_keys_persist_enforce_permissions_and_revoke() {
        let path = path();
        let logs = LogStore::open_unpruned(path.to_str().unwrap())
            .await
            .unwrap();
        let store = SettingsStore::new(logs, &key(3)).await.unwrap();
        let response = create_key(
            State(store.clone()),
            Json(CreateKey {
                name: "limited".into(),
                providers: vec!["codex".into(), "openai".into()],
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
            .exec(&mut (*store.logs.db).clone())
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

        let logs = LogStore::open_unpruned(path.to_str().unwrap())
            .await
            .unwrap();
        let store = SettingsStore::new(logs, &key(3)).await.unwrap();
        let app = Router::new()
            .route("/models/{model}", get(model))
            .layer(axum::middleware::from_fn_with_state(
                (store.clone(), "codex"),
                virtual_key_auth,
            ))
            .route("/keys", post(create_key).with_state(store.clone()));
        let request = |model: &str, token: &str| {
            Request::builder()
                .uri(format!("/models/{model}"))
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
        let other_provider = Router::new().route("/models/{model}", get(model)).layer(
            axum::middleware::from_fn_with_state((store.clone(), "agy"), virtual_key_auth),
        );
        assert_eq!(
            other_provider
                .oneshot(request("allowed-model", &token))
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

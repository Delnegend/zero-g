use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get as route_get,
};
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::{
    database::transaction_with_retry,
    provider::registry,
    server::settings::SettingsStore,
    storage::{ProviderCredential, ProviderRecord, Secret, Setting, VirtualKey},
};

/// Credential metadata exposed without the underlying secret.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct Credential {
    /// Stable credential identifier.
    pub(crate) id: String,
    /// User-facing credential name.
    pub(crate) name: String,
    /// Relative selection weight.
    pub(crate) weight: i64,
    /// Whether the credential is enabled.
    pub(crate) enabled: bool,
    /// Whether its provider-specific secret exists.
    pub(crate) connected: bool,
    /// Account email returned during OAuth sign-in, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) email: Option<String>,
}

/// Builds public credential metadata and checks for its stored secret.
async fn info(
    store: &SettingsStore,
    row: ProviderCredential,
    kind: &str,
) -> Result<Credential, String> {
    let suffix = if crate::provider::ProviderKind::from_str(kind)
        .is_some_and(crate::provider::ProviderKind::oauth)
    {
        "credential"
    } else {
        "api_key"
    };
    let connected = store
        .secret_get(&format!("credential.{}.{suffix}", row.id))
        .await?
        .is_some();
    Ok(Credential {
        id: row.id,
        name: row.name,
        weight: row.weight,
        enabled: row.enabled,
        connected,
        email: None,
    })
}

/// Profile information stored alongside an OAuth token.
#[derive(Deserialize)]
struct AccountProfile {
    /// Account email supplied by the provider.
    email: Option<String>,
}

/// Populate dashboard-only identity metadata without parsing it on inference requests.
async fn include_emails(
    store: &SettingsStore,
    provider_id: &str,
    credentials: &mut [Credential],
) -> Result<(), String> {
    let Some(provider) = registry::get(&store.database, provider_id).await? else {
        return Ok(());
    };
    if !matches!(provider.kind.as_str(), "agy" | "codex" | "claude") {
        return Ok(());
    }
    for credential in credentials
        .iter_mut()
        .filter(|credential| credential.connected)
    {
        if let Some(secret) = store
            .secret_get(&format!("credential.{}.credential", credential.id))
            .await?
        {
            let profile: AccountProfile = serde_json::from_str(&secret)
                .map_err(|_| "Invalid stored account profile".to_string())?;
            credential.email = profile.email.filter(|email| !email.trim().is_empty());
        }
    }
    Ok(())
}

/// Retrieves a credential belonging to the named provider, if present.
pub(crate) async fn get(
    store: &SettingsStore,
    provider_id: &str,
    credential_id: &str,
) -> Result<Option<Credential>, String> {
    let Some(provider) = registry::get(&store.database, provider_id).await? else {
        return Ok(None);
    };
    let row = ProviderCredential::filter_by_id(credential_id)
        .first()
        .exec(&mut (*store.database.db).clone())
        .await
        .map_err(|e| e.to_string())?;
    match row.filter(|row| row.provider_id == provider_id) {
        Some(row) => Ok(Some(info(store, row, &provider.kind).await?)),
        None => Ok(None),
    }
}

/// Lists credentials for a provider, or returns none if it does not exist.
pub(crate) async fn all(
    store: &SettingsStore,
    provider_id: &str,
) -> Result<Option<Vec<Credential>>, String> {
    let Some(provider) = registry::get(&store.database, provider_id).await? else {
        return Ok(None);
    };
    let rows =
        ProviderCredential::filter(ProviderCredential::fields().provider_id().eq(provider_id))
            .exec(&mut (*store.database.db).clone())
            .await
            .map_err(|e| e.to_string())?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(info(store, row, &provider.kind).await?);
    }
    Ok(Some(result))
}

/// Draw without replacement, proportional to each remaining credential's weight.
pub(crate) async fn list(
    store: &SettingsStore,
    provider_id: &str,
) -> Result<Vec<Credential>, String> {
    let mut remaining: Vec<_> = all(store, provider_id)
        .await?
        .unwrap_or_default()
        .into_iter()
        .filter(|item| item.enabled && item.connected && item.weight > 0)
        .collect();
    let mut result = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let total: u128 = remaining
            .iter()
            .map(|item| u128::try_from(item.weight).unwrap_or(0))
            .sum();
        let mut pick = rand::rng().random_range(0..total);
        let mut index = 0;
        for (i, item) in remaining.iter().enumerate() {
            let weight = u128::try_from(item.weight).unwrap_or(0);
            if pick < weight {
                index = i;
                break;
            }
            pick = pick.saturating_sub(weight);
        }
        result.push(remaining.swap_remove(index));
    }
    Ok(result)
}

/// Callers hold the credential lock while writing OAuth state.
pub(crate) async fn save_oauth(
    store: &SettingsStore,
    provider_id: &str,
    credential_id: &str,
    kind: &str,
    value: &str,
) -> Result<(), String> {
    let name = format!("credential.{credential_id}.credential");
    let encrypted = store.encrypt(&name, value)?;
    transaction_with_retry!(store.database, |mut tx| {
        let provider = ProviderRecord::filter_by_id(provider_id).first().exec(&mut tx).await?;
        let credential = ProviderCredential::filter_by_id(credential_id).first().exec(&mut tx).await?;
        if !matches!(provider, Some(row) if !row.deleted && row.kind == kind && crate::provider::ProviderKind::from_str(kind).is_some_and(crate::provider::ProviderKind::oauth))
            || !matches!(credential, Some(row) if row.provider_id == provider_id && row.enabled) {
            return Ok(Err("Credential no longer exists, is disabled, or has the wrong provider".to_string()));
        }
        Secret::upsert_by_name(name.as_str()).value(encrypted.clone()).exec(&mut tx).await?;
        tx.commit().await?;
        Ok(Ok(()))
    }).map_err(|e| e.to_string())?
}

/// Credential creation or update request.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct CredentialInput {
    /// User-facing credential name.
    name: String,
    /// Positive relative selection weight.
    weight: i64,
    /// Whether the credential can be selected.
    enabled: bool,
    /// API key to store, when supplied.
    api_key: Option<String>,
}

/// Rejects empty names and nonpositive weights.
fn validate(input: &CredentialInput) -> Result<(), &'static str> {
    if input.name.trim().is_empty() {
        return Err("Credential name must not be empty");
    }
    if input.weight <= 0 {
        return Err("Credential weight must be a positive integer");
    }
    Ok(())
}

/// Logs a storage failure and returns a generic server error.
fn internal(error: &str) -> Response {
    tracing::error!("Credential storage error: {error}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Credential storage error",
    )
        .into_response()
}

/// Credential listing.
#[derive(Serialize, utoipa::ToSchema)]
struct CredentialsResponse {
    /// Credential metadata without secrets.
    credentials: Vec<Credential>,
}

/// Credential management API.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(list_endpoint, create, update, delete),
    components(schemas(Credential, CredentialInput, CredentialsResponse))
)]
pub(crate) struct Api;

/// Lists a provider's credentials over HTTP.
#[utoipa::path(get, path = "/api/providers/{id}/credentials", operation_id = "listCredentials", params(("id" = String, Path)), responses((status = 200, body = CredentialsResponse)))]
async fn list_endpoint(
    State(store): State<SettingsStore>,
    Path(provider_id): Path<String>,
) -> Response {
    match all(&store, &provider_id).await {
        Ok(Some(mut credentials)) => {
            match include_emails(&store, &provider_id, &mut credentials).await {
                Ok(()) => Json(CredentialsResponse { credentials }).into_response(),
                Err(e) => internal(&e),
            }
        }
        Ok(None) => (StatusCode::NOT_FOUND, "Provider not found").into_response(),
        Err(e) => internal(&e),
    }
}

/// Creates a credential and stores any supplied secret.
#[utoipa::path(post, path = "/api/providers/{id}/credentials", operation_id = "createCredential", params(("id" = String, Path)), request_body = CredentialInput, responses((status = 201, body = Credential)))]
async fn create(
    State(store): State<SettingsStore>,
    Path(provider_id): Path<String>,
    Json(input): Json<CredentialInput>,
) -> Response {
    if let Err(e) = validate(&input) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    let provider_lock =
        crate::provider::login::credential_lock(&format!("provider:{provider_id}")).await;
    let _provider_guard = provider_lock.lock().await;
    let id = uuid::Uuid::new_v4().to_string();
    let secret_name = format!("credential.{id}.api_key");
    let encrypted = match input
        .api_key
        .as_deref()
        .map(|key| store.encrypt(&secret_name, key))
        .transpose()
    {
        Ok(value) => value,
        Err(e) => return internal(&e),
    };
    let result = transaction_with_retry!(store.database, |mut tx| {
        let provider = ProviderRecord::filter_by_id(provider_id.as_str())
            .first()
            .exec(&mut tx)
            .await?
            .filter(|row| !row.deleted);
        let Some(provider) = provider else {
            return Ok(StatusCode::NOT_FOUND);
        };
        let Some(kind) = crate::provider::ProviderKind::from_str(&provider.kind) else {
            return Ok(StatusCode::BAD_REQUEST);
        };
        if encrypted.is_some() && kind.oauth() {
            return Ok(StatusCode::BAD_REQUEST);
        }
        if kind.api_key() && !kind.keyless() && input.api_key.as_deref().is_none_or(str::is_empty) {
            return Ok(StatusCode::BAD_REQUEST);
        }
        toasty::create!(ProviderCredential {
            id: id.clone(),
            provider_id: provider_id.clone(),
            name: input.name.clone(),
            weight: input.weight,
            enabled: input.enabled
        })
        .exec(&mut tx)
        .await?;
        if let Some(value) = &encrypted {
            Secret::upsert_by_name(secret_name.as_str())
                .value(value.clone())
                .exec(&mut tx)
                .await?;
        }
        tx.commit().await?;
        Ok(StatusCode::CREATED)
    });
    match result {
        Ok(StatusCode::CREATED) => match get(&store, &provider_id, &id).await {
            Ok(Some(value)) => (StatusCode::CREATED, Json(value)).into_response(),
            Ok(None) => (StatusCode::NOT_FOUND, "Credential not found").into_response(),
            Err(e) => internal(&e),
        },
        Ok(StatusCode::BAD_REQUEST) => {
            (StatusCode::BAD_REQUEST, "Invalid API key for this provider").into_response()
        }
        Ok(_) => (StatusCode::NOT_FOUND, "Provider not found").into_response(),
        Err(e) => internal(&e.to_string()),
    }
}

/// Updates credential metadata and an optional API key.
#[utoipa::path(put, path = "/api/providers/{id}/credentials/{credential_id}", operation_id = "updateCredential", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = CredentialInput, responses((status = 200, body = Credential)))]
async fn update(
    State(store): State<SettingsStore>,
    Path((provider_id, id)): Path<(String, String)>,
    Json(input): Json<CredentialInput>,
) -> Response {
    if let Err(e) = validate(&input) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    let lock = crate::provider::login::credential_lock(&id).await;
    let _guard = lock.lock().await;
    let secret_name = format!("credential.{id}.api_key");
    let encrypted = match input
        .api_key
        .as_deref()
        .map(|key| store.encrypt(&secret_name, key))
        .transpose()
    {
        Ok(value) => value,
        Err(e) => return internal(&e),
    };
    let result = transaction_with_retry!(store.database, |mut tx| {
        let provider = ProviderRecord::filter_by_id(provider_id.as_str())
            .first()
            .exec(&mut tx)
            .await?
            .filter(|row| !row.deleted);
        let row = ProviderCredential::filter_by_id(id.as_str())
            .first()
            .exec(&mut tx)
            .await?;
        let Some(provider) = provider else {
            return Ok(StatusCode::NOT_FOUND);
        };
        if !matches!(row, Some(row) if row.provider_id == provider_id) {
            return Ok(StatusCode::NOT_FOUND);
        }
        let Some(kind) = crate::provider::ProviderKind::from_str(&provider.kind) else {
            return Ok(StatusCode::BAD_REQUEST);
        };
        if encrypted.is_some() && kind.oauth() {
            return Ok(StatusCode::BAD_REQUEST);
        }
        if kind.api_key() && !kind.keyless() && input.api_key.as_deref().is_some_and(str::is_empty)
        {
            return Ok(StatusCode::BAD_REQUEST);
        }
        ProviderCredential::upsert_by_id(id.as_str())
            .provider_id(provider_id.clone())
            .name(input.name.clone())
            .weight(input.weight)
            .enabled(input.enabled)
            .exec(&mut tx)
            .await?;
        if let Some(value) = &encrypted {
            Secret::upsert_by_name(secret_name.as_str())
                .value(value.clone())
                .exec(&mut tx)
                .await?;
        }
        tx.commit().await?;
        Ok(StatusCode::OK)
    });
    match result {
        Ok(StatusCode::OK) => match get(&store, &provider_id, &id).await {
            Ok(Some(mut value)) => {
                match include_emails(&store, &provider_id, std::slice::from_mut(&mut value)).await {
                    Ok(()) => Json(value).into_response(),
                    Err(e) => internal(&e),
                }
            }
            Ok(None) => (StatusCode::NOT_FOUND, "Credential not found").into_response(),
            Err(e) => internal(&e),
        },
        Ok(StatusCode::BAD_REQUEST) => {
            (StatusCode::BAD_REQUEST, "Invalid API key for this provider").into_response()
        }
        Ok(_) => (StatusCode::NOT_FOUND, "Credential not found").into_response(),
        Err(e) => internal(&e.to_string()),
    }
}

/// Deletes a provider credential and its associated state.
#[utoipa::path(delete, path = "/api/providers/{id}/credentials/{credential_id}", operation_id = "deleteCredential", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 204)))]
async fn delete(
    State(store): State<SettingsStore>,
    Path((provider_id, id)): Path<(String, String)>,
) -> Response {
    let lock = crate::provider::login::credential_lock(&id).await;
    let _guard = lock.lock().await;
    let result = transaction_with_retry!(store.database, |mut tx| {
        let provider = ProviderRecord::filter_by_id(provider_id.as_str())
            .first()
            .exec(&mut tx)
            .await?;
        let row = ProviderCredential::filter_by_id(id.as_str())
            .first()
            .exec(&mut tx)
            .await?;
        if !matches!(provider, Some(row) if !row.deleted)
            || !matches!(row, Some(row) if row.provider_id == provider_id)
        {
            return Ok(false);
        }
        for suffix in [
            "api_key",
            "credential",
            "console_session",
            "console_workspace",
            "openrouter_management_key",
        ] {
            Secret::delete_by_name(&mut tx, format!("credential.{id}.{suffix}").as_str()).await?;
        }
        ProviderCredential::delete_by_id(&mut tx, id.as_str()).await?;
        tx.commit().await?;
        Ok(true)
    });
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "Credential not found").into_response(),
        Err(e) => internal(&e.to_string()),
    }
}

/// Builds the credential management routes.
pub(crate) fn router(store: SettingsStore) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials",
            route_get(list_endpoint).post(create),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}",
            axum::routing::put(update).delete(delete),
        )
        .with_state(store.clone())
        .merge(crate::provider::quota::router(store))
}

/// Migrate encrypted legacy credentials after the encryption key has been verified.
/// All provider, permission and credential changes commit together; old secrets are
/// removed only after their replacement is encrypted in that same transaction.
pub(crate) async fn migrate_legacy(store: &SettingsStore) -> Result<(), String> {
    const MARKER: &str = "credential_pool_migrated_v1";
    if store.setting_get(MARKER).await?.is_some() {
        return Ok(());
    }
    let mut providers = registry::list(&store.database).await?;
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    let mut canonical = std::collections::HashMap::<String, String>::new();
    let mut remap = std::collections::HashMap::<String, String>::new();
    let mut groups = std::collections::HashMap::<String, Vec<String>>::new();
    let mut entries = Vec::new();
    let mut singleton_urls = std::collections::HashMap::new();
    for provider in &providers {
        let kind = crate::provider::ProviderKind::from_str(&provider.kind)
            .ok_or_else(|| format!("Unsupported provider kind: {}", provider.kind))?;
        let descriptor = kind.descriptor();
        if descriptor.legacy.same_base {
            let url = reqwest::Url::parse(provider.base_url.trim_end_matches('/'))
                .map_err(|e| format!("Invalid existing {} provider URL: {e}", descriptor.label))?;
            if singleton_urls
                .get(&provider.kind)
                .is_some_and(|existing| existing != &url)
            {
                return Err(format!(
                    "Existing {} providers use different base URLs. Reconcile them before upgrading; no credentials have been changed.",
                    descriptor.label
                ));
            }
            singleton_urls.insert(provider.kind.clone(), url);
        }
        let group = if descriptor.fixed_id.is_none() {
            let url = reqwest::Url::parse(provider.base_url.trim_end_matches('/'))
                .map_err(|e| format!("Invalid existing provider URL: {e}"))?;
            format!("{}:{url}", provider.kind)
        } else {
            provider.kind.clone()
        };
        let target = canonical
            .entry(group)
            .or_insert_with(|| provider.id.clone())
            .clone();
        groups
            .entry(target.clone())
            .or_default()
            .push(provider.id.clone());
        remap.insert(provider.id.clone(), target.clone());
        let id = uuid::Uuid::new_v4().to_string();
        let suffix = if kind.oauth() {
            "credential"
        } else {
            "api_key"
        };
        let old_name = format!("provider.{}.{suffix}", provider.id);
        let value = store.secret_get(&old_name).await?;
        let value = value.or_else(|| descriptor.legacy.key.map(str::to_owned));
        let new_name = format!("credential.{id}.{suffix}");
        let encrypted = value
            .as_deref()
            .map(|value| store.encrypt(&new_name, value))
            .transpose()?;
        entries.push((provider.clone(), target, id, old_name, new_name, encrypted));
    }
    transaction_with_retry!(store.database, |mut tx| {
        if Setting::filter_by_name(MARKER).first().exec(&mut tx).await?.is_some() { return Ok(Ok(())); }
        let keys = VirtualKey::all().exec(&mut tx).await?;
        for key in keys {
            let mut permissions: Vec<String> = match serde_json::from_str(&key.providers) {
                Ok(value) => value, Err(e) => return Ok(Err(format!("Cannot migrate virtual key provider restrictions: {e}"))),
            };
            if !permissions.is_empty() {
                for ids in groups.values().filter(|ids| ids.len() > 1) {
                    let allowed = ids.iter().filter(|id| permissions.contains(id)).count();
                    if allowed > 0 && allowed < ids.len() {
                        return Ok(Err(format!(
                            "Virtual key '{}' ({}) restricts only some providers being merged ({}). Remove or recreate this key with access to all or none of those providers before migration.",
                            key.name, key.id, ids.join(", ")
                        )));
                    }
                }
            }
            for provider in &mut permissions { if let Some(id) = remap.get(provider) { *provider = id.clone(); } }
            permissions.sort(); permissions.dedup();
            let permissions = match serde_json::to_string(&permissions) { Ok(value) => value, Err(e) => return Ok(Err(e.to_string())) };
            VirtualKey::upsert_by_id(key.id.as_str()).name(key.name).key_hash(key.key_hash).providers(permissions).models(key.models).created_at(key.created_at).exec(&mut tx).await?;
        }
        for (provider, target, id, old_name, new_name, encrypted) in &entries {
            toasty::create!(ProviderCredential { id: id.clone(), provider_id: target.clone(), name: provider.name.clone(), weight: 1, enabled: true }).exec(&mut tx).await?;
            if let Some(value) = encrypted { Secret::upsert_by_name(new_name.as_str()).value(value.clone()).exec(&mut tx).await?; }
            Secret::delete_by_name(&mut tx, old_name.as_str()).await?;
            if provider.id != *target {
                ProviderRecord::upsert_by_id(provider.id.as_str()).name(String::new()).kind(provider.kind.clone()).base_url(String::new()).deleted(true).exec(&mut tx).await?;
            }
        }
        Setting::upsert_by_name(MARKER).value("1".to_string()).exec(&mut tx).await?;
        tx.commit().await?;
        Ok(Ok(()))
    }).map_err(|e| e.to_string())?
}

/// Legacy credential migration checks.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    /// Dashboard credential lists expose account email without exposing OAuth secrets.
    #[tokio::test]
    async fn dashboard_lists_stored_account_emails_without_tokens() {
        use axum::body::to_bytes;
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*store.database.db).clone();
        for kind in ["agy", "codex", "claude"] {
            toasty::create!(ProviderRecord {
                id: kind.to_string(),
                name: kind.to_string(),
                kind: kind.to_string(),
                base_url: String::new(),
                deleted: false
            })
            .exec(&mut db)
            .await
            .unwrap();
            let id = format!("{kind}-account");
            toasty::create!(ProviderCredential {
                id: id.clone(),
                provider_id: kind.to_string(),
                name: "Personal".to_string(),
                weight: 1,
                enabled: true
            })
            .exec(&mut db)
            .await
            .unwrap();
            let name = format!("credential.{id}.credential");
            let json =
                serde_json::json!({"email":"person@example.test","access_token":"secret-token"})
                    .to_string();
            Secret::upsert_by_name(name.as_str())
                .value(store.encrypt(&name, &json).unwrap())
                .exec(&mut db)
                .await
                .unwrap();
            let response = list_endpoint(State(store.clone()), Path(kind.to_string())).await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 65536).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["credentials"][0]["email"], "person@example.test");
            assert!(!String::from_utf8_lossy(&body).contains("secret-token"));
        }
    }

    #[tokio::test]
    /// Merging legacy credentials must not widen virtual-key access.
    #[allow(clippy::too_many_lines)]
    async fn legacy_merge_preserves_credentials_and_refuses_to_widen_key_access() {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*store.database.db).clone();
        Setting::delete_by_name(&mut db, "credential_pool_migrated_v1")
            .await
            .unwrap();
        for id in ["first", "second"] {
            toasty::create!(ProviderRecord {
                id: id.to_string(),
                name: id.to_string(),
                kind: "agy".to_string(),
                base_url: String::new(),
                deleted: false
            })
            .exec(&mut db)
            .await
            .unwrap();
            let name = format!("provider.{id}.credential");
            let encrypted = store.encrypt(&name, id).unwrap();
            Secret::upsert_by_name(name.as_str())
                .value(encrypted)
                .exec(&mut db)
                .await
                .unwrap();
        }
        toasty::create!(VirtualKey {
            id: "key".to_string(),
            name: "restricted".to_string(),
            key_hash: vec![1],
            providers: "[\"first\"]".to_string(),
            models: "[]".to_string(),
            created_at: 1
        })
        .exec(&mut db)
        .await
        .unwrap();

        let error = migrate_legacy(&store).await.unwrap_err();
        assert!(error.contains("Virtual key 'restricted'"));
        assert!(error.contains("Remove or recreate"));
        assert_eq!(registry::list(&store.database).await.unwrap().len(), 2);
        assert_eq!(
            store
                .secret_get("provider.first.credential")
                .await
                .unwrap()
                .as_deref(),
            Some("first")
        );
        assert!(
            ProviderCredential::all()
                .exec(&mut db)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .setting_get("credential_pool_migrated_v1")
                .await
                .unwrap()
                .is_none()
        );

        VirtualKey::upsert_by_id("key")
            .name("restricted".to_string())
            .key_hash(vec![1])
            .providers("[\"first\",\"second\"]".to_string())
            .models("[]".to_string())
            .created_at(1)
            .exec(&mut db)
            .await
            .unwrap();
        migrate_legacy(&store).await.unwrap();
        assert_eq!(registry::list(&store.database).await.unwrap().len(), 1);
        let credentials = list(&store, "first").await.unwrap();
        assert_eq!(credentials.len(), 2);
        let mut values = Vec::new();
        for credential in credentials {
            values.push(
                store
                    .secret_get(&format!("credential.{}.credential", credential.id))
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        values.sort();
        assert_eq!(values, ["first", "second"]);
        assert!(
            store
                .secret_get("provider.first.credential")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .secret_get("provider.second.credential")
                .await
                .unwrap()
                .is_none()
        );
        let key = VirtualKey::filter_by_id("key")
            .first()
            .exec(&mut db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.providers, "[\"first\"]");
        migrate_legacy(&store).await.unwrap();
        assert_eq!(list(&store, "first").await.unwrap().len(), 2);
    }
}

/// Checks for incompatible legacy provider configurations.
#[cfg(test)]
mod incompatible_legacy_tests {
    use super::*;
    use crate::database::Database;

    /// Legacy Zen instances share the same official upstream regardless of old shim URLs.
    #[tokio::test]
    async fn different_zen_upstreams_are_merged() {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*store.database.db).clone();
        Setting::delete_by_name(&mut db, "credential_pool_migrated_v1")
            .await
            .unwrap();
        for id in ["one", "two"] {
            toasty::create!(ProviderRecord {
                id: id.to_string(),
                name: id.to_string(),
                kind: "zen".to_string(),
                base_url: format!("https://{id}.example/v1"),
                deleted: false
            })
            .exec(&mut db)
            .await
            .unwrap();
        }
        migrate_legacy(&store).await.unwrap();
        assert_eq!(registry::list(&store.database).await.unwrap().len(), 1);
    }
}

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
    storage::{ProviderCredential, ProviderRecord, Secret},
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
    /// Provider-reported subscription plan, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_type: Option<String>,
}

/// Provider-specific secrets stored per credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretKind {
    /// API key for API-key providers.
    ApiKey,
    /// Serialized OAuth state for OAuth providers.
    OAuth,
    /// `OpenCode` Console session cookie.
    ConsoleSession,
    /// `OpenCode` Console workspace identifier.
    ConsoleWorkspace,
    /// `OpenRouter` management key used for balances.
    OpenrouterManagementKey,
}

impl SecretKind {
    /// Every kind, so deletion removes all of a credential's secrets.
    pub(crate) const ALL: [Self; 5] = [
        Self::ApiKey,
        Self::OAuth,
        Self::ConsoleSession,
        Self::ConsoleWorkspace,
        Self::OpenrouterManagementKey,
    ];

    /// The secret whose presence marks a credential of a provider kind as connected.
    fn primary(kind: &str) -> Self {
        if crate::provider::ProviderKind::from_str(kind)
            .is_some_and(crate::provider::ProviderKind::oauth)
        {
            Self::OAuth
        } else {
            Self::ApiKey
        }
    }

    /// Stored secret name, which is also the encryption associated data.
    pub(crate) fn name(self, credential_id: &str) -> String {
        let suffix = match self {
            Self::ApiKey => "api_key",
            Self::OAuth => "credential",
            Self::ConsoleSession => "console_session",
            Self::ConsoleWorkspace => "console_workspace",
            Self::OpenrouterManagementKey => "openrouter_management_key",
        };
        format!("credential.{credential_id}.{suffix}")
    }
}

/// Builds public credential metadata, checking which rows have their primary
/// secret with one name query and no decryption.
async fn describe(
    store: &SettingsStore,
    rows: Vec<ProviderCredential>,
    kind: &str,
) -> Result<Vec<Credential>, String> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let secret = SecretKind::primary(kind);
    // Antigravity holds no secret: it is connected once a quota snapshot has been pushed.
    let name_of = |id: &str| {
        if kind == "agy" {
            crate::provider::antigravity::quota::setting_name(id)
        } else {
            secret.name(id)
        }
    };
    let names: Vec<String> = rows.iter().map(|row| name_of(&row.id)).collect();
    let mut db = (*store.database.db).clone();
    let present = if kind == "agy" {
        crate::storage::Setting::filter(crate::storage::Setting::fields().name().in_list(names))
            .select(crate::storage::Setting::fields().name())
            .exec(&mut db)
            .await
    } else {
        Secret::filter(Secret::fields().name().in_list(names))
            .select(Secret::fields().name())
            .exec(&mut db)
            .await
    }
    .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| Credential {
            connected: present.contains(&name_of(&row.id)),
            id: row.id,
            name: row.name,
            weight: row.weight,
            enabled: row.enabled,
            email: None,
            plan_type: None,
        })
        .collect())
}

/// Profile information stored alongside an OAuth token.
#[derive(Deserialize)]
struct AccountProfile {
    /// Account email supplied by the provider.
    email: Option<String>,
    /// Subscription plan saved from Codex token claims.
    plan_type: Option<String>,
}

/// Populate dashboard-only identity metadata without parsing it on inference requests.
async fn include_profiles(
    store: &SettingsStore,
    kind: &str,
    credentials: &mut [Credential],
) -> Result<(), String> {
    if !matches!(kind, "codex" | "claude") {
        return Ok(());
    }
    for credential in credentials
        .iter_mut()
        .filter(|credential| credential.connected)
    {
        if let Some(secret) = store
            .secret_get(&SecretKind::OAuth.name(&credential.id))
            .await?
        {
            let profile: AccountProfile = serde_json::from_str(&secret)
                .map_err(|_| "Invalid stored account profile".to_string())?;
            credential.email = profile.email.filter(|email| !email.trim().is_empty());
            if kind == "codex" {
                credential.plan_type = profile.plan_type.filter(|plan| !plan.trim().is_empty());
            }
        }
    }
    Ok(())
}

/// Retrieves a credential belonging to a provider, if present.
async fn get(
    store: &SettingsStore,
    provider: &registry::Provider,
    credential_id: &str,
) -> Result<Option<Credential>, String> {
    let row = ProviderCredential::filter_by_id(credential_id)
        .first()
        .exec(&mut (*store.database.db).clone())
        .await
        .map_err(|e| e.to_string())?
        .filter(|row| row.provider_id == provider.id);
    Ok(describe(store, row.into_iter().collect(), &provider.kind)
        .await?
        .pop())
}

/// Lists every credential of a provider.
async fn all(
    store: &SettingsStore,
    provider: &registry::Provider,
) -> Result<Vec<Credential>, String> {
    let rows = ProviderCredential::filter_by_provider_id(provider.id.as_str())
        .exec(&mut (*store.database.db).clone())
        .await
        .map_err(|e| e.to_string())?;
    describe(store, rows, &provider.kind).await
}

/// Draw enabled, connected credentials without replacement, proportional to
/// each remaining credential's weight.
pub(crate) async fn list(
    store: &SettingsStore,
    provider: &registry::Provider,
) -> Result<Vec<Credential>, String> {
    let rows = ProviderCredential::filter_by_provider_id(provider.id.as_str())
        .filter(ProviderCredential::fields().enabled().eq(true))
        .exec(&mut (*store.database.db).clone())
        .await
        .map_err(|e| e.to_string())?;
    let rows = rows.into_iter().filter(|row| row.weight > 0).collect();
    let mut remaining: Vec<_> = describe(store, rows, &provider.kind)
        .await?
        .into_iter()
        .filter(|item| item.connected)
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

/// Loads typed OAuth state after revalidating its owner: the provider must exist
/// with this kind and the credential must belong to it and be enabled. The caller
/// must hold the credential lock until it has finished using or refreshing the state.
pub(crate) async fn load_oauth<T: serde::de::DeserializeOwned>(
    store: &SettingsStore,
    provider_id: &str,
    credential_id: &str,
    kind: &str,
    provider_name: &str,
) -> Result<Option<T>, String> {
    crate::provider::login::require_credential(store, provider_id, credential_id, kind)
        .await
        .map_err(|(_, message)| message)?;
    store
        .secret_get(&SecretKind::OAuth.name(credential_id))
        .await?
        .map(|json| {
            serde_json::from_str(&json)
                .map_err(|_| format!("Invalid stored {provider_name} credentials"))
        })
        .transpose()
}

/// Checks auxiliary-secret ownership inside the transaction performing the write.
pub(crate) async fn owns_auxiliary_secret(
    tx: &mut toasty::Transaction<'_>,
    provider_id: &str,
    credential_id: &str,
    kind: &str,
) -> Result<bool, toasty::Error> {
    let provider = ProviderRecord::filter_by_id(provider_id)
        .first()
        .exec(&mut *tx)
        .await?;
    let credential = ProviderCredential::filter_by_id(credential_id)
        .first()
        .exec(&mut *tx)
        .await?;
    Ok(
        matches!(provider, Some(row) if !row.deleted && row.kind == kind)
            && matches!(credential, Some(row) if row.provider_id == provider_id),
    )
}

/// Callers hold the credential lock while writing OAuth state.
pub(crate) async fn save_oauth(
    store: &SettingsStore,
    provider_id: &str,
    credential_id: &str,
    kind: &str,
    value: &str,
) -> Result<(), String> {
    let name = SecretKind::OAuth.name(credential_id);
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
#[utoipa::path(get, path = "/api/providers/{id}/credentials", operation_id = "listCredentials", params(("id" = String, Path)), responses((status = 200, body = CredentialsResponse), (status = 404, description = "Provider not found", body = String, content_type = "text/plain"), (status = 500, description = "Credential storage error", body = String, content_type = "text/plain")))]
async fn list_endpoint(
    State(store): State<SettingsStore>,
    Path(provider_id): Path<String>,
) -> Response {
    let provider = match registry::get(&store.database, &provider_id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => return (StatusCode::NOT_FOUND, "Provider not found").into_response(),
        Err(e) => return internal(&e),
    };
    let mut credentials = match all(&store, &provider).await {
        Ok(credentials) => credentials,
        Err(e) => return internal(&e),
    };
    match include_profiles(&store, &provider.kind, &mut credentials).await {
        Ok(()) => Json(CredentialsResponse { credentials }).into_response(),
        Err(e) => internal(&e),
    }
}

/// Reloads a credential after a write, optionally with its account profile.
async fn written(
    store: &SettingsStore,
    provider_id: &str,
    id: &str,
    profile: bool,
) -> Result<Option<Credential>, String> {
    let Some(provider) = registry::get(&store.database, provider_id).await? else {
        return Ok(None);
    };
    let Some(mut credential) = get(store, &provider, id).await? else {
        return Ok(None);
    };
    if profile {
        include_profiles(store, &provider.kind, std::slice::from_mut(&mut credential)).await?;
    }
    Ok(Some(credential))
}

/// Creates a credential and stores any supplied secret.
#[utoipa::path(post, path = "/api/providers/{id}/credentials", operation_id = "createCredential", params(("id" = String, Path)), request_body = CredentialInput, responses((status = 201, body = Credential), (status = 400, description = "Invalid credential or malformed JSON", body = String, content_type = "text/plain"), (status = 404, description = "Provider or credential not found", body = String, content_type = "text/plain"), (status = 415, description = "Expected JSON content type", body = String, content_type = "text/plain"), (status = 422, description = "Invalid JSON body", body = String, content_type = "text/plain"), (status = 500, description = "Credential storage error", body = String, content_type = "text/plain")))]
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
    let secret_name = SecretKind::ApiKey.name(&id);
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
        Ok(StatusCode::CREATED) => match written(&store, &provider_id, &id, false).await {
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
#[utoipa::path(put, path = "/api/providers/{id}/credentials/{credential_id}", operation_id = "updateCredential", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = CredentialInput, responses((status = 200, body = Credential), (status = 400, description = "Invalid credential or malformed JSON", body = String, content_type = "text/plain"), (status = 404, description = "Credential not found", body = String, content_type = "text/plain"), (status = 415, description = "Expected JSON content type", body = String, content_type = "text/plain"), (status = 422, description = "Invalid JSON body", body = String, content_type = "text/plain"), (status = 500, description = "Credential storage error", body = String, content_type = "text/plain")))]
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
    let secret_name = SecretKind::ApiKey.name(&id);
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
        toasty::update!(ProviderCredential::filter_by_id(id.as_str()) {
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
        Ok(StatusCode::OK)
    });
    match result {
        Ok(StatusCode::OK) => match written(&store, &provider_id, &id, true).await {
            Ok(Some(value)) => Json(value).into_response(),
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
#[utoipa::path(delete, path = "/api/providers/{id}/credentials/{credential_id}", operation_id = "deleteCredential", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 204), (status = 404, description = "Credential not found", body = String, content_type = "text/plain"), (status = 500, description = "Credential storage error", body = String, content_type = "text/plain")))]
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
        Secret::filter(
            Secret::fields()
                .name()
                .in_list(SecretKind::ALL.map(|kind| kind.name(&id)).to_vec()),
        )
        .delete()
        .exec(&mut tx)
        .await?;
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

/// Credential storage checks.
#[cfg(test)]
mod tests;

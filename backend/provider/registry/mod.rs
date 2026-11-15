use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get as route_get,
};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;

use crate::{
    database::{Database, transaction_with_retry},
    provider::{PROVIDER_KINDS, common::ProviderDescriptor, credentials},
    server::settings::SettingsStore,
    storage::{ProviderCredential, ProviderRecord, Secret, VirtualKey},
};

/// Active configured provider instance exposed by the registry.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct Provider {
    /// Stable instance identifier.
    pub(crate) id: String,
    /// User-facing instance name.
    pub(crate) name: String,
    /// Backend kind identifier.
    pub(crate) kind: String,
    /// Configured upstream base URL.
    pub(crate) base_url: String,
    /// Whether the provider connects to a keyless upstream.
    pub(crate) keyless: bool,
}
impl From<ProviderRecord> for Provider {
    fn from(row: ProviderRecord) -> Self {
        Self {
            id: row.id,
            name: row.name,
            kind: row.kind,
            base_url: row.base_url,
            keyless: row.keyless,
        }
    }
}

/// Retrieves an active provider by identifier.
pub(crate) async fn get(database: &Database, id: &str) -> Result<Option<Provider>, String> {
    Ok(ProviderRecord::filter_by_id(id)
        .first()
        .exec(&mut (*database.db).clone())
        .await
        .map_err(|e| e.to_string())?
        .filter(|row| !row.deleted)
        .map(Provider::from))
}

/// Lists all active providers.
pub(crate) async fn list(database: &Database) -> Result<Vec<Provider>, String> {
    Ok(ProviderRecord::all()
        .exec(&mut (*database.db).clone())
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|row| !row.deleted)
        .map(Provider::from)
        .collect())
}

/// Request to create a provider instance.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct CreateProvider {
    /// Requested instance identifier for non-fixed kinds.
    id: Option<String>,
    /// Requested instance name for non-fixed kinds.
    name: Option<String>,
    /// Backend kind identifier.
    kind: String,
    /// Upstream base URL, empty when the backend supplies a default.
    #[serde(default)]
    base_url: String,
    /// Whether the provider connects to a keyless upstream.
    #[serde(default)]
    keyless: bool,
}

/// Mutable provider instance properties.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct UpdateProvider {
    /// Replacement display name, if supplied.
    name: Option<String>,
    /// Replacement upstream base URL, if supplied.
    base_url: Option<String>,
    /// Replacement keyless flag, if supplied.
    keyless: Option<bool>,
}
/// Validates provider identity, kind, and upstream URL.
fn validate(id: &str, name: &str, kind: &str, base_url: &str) -> Result<(), &'static str> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return Err("Provider ID must be 1–64 lowercase letters, digits, or hyphens");
    }
    if name.trim().is_empty() {
        return Err("Provider name must not be empty");
    }
    let kind = crate::provider::ProviderKind::from_str(kind).ok_or("Unsupported provider kind")?;
    if kind.as_str() == "zen" && !base_url.is_empty() {
        return Err("Zen uses a fixed upstream URL");
    }
    if kind.as_str() == "openrouter" && !base_url.is_empty() {
        return Err("OpenRouter uses a fixed upstream URL");
    }
    if kind.as_str() == "chatjimmy" && !base_url.is_empty() {
        return Err("ChatJimmy uses a fixed upstream URL");
    }
    if matches!(kind.as_str(), "commandcode" | "commandcode-go") && !base_url.is_empty() {
        return Err("Command Code uses a fixed upstream URL");
    }
    if base_url.is_empty() {
        if kind.api_key() && kind.default_base_url().is_none() {
            return Err("Provider base URL is required for this kind");
        }
    } else {
        let url = reqwest::Url::parse(base_url)
            .map_err(|_| "Provider base URL must be a valid HTTP(S) URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("Provider base URL must be HTTP(S) without embedded credentials");
        }
    }
    Ok(())
}

/// Checks whether another active provider conflicts by kind or base URL.
fn duplicate(rows: &[ProviderRecord], id: &str, kind: &str, base_url: &str) -> bool {
    rows.iter()
        .filter(|row| !row.deleted && row.id != id && row.kind == kind)
        .any(|row| {
            if PROVIDER_KINDS
                .iter()
                .find(|item| item.value == kind)
                .is_none_or(|item| item.fixed_id.is_some())
            {
                return true;
            }
            match (
                reqwest::Url::parse(row.base_url.trim_end_matches('/')),
                reqwest::Url::parse(base_url.trim_end_matches('/')),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => row.base_url == base_url,
            }
        })
}

/// Logs registry storage errors and returns a generic server response.
fn internal_error(error: &str) -> Response {
    tracing::error!("Provider registry error: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Provider registry error").into_response()
}

/// Provider kind listing.
#[derive(Serialize, utoipa::ToSchema)]
struct ProviderKindsResponse {
    /// Available kinds.
    kinds: &'static [ProviderDescriptor],
}

/// Provider instance listing.
#[derive(Serialize, utoipa::ToSchema)]
struct ProvidersResponse {
    /// Active providers.
    providers: Vec<Provider>,
}

/// Provider registry API.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        list_provider_kinds,
        list_providers,
        create_provider,
        update_provider,
        delete_provider
    ),
    components(schemas(
        Provider,
        ProviderDescriptor,
        ProviderKindsResponse,
        ProvidersResponse,
        CreateProvider,
        UpdateProvider
    ))
)]
pub(crate) struct Api;

/// Returns the supported provider kind descriptors.
#[utoipa::path(get, path = "/api/provider-kinds", operation_id = "listProviderKinds", responses((status = 200, body = ProviderKindsResponse)))]
async fn list_provider_kinds() -> impl IntoResponse {
    Json(ProviderKindsResponse {
        kinds: PROVIDER_KINDS,
    })
}

/// Returns the active provider instances.
#[utoipa::path(get, path = "/api/providers", operation_id = "listProviders", responses((status = 200, body = ProvidersResponse), (status = 500, description = "Provider registry error", body = String, content_type = "text/plain")))]
async fn list_providers(State(store): State<SettingsStore>) -> Response {
    let providers = match list(&store.database).await {
        Ok(providers) => providers,
        Err(e) => return internal_error(&e),
    };
    Json(ProvidersResponse { providers }).into_response()
}

/// Creates a provider after validating identity and uniqueness.
#[utoipa::path(post, path = "/api/providers", operation_id = "createProvider", request_body = CreateProvider, responses((status = 201, body = Provider), (status = 400, description = "Invalid provider or malformed JSON", body = String, content_type = "text/plain"), (status = 409, description = "Provider conflicts with existing configuration", body = String, content_type = "text/plain"), (status = 415, description = "Expected JSON content type", body = String, content_type = "text/plain"), (status = 422, description = "Invalid JSON body", body = String, content_type = "text/plain"), (status = 500, description = "Provider registry error", body = String, content_type = "text/plain")))]
#[allow(clippy::too_many_lines)]
async fn create_provider(
    State(store): State<SettingsStore>,
    Json(input): Json<CreateProvider>,
) -> Response {
    let Some(kind) = PROVIDER_KINDS.iter().find(|kind| kind.value == input.kind) else {
        return (StatusCode::BAD_REQUEST, "Unsupported provider kind").into_response();
    };
    let (id, name) = match (kind.fixed_id, kind.fixed_name, &input.id, &input.name) {
        (Some(id), Some(name), None, None) => (id.to_string(), name.to_string()),
        (Some(_), Some(_), _, _) => {
            return (
                StatusCode::BAD_REQUEST,
                "Built-in provider ID and name are fixed",
            )
                .into_response();
        }
        (None, None, Some(id), Some(name)) => {
            if PROVIDER_KINDS
                .iter()
                .any(|kind| kind.fixed_id == Some(id.as_str()))
            {
                return (
                    StatusCode::BAD_REQUEST,
                    "Provider ID is reserved for a built-in provider",
                )
                    .into_response();
            }
            (id.clone(), name.clone())
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "Custom provider ID and name are required",
            )
                .into_response();
        }
    };
    let keyless = if input.kind == "chatjimmy" {
        true
    } else if input.kind == "openai" {
        input.keyless
    } else {
        false
    };
    if let Err(e) = validate(&id, &name, &input.kind, &input.base_url) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    let provider_lock = crate::provider::login::credential_lock(&format!("provider:{id}")).await;
    let _provider_guard = provider_lock.lock().await;
    let result = transaction_with_retry!(store.database, |mut tx| {
        let rows = ProviderRecord::all().exec(&mut tx).await?;
        if duplicate(&rows, &id, &input.kind, &input.base_url) {
            return Ok(Some("Provider ID or provider kind/base URL already exists"));
        }
        if let Some(existing) = rows.iter().find(|row| row.id == id) {
            if !existing.deleted || kind.fixed_id.is_none() || existing.kind != input.kind {
                return Ok(Some("Provider ID or provider kind/base URL already exists"));
            }
            for key in VirtualKey::all().exec(&mut tx).await? {
                if key.revoked_at.is_some() {
                    continue;
                }
                let permissions: Vec<String> = match serde_json::from_str(&key.providers) {
                    Ok(value) => value,
                    Err(_) => {
                        return Ok(Some(
                            "Cannot safely recreate provider: invalid virtual key restrictions",
                        ));
                    }
                };
                if permissions.contains(&id) {
                    return Ok(Some(
                        "Revoke affected virtual keys before recreating this provider",
                    ));
                }
            }
        }
        ProviderRecord::upsert_by_id(id.as_str())
            .name(name.clone())
            .kind(input.kind.clone())
            .base_url(input.base_url.clone())
            .keyless(keyless)
            .deleted(false)
            .exec(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(None)
    });
    match result {
        Ok(Some(message)) => (StatusCode::CONFLICT, message).into_response(),
        Err(e) => internal_error(&e.to_string()),
        Ok(None) => (
            StatusCode::CREATED,
            Json(Provider {
                id,
                name,
                kind: input.kind,
                base_url: input.base_url,
                keyless,
            }),
        )
            .into_response(),
    }
}

/// Updates a provider's mutable properties.
#[utoipa::path(put, path = "/api/providers/{id}", operation_id = "updateProvider", params(("id" = String, Path)), request_body = UpdateProvider, responses((status = 200, body = Provider), (status = 400, description = "Invalid provider or malformed JSON", body = String, content_type = "text/plain"), (status = 404, description = "Provider not found", body = String, content_type = "text/plain"), (status = 409, description = "Provider kind or base URL already exists", body = String, content_type = "text/plain"), (status = 415, description = "Expected JSON content type", body = String, content_type = "text/plain"), (status = 422, description = "Invalid JSON body", body = String, content_type = "text/plain"), (status = 500, description = "Provider registry error", body = String, content_type = "text/plain")))]
async fn update_provider(
    State(store): State<SettingsStore>,
    Path(id): Path<String>,
    Json(input): Json<UpdateProvider>,
) -> Response {
    let result = transaction_with_retry!(store.database, |mut tx| {
        let rows = ProviderRecord::all().exec(&mut tx).await?;
        let Some(row) = rows.iter().find(|row| row.id == id && !row.deleted) else {
            return Ok(Err((StatusCode::NOT_FOUND, "Provider not found")));
        };
        let kind = PROVIDER_KINDS.iter().find(|kind| kind.value == row.kind);
        let name = match (kind.and_then(|kind| kind.fixed_name), &input.name) {
            (Some(_), Some(_)) => {
                return Ok(Err((
                    StatusCode::BAD_REQUEST,
                    "Built-in provider name is fixed",
                )));
            }
            (Some(name), None) => name.to_string(),
            (None, Some(name)) => name.clone(),
            (None, None) => {
                return Ok(Err((
                    StatusCode::BAD_REQUEST,
                    "Custom provider name is required",
                )));
            }
        };
        let base_url = input
            .base_url
            .clone()
            .unwrap_or_else(|| row.base_url.clone());
        if let Err(e) = validate(&id, &name, &row.kind, &base_url) {
            return Ok(Err((StatusCode::BAD_REQUEST, e)));
        }
        let keyless = if row.kind == "chatjimmy" {
            true
        } else if row.kind == "openai" {
            input.keyless.unwrap_or(row.keyless)
        } else {
            false
        };
        if duplicate(&rows, &id, &row.kind, &base_url) {
            return Ok(Err((
                StatusCode::CONFLICT,
                "Provider kind or base URL already exists",
            )));
        }
        toasty::update!(ProviderRecord::filter_by_id(id.as_str()) {
            name: name.clone(),
            base_url: base_url.clone(),
            keyless: keyless
        })
        .exec(&mut tx)
        .await?;
        tx.commit().await?;
        Ok(Ok(Provider {
            id: id.clone(),
            name,
            kind: row.kind.clone(),
            base_url,
            keyless,
        }))
    });
    match result {
        Ok(Ok(provider)) => Json(provider).into_response(),
        Ok(Err((status, e))) => (status, e).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Deletes a provider only when it has no credentials.
#[utoipa::path(delete, path = "/api/providers/{id}", operation_id = "deleteProvider", params(("id" = String, Path)), responses((status = 204), (status = 404, description = "Provider not found", body = String, content_type = "text/plain"), (status = 409, description = "Remove all credentials before deleting this provider", body = String, content_type = "text/plain"), (status = 500, description = "Provider registry error", body = String, content_type = "text/plain")))]
async fn delete_provider(State(store): State<SettingsStore>, Path(id): Path<String>) -> Response {
    let provider_lock = crate::provider::login::credential_lock(&format!("provider:{id}")).await;
    let _provider_guard = provider_lock.lock().await;
    let result = transaction_with_retry!(store.database, |mut tx| {
        let Some(row) = ProviderRecord::filter_by_id(id.as_str())
            .first()
            .exec(&mut tx)
            .await?
            .filter(|row| !row.deleted)
        else {
            return Ok(StatusCode::NOT_FOUND);
        };
        let credentials = ProviderCredential::filter_by_provider_id(id.as_str())
            .exec(&mut tx)
            .await?;
        if !credentials.is_empty() {
            if !row.keyless {
                return Ok(StatusCode::CONFLICT);
            }
            for cred in credentials {
                ProviderCredential::delete_by_id(&mut tx, cred.id.as_str()).await?;
                Secret::delete_by_name(
                    &mut tx,
                    &crate::provider::credentials::SecretKind::ApiKey.name(&cred.id),
                )
                .await?;
            }
        }
        toasty::update!(ProviderRecord::filter_by_id(id.as_str()) {
            name: String::new(),
            base_url: String::new(),
            keyless: false,
            deleted: true
        })
        .exec(&mut tx)
        .await?;
        tx.commit().await?;
        Ok(StatusCode::NO_CONTENT)
    });
    match result {
        Ok(StatusCode::NO_CONTENT) => StatusCode::NO_CONTENT.into_response(),
        Ok(StatusCode::CONFLICT) => (
            StatusCode::CONFLICT,
            "Remove all credentials before deleting this provider",
        )
            .into_response(),
        Ok(_) => (StatusCode::NOT_FOUND, "Provider not found").into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Builds provider registry management routes.
pub(crate) fn router(store: SettingsStore) -> Router {
    Router::new()
        .route("/api/provider-kinds", route_get(list_provider_kinds))
        .route(
            "/api/providers",
            route_get(list_providers).post(create_provider),
        )
        .route(
            "/api/providers/{id}",
            axum::routing::put(update_provider).delete(delete_provider),
        )
        .with_state(store.clone())
        .merge(credentials::router(store))
}

/// Provider registry and credential lifecycle integration checks.
#[cfg(test)]
mod tests;

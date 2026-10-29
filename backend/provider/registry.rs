use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get as route_get,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    database::Database,
    provider::credentials,
    server::settings::SettingsStore,
    storage::{ProviderCredential, ProviderRecord, Secret, Setting, VirtualKey},
};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Provider {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) base_url: String,
}

impl From<ProviderRecord> for Provider {
    fn from(row: ProviderRecord) -> Self {
        Self {
            id: row.id,
            name: row.name,
            kind: row.kind,
            base_url: row.base_url,
        }
    }
}

pub(crate) async fn get(database: &Database, id: &str) -> Result<Option<Provider>, String> {
    Ok(ProviderRecord::filter_by_id(id)
        .first()
        .exec(&mut (*database.db).clone())
        .await
        .map_err(|e| e.to_string())?
        .filter(|row| !row.deleted)
        .map(Provider::from))
}

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

#[derive(Serialize)]
struct ProviderKind {
    value: &'static str,
    label: &'static str,
    oauth: bool,
    api_key: bool,
    fixed_id: Option<&'static str>,
    fixed_name: Option<&'static str>,
}
const PROVIDER_KINDS: &[ProviderKind] = &[
    ProviderKind {
        value: "agy",
        label: "Antigravity (Google)",
        oauth: true,
        api_key: false,
        fixed_id: Some("agy"),
        fixed_name: Some("Google Antigravity"),
    },
    ProviderKind {
        value: "codex",
        label: "Codex (OpenAI)",
        oauth: true,
        api_key: false,
        fixed_id: Some("codex"),
        fixed_name: Some("OpenAI Codex"),
    },
    ProviderKind {
        value: "zen",
        label: "Zen (OpenCode)",
        oauth: false,
        api_key: true,
        fixed_id: Some("zen"),
        fixed_name: Some("OpenCode Zen"),
    },
    ProviderKind {
        value: "go",
        label: "OpenCode Go",
        oauth: false,
        api_key: true,
        fixed_id: Some("go"),
        fixed_name: Some("OpenCode Go"),
    },
    ProviderKind {
        value: "openai",
        label: "OpenAI-compatible",
        oauth: false,
        api_key: true,
        fixed_id: None,
        fixed_name: None,
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProvider {
    id: Option<String>,
    name: Option<String>,
    kind: String,
    #[serde(default)]
    base_url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateProvider {
    name: Option<String>,
    base_url: Option<String>,
}

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
    let kind = PROVIDER_KINDS
        .iter()
        .find(|candidate| candidate.value == kind)
        .ok_or("Unsupported provider kind")?;
    if base_url.is_empty() {
        if kind.api_key && kind.value != "go" {
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

fn duplicate(rows: &[ProviderRecord], id: &str, kind: &str, base_url: &str) -> bool {
    rows.iter()
        .filter(|row| !row.deleted && row.id != id && row.kind == kind)
        .any(|row| {
            if kind != "openai" {
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

fn internal_error(error: &str) -> Response {
    tracing::error!("Provider registry error: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Provider registry error").into_response()
}

async fn list_provider_kinds() -> impl IntoResponse {
    Json(json!({"kinds": PROVIDER_KINDS}))
}

async fn list_providers(State(store): State<SettingsStore>) -> Response {
    let providers = match list(&store.database).await {
        Ok(providers) => providers,
        Err(e) => return internal_error(&e),
    };
    Json(json!({"providers": providers})).into_response()
}

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
    if let Err(e) = validate(&id, &name, &input.kind, &input.base_url) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    let result = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
            let rows = ProviderRecord::all().exec(&mut tx).await?;
            if duplicate(&rows, &id, &input.kind, &input.base_url) {
                return Ok(Some("Provider ID or provider kind/base URL already exists"));
            }
            if let Some(existing) = rows.iter().find(|row| row.id == id) {
                if !existing.deleted || kind.fixed_id.is_none() || existing.kind != input.kind {
                    return Ok(Some("Provider ID or provider kind/base URL already exists"));
                }
                for key in VirtualKey::all().exec(&mut tx).await? {
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
                .deleted(false)
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(None)
        })
        .await;
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
            }),
        )
            .into_response(),
    }
}

async fn update_provider(
    State(store): State<SettingsStore>,
    Path(id): Path<String>,
    Json(input): Json<UpdateProvider>,
) -> Response {
    let result = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
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
            if duplicate(&rows, &id, &row.kind, &base_url) {
                return Ok(Err((
                    StatusCode::CONFLICT,
                    "Provider kind or base URL already exists",
                )));
            }
            ProviderRecord::upsert_by_id(id.as_str())
                .name(name.clone())
                .kind(row.kind.clone())
                .base_url(base_url.clone())
                .deleted(false)
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(Ok(Provider {
                id: id.clone(),
                name,
                kind: row.kind.clone(),
                base_url,
            }))
        })
        .await;
    match result {
        Ok(Ok(provider)) => Json(provider).into_response(),
        Ok(Err((status, e))) => (status, e).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn delete_provider(State(store): State<SettingsStore>, Path(id): Path<String>) -> Response {
    let provider_lock = crate::provider::login::credential_lock(&format!("provider:{id}")).await;
    let _provider_guard = provider_lock.lock().await;
    // Holding every existing credential lock prevents refresh from racing deletion.
    let rows = match credentials::all(&store, &id).await {
        Ok(Some(rows)) => rows,
        Ok(None) => return (StatusCode::NOT_FOUND, "Provider not found").into_response(),
        Err(e) => return internal_error(&e),
    };
    let mut ids: Vec<_> = rows.into_iter().map(|row| row.id).collect();
    ids.sort();
    let mut guards = Vec::with_capacity(ids.len());
    for credential_id in &ids {
        guards.push(
            crate::provider::login::credential_lock(credential_id)
                .await
                .lock_owned()
                .await,
        );
    }
    let result = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
            let Some(row) = ProviderRecord::filter_by_id(id.as_str())
                .first()
                .exec(&mut tx)
                .await?
                .filter(|row| !row.deleted)
            else {
                return Ok(false);
            };
            let credentials = ProviderCredential::filter(
                ProviderCredential::fields().provider_id().eq(id.as_str()),
            )
            .exec(&mut tx)
            .await?;
            for credential in credentials {
                for suffix in [
                    "api_key",
                    "credential",
                    "console_session",
                    "console_workspace",
                ] {
                    Secret::delete_by_name(
                        &mut tx,
                        format!("credential.{}.{suffix}", credential.id).as_str(),
                    )
                    .await?;
                }
                ProviderCredential::delete_by_id(&mut tx, credential.id.as_str()).await?;
            }
            ProviderRecord::upsert_by_id(id.as_str())
                .name(String::new())
                .kind(row.kind)
                .base_url(String::new())
                .deleted(true)
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(true)
        })
        .await;
    drop(guards);
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "Provider not found").into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// Cut over built-in identities atomically, refusing occupied IDs and stale grants.
pub(crate) async fn migrate_fixed_identities(store: &SettingsStore) -> Result<(), String> {
    const MARKER: &str = "provider_fixed_identities_v1";
    store.database.retry(|| async {
        let mut db = (*store.database.db).clone();
        let mut tx = db.transaction().await?;
        if Setting::filter_by_name(MARKER).first().exec(&mut tx).await?.is_some() {
            return Ok(Ok(()));
        }
        let rows = ProviderRecord::all().exec(&mut tx).await?;
        let mut remap = std::collections::HashMap::new();
        for row in rows.iter().filter(|row| !row.deleted) {
            let Some(target) = PROVIDER_KINDS.iter().find(|kind| kind.value == row.kind).and_then(|kind| kind.fixed_id) else { continue };
            if rows.iter().any(|other| other.id == target && other.id != row.id) {
                return Ok(Err(format!("Cannot migrate provider '{}' to '{target}': ID is occupied, including by deleted providers. Resolve the identity conflict before upgrading.", row.id)));
            }
            if rows.iter().any(|other| !other.deleted && other.kind == row.kind && other.id != row.id) {
                return Ok(Err(format!("Cannot migrate multiple '{}' providers; reconcile them before upgrading.", row.kind)));
            }
            if row.id != target {
                remap.insert(row.id.clone(), target.to_string());
            }
        }
        let keys = VirtualKey::all().exec(&mut tx).await?;
        for key in keys {
            let mut permissions: Vec<String> = match serde_json::from_str(&key.providers) {
                Ok(value) => value,
                Err(e) => return Ok(Err(format!("Cannot migrate virtual key '{}' provider restrictions: {e}", key.id))),
            };
            for (old, target) in &remap {
                if permissions.contains(target) && !permissions.contains(old) {
                    return Ok(Err(format!("Virtual key '{}' ({}) already references destination provider '{}'; revoke or correct this key before migration.", key.name, key.id, target)));
                }
            }
            for permission in &mut permissions {
                if let Some(target) = remap.get(permission) { *permission = target.clone(); }
            }
            permissions.sort();
            permissions.dedup();
            let permissions = match serde_json::to_string(&permissions) {
                Ok(value) => value,
                Err(e) => return Ok(Err(e.to_string())),
            };
            VirtualKey::upsert_by_id(key.id.as_str()).name(key.name).key_hash(key.key_hash).providers(permissions).models(key.models).created_at(key.created_at).exec(&mut tx).await?;
        }
        for credential in ProviderCredential::all().exec(&mut tx).await? {
            if let Some(target) = remap.get(&credential.provider_id) {
                ProviderCredential::upsert_by_id(credential.id.as_str()).provider_id(target.clone()).name(credential.name).weight(credential.weight).enabled(credential.enabled).exec(&mut tx).await?;
            }
        }
        for row in rows.iter().filter(|row| !row.deleted) {
            let Some((target, name)) = PROVIDER_KINDS.iter().find(|kind| kind.value == row.kind).and_then(|kind| kind.fixed_id.zip(kind.fixed_name)) else { continue };
            ProviderRecord::upsert_by_id(target).name(name.to_string()).kind(row.kind.clone()).base_url(row.base_url.clone()).deleted(false).exec(&mut tx).await?;
            if row.id != target {
                ProviderRecord::upsert_by_id(row.id.as_str()).name(String::new()).kind(row.kind.clone()).base_url(String::new()).deleted(true).exec(&mut tx).await?;
            }
        }
        if !remap.is_empty() {
            Setting::delete_by_name(&mut tx, "model_catalog").await?;
        }
        Setting::upsert_by_name(MARKER).value("1".to_string()).exec(&mut tx).await?;
        tx.commit().await?;
        Ok(Ok(()))
    }).await.map_err(|e| e.to_string())?
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    async fn request(
        router: &Router,
        method: &str,
        path: &str,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn credentials_are_isolated_encrypted_and_deleted_with_provider() {
        let database = Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .await
            .unwrap();
        let router = router(store.clone());
        for id in ["first", "second"] {
            let body = json!({"id": id, "name": id, "kind": "openai", "base_url": format!("https://{id}.example/v1")});
            assert_eq!(
                request(&router, "POST", "/api/providers", &body.to_string())
                    .await
                    .0,
                StatusCode::CREATED
            );
        }
        let (status, credential) = request(
            &router,
            "POST",
            "/api/providers/first/credentials",
            r#"{"name":"primary","weight":3,"enabled":true,"api_key":"secret"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = credential["id"].as_str().unwrap();
        assert_eq!(credential["connected"], true);
        let secret_name = format!("credential.{id}.api_key");
        let row = Secret::filter_by_name(secret_name.as_str())
            .first()
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(row.value.as_slice(), b"secret");
        assert_eq!(
            store.secret_get(&secret_name).await.unwrap().as_deref(),
            Some("secret")
        );
        assert_eq!(
            request(
                &router,
                "DELETE",
                &format!("/api/providers/second/credentials/{id}"),
                ""
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(
                &router,
                "PUT",
                &format!("/api/providers/second/credentials/{id}"),
                r#"{"name":"stolen","weight":1,"enabled":true,"api_key":"replacement"}"#
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            store.secret_get(&secret_name).await.unwrap().as_deref(),
            Some("secret")
        );
        assert_eq!(credentials::list(&store, "first").await.unwrap().len(), 1);
        assert_eq!(
            request(
                &router,
                "PUT",
                &format!("/api/providers/first/credentials/{id}"),
                r#"{"name":"primary","weight":3,"enabled":false}"#
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(credentials::list(&store, "first").await.unwrap().is_empty());
        assert_eq!(
            request(
                &router,
                "PUT",
                &format!("/api/providers/first/credentials/{id}"),
                r#"{"name":"keyless","weight":2,"enabled":true,"api_key":""}"#
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            store.secret_get(&secret_name).await.unwrap().as_deref(),
            Some("")
        );
        assert_eq!(credentials::list(&store, "first").await.unwrap().len(), 1);
        assert_eq!(
            request(&router, "DELETE", "/api/providers/first", "")
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        assert!(store.secret_get(&secret_name).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn provider_uniqueness_and_credential_validation() {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let router = router(store);
        assert_eq!(
            request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
                .await
                .0,
            StatusCode::CREATED
        );
        assert_eq!(
            request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &router,
                "POST",
                "/api/providers/agy/credentials",
                r#"{"name":"A","weight":0,"enabled":true}"#
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &router,
                "POST",
                "/api/providers/agy/credentials",
                r#"{"name":"A","weight":1,"enabled":true,"api_key":"key"}"#
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &router,
                "POST",
                "/api/providers",
                r#"{"id":"c","name":"C","kind":"openai","base_url":"https://example.com/v1"}"#
            )
            .await
            .0,
            StatusCode::CREATED
        );
        assert_eq!(
            request(
                &router,
                "POST",
                "/api/providers",
                r#"{"id":"d","name":"D","kind":"openai","base_url":"https://example.com/v1/"}"#
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    #[tokio::test]
    async fn fixed_provider_identity_is_enforced() {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let router = router(store);
        let (_, kinds) = request(&router, "GET", "/api/provider-kinds", "").await;
        assert!(
            kinds["kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind["value"] == "agy"
                    && kind["fixed_id"] == "agy"
                    && kind["fixed_name"] == "Google Antigravity")
        );
        assert_eq!(
            request(
                &router,
                "POST",
                "/api/providers",
                r#"{"id":"agy","name":"custom","kind":"openai","base_url":"https://example.com"}"#
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &router,
                "POST",
                "/api/providers",
                r#"{"id":"wrong","name":"wrong","kind":"agy"}"#
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let (status, provider) =
            request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(provider["id"], "agy");
        assert_eq!(provider["name"], "Google Antigravity");
        assert_eq!(
            request(
                &router,
                "PUT",
                "/api/providers/agy",
                r#"{"name":"renamed"}"#
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(&router, "PUT", "/api/providers/agy", "{}").await.0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn provider_recreation_respects_virtual_keys() {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let router = router(store.clone());
        assert_eq!(
            request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
                .await
                .0,
            StatusCode::CREATED
        );
        assert_eq!(
            request(&router, "DELETE", "/api/providers/agy", "").await.0,
            StatusCode::NO_CONTENT
        );
        let mut db = (*store.database.db).clone();
        toasty::create!(VirtualKey {
            id: "key".to_string(),
            name: "restricted".to_string(),
            key_hash: vec![1],
            providers: "[\"agy\"]".to_string(),
            models: "[]".to_string(),
            created_at: 0
        })
        .exec(&mut db)
        .await
        .unwrap();
        assert_eq!(
            request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
                .await
                .0,
            StatusCode::CONFLICT
        );
        VirtualKey::delete_by_id(&mut db, "key").await.unwrap();
        assert_eq!(
            request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
                .await
                .0,
            StatusCode::CREATED
        );
        assert_eq!(
            request(&router, "DELETE", "/api/providers/agy", "").await.0,
            StatusCode::NO_CONTENT
        );
        toasty::create!(VirtualKey {
            id: "all".to_string(),
            name: "all".to_string(),
            key_hash: vec![3],
            providers: "[]".to_string(),
            models: "[]".to_string(),
            created_at: 0
        })
        .exec(&mut db)
        .await
        .unwrap();
        assert_eq!(
            request(&router, "POST", "/api/providers", r#"{"kind":"agy"}"#)
                .await
                .0,
            StatusCode::CREATED
        );
        assert_eq!(request(&router, "POST", "/api/providers", r#"{"id":"custom","name":"Custom","kind":"openai","base_url":"https://example.com"}"#).await.0, StatusCode::CREATED);
        assert_eq!(
            request(&router, "DELETE", "/api/providers/custom", "")
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(request(&router, "POST", "/api/providers", r#"{"id":"custom","name":"Custom","kind":"openai","base_url":"https://example.com"}"#).await.0, StatusCode::CONFLICT);
    }

    async fn cutover_fixture() -> SettingsStore {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*store.database.db).clone();
        Setting::delete_by_name(&mut db, "provider_fixed_identities_v1")
            .await
            .unwrap();
        toasty::create!(ProviderRecord {
            id: "old".to_string(),
            name: "Old".to_string(),
            kind: "agy".to_string(),
            base_url: String::new(),
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        toasty::create!(ProviderRecord {
            id: "agy".to_string(),
            name: String::new(),
            kind: "openai".to_string(),
            base_url: String::new(),
            deleted: true
        })
        .exec(&mut db)
        .await
        .unwrap();
        toasty::create!(ProviderCredential {
            id: "cred".to_string(),
            provider_id: "old".to_string(),
            name: "account".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut db)
        .await
        .unwrap();
        toasty::create!(VirtualKey {
            id: "key".to_string(),
            name: "restricted".to_string(),
            key_hash: vec![2],
            providers: "[\"old\"]".to_string(),
            models: "[]".to_string(),
            created_at: 0
        })
        .exec(&mut db)
        .await
        .unwrap();
        store
    }

    #[tokio::test]
    async fn identity_cutover_is_atomic_and_preserves_restrictions() {
        let store = cutover_fixture().await;
        let mut db = (*store.database.db).clone();
        assert!(
            migrate_fixed_identities(&store)
                .await
                .unwrap_err()
                .contains("occupied")
        );
        assert_eq!(
            ProviderCredential::filter_by_id("cred")
                .first()
                .exec(&mut db)
                .await
                .unwrap()
                .unwrap()
                .provider_id,
            "old"
        );
        assert_eq!(
            VirtualKey::filter_by_id("key")
                .first()
                .exec(&mut db)
                .await
                .unwrap()
                .unwrap()
                .providers,
            "[\"old\"]"
        );
        ProviderRecord::delete_by_id(&mut db, "agy").await.unwrap();
        VirtualKey::upsert_by_id("key")
            .name("restricted".to_string())
            .key_hash(vec![2])
            .providers("[\"agy\"]".to_string())
            .models("[]".to_string())
            .created_at(0)
            .exec(&mut db)
            .await
            .unwrap();
        assert!(
            migrate_fixed_identities(&store)
                .await
                .unwrap_err()
                .contains("already references")
        );
        VirtualKey::upsert_by_id("key")
            .name("restricted".to_string())
            .key_hash(vec![2])
            .providers("[\"old\"]".to_string())
            .models("[]".to_string())
            .created_at(0)
            .exec(&mut db)
            .await
            .unwrap();
        migrate_fixed_identities(&store).await.unwrap();
        assert_eq!(
            get(&store.database, "agy").await.unwrap().unwrap().name,
            "Google Antigravity"
        );
        assert!(get(&store.database, "old").await.unwrap().is_none());
        assert_eq!(
            ProviderCredential::filter_by_id("cred")
                .first()
                .exec(&mut db)
                .await
                .unwrap()
                .unwrap()
                .provider_id,
            "agy"
        );
        assert_eq!(
            VirtualKey::filter_by_id("key")
                .first()
                .exec(&mut db)
                .await
                .unwrap()
                .unwrap()
                .providers,
            "[\"agy\"]"
        );
        migrate_fixed_identities(&store).await.unwrap();
    }
}

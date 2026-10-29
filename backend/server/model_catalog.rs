use std::collections::HashMap;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{catalog::discover, registry};
use crate::server::{AppState, settings::SettingsStore};
use crate::storage::Setting;

/// Settings key for the persisted discovery snapshot.
const CACHE_KEY: &str = "model_catalog";
/// Maximum time allowed for one provider discovery.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Discovered models and the latest discovery error for a provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CatalogProvider {
    /// Registry identifier.
    id: String,
    /// Display name from the registry.
    name: String,
    /// Provider implementation kind.
    kind: String,
    /// Endpoint used to decide whether cached results remain valid.
    #[serde(default)]
    base_url: String,
    /// Last successfully discovered model records.
    models: Vec<Value>,
    /// Most recent discovery failure, if any.
    error: Option<String>,
}

/// Persisted snapshot of provider discovery results.
#[derive(Default, Serialize, Deserialize)]
struct Catalog {
    /// Providers reconciled with the registry on load.
    providers: Vec<CatalogProvider>,
}

/// Load the snapshot and reconcile it against the current registry.
// Reconcile with the registry on every read: new providers appear immediately and
// deleted providers never leak out of an old persisted snapshot.
async fn load(store: &SettingsStore) -> Result<Catalog, String> {
    let cached = match store.setting_get(CACHE_KEY).await? {
        Some(value) => serde_json::from_str::<Catalog>(&value).map_err(|e| e.to_string())?,
        None => Catalog::default(),
    };
    let mut cached: HashMap<_, _> = cached
        .providers
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();
    let providers = registry::list(&store.database)
        .await?
        .into_iter()
        .map(|p| {
            let prior = cached
                .remove(&p.id)
                .filter(|old| old.kind == p.kind && old.base_url == p.base_url);
            CatalogProvider {
                id: p.id,
                name: p.name,
                kind: p.kind,
                base_url: p.base_url,
                models: prior.as_ref().map(|p| p.models.clone()).unwrap_or_default(),
                error: prior.and_then(|p| p.error),
            }
        })
        .collect();
    Ok(Catalog { providers })
}

/// Persist a catalog snapshot transactionally.
async fn save(store: &SettingsStore, catalog: &Catalog) -> Result<(), String> {
    let value = serde_json::to_string(catalog).map_err(|e| e.to_string())?;
    store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
            Setting::upsert_by_name(CACHE_KEY)
                .value(value.as_str())
                .exec(&mut tx)
                .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())
}

/// Discover providers concurrently and retain last successful models on failure.
async fn refresh_catalog(state: &AppState) -> Result<Catalog, String> {
    let current = registry::list(&state.settings.database).await?;
    let mut catalog = load(&state.settings).await?;
    let results = join_all(current.iter().map(|provider| async move {
        tokio::time::timeout(DISCOVERY_TIMEOUT, discover(state, provider))
            .await
            .unwrap_or_else(|_| Err("Provider model discovery timed out".into()))
    }))
    .await;
    let mut results: HashMap<_, _> = current
        .into_iter()
        .zip(results)
        .map(|(p, r)| (p.id, r))
        .collect();
    for provider in &mut catalog.providers {
        if let Some(result) = results.remove(&provider.id) {
            match result {
                Ok(models) => {
                    provider.models = models;
                    provider.error = None;
                }
                Err(error) => provider.error = Some(error),
            }
        }
    }
    save(&state.settings, &catalog).await?;
    // Registry may change while network calls are running.
    load(&state.settings).await
}

/// Refresh the catalog, logging failures for background callers.
pub(crate) async fn refresh(state: &AppState) {
    if refresh_catalog(state).await.is_err() {
        tracing::error!("Failed to refresh model catalog");
    }
}

/// Return the current reconciled catalog.
async fn get_catalog(State(state): State<AppState>) -> Response {
    load(&state.settings).await.map_or_else(
        |_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load model catalog",
            )
                .into_response()
        },
        |catalog| Json(catalog).into_response(),
    )
}

/// Refresh the catalog on demand and return the result.
async fn post_refresh(State(state): State<AppState>) -> Response {
    refresh_catalog(&state).await.map_or_else(
        |_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to refresh model catalog",
            )
                .into_response()
        },
        |catalog| Json(catalog).into_response(),
    )
}

/// Build dashboard routes for catalog access and refresh.
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/model-catalog", get(get_catalog))
        .route("/api/model-catalog/refresh", post(post_refresh))
        .with_state(state)
}

/// Catalog persistence and registry reconciliation tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::database::Database;
    use axum::{
        body::Body,
        http::{Request, header},
    };
    use serde_json::json;
    use tower::ServiceExt;

    /// Create an in-memory settings store.
    async fn store() -> SettingsStore {
        let database = Database::open(":memory:").await.unwrap();
        SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .await
            .unwrap()
    }

    /// Send a JSON request to the provider registry.
    async fn registry_request(
        store: &SettingsStore,
        method: &str,
        path: &str,
        body: Value,
    ) -> StatusCode {
        registry::router(store.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Cached models survive reopening and follow registry changes.
    #[tokio::test]
    async fn cache_survives_store_recreation_and_tracks_registry() {
        let store = store().await;
        let provider = |id| json!({"id":id,"name":id,"kind":"openai","base_url":format!("http://{id}.localhost/v1")});
        assert_eq!(
            registry_request(&store, "POST", "/api/providers", provider("first")).await,
            StatusCode::CREATED
        );
        let mut catalog = load(&store).await.unwrap();
        catalog.providers[0].models = vec![json!({"id":"cached", "metadata":42})];
        catalog.providers[0].error = Some("offline".into());
        save(&store, &catalog).await.unwrap();
        let reopened = SettingsStore::new(
            store.database.clone(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let cached = load(&reopened).await.unwrap();
        assert_eq!(cached.providers[0].models[0]["metadata"], 42);
        assert_eq!(cached.providers[0].error.as_deref(), Some("offline"));
        assert_eq!(
            registry_request(&store, "POST", "/api/providers", provider("second")).await,
            StatusCode::CREATED
        );
        assert_eq!(
            registry_request(&store, "DELETE", "/api/providers/first", Value::Null).await,
            StatusCode::NO_CONTENT
        );
        let updated = load(&reopened).await.unwrap();
        assert_eq!(updated.providers.len(), 1);
        assert_eq!(updated.providers[0].id, "second");
        assert_eq!(updated.providers[0].models, Vec::<Value>::new());
    }
}

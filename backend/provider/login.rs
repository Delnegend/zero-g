use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Weak};

use axum::http::{HeaderMap, StatusCode, header};
use tokio::sync::Mutex;

/// Credential operation failure, optionally carrying an upstream HTTP status.
#[derive(Debug)]
pub(crate) struct CredentialError {
    /// Human-readable error detail.
    pub(crate) message: String,
    /// Status returned by the upstream, if available.
    pub(crate) upstream_status: Option<reqwest::StatusCode>,
}

impl CredentialError {
    /// Creates an error from an upstream response.
    pub(crate) const fn upstream(status: reqwest::StatusCode, message: String) -> Self {
        Self {
            message,
            upstream_status: Some(status),
        }
    }
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CredentialError {}

impl From<String> for CredentialError {
    fn from(message: String) -> Self {
        Self {
            message,
            upstream_status: None,
        }
    }
}

/// Weakly held per-credential locks used to serialize credential operations.
static REFRESH_LOCKS: LazyLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns the shared lock for an identifier, removing expired entries.
pub(crate) async fn credential_lock(id: &str) -> Arc<Mutex<()>> {
    let mut locks = REFRESH_LOCKS.lock().await;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(id.to_string(), Arc::downgrade(&lock));
    lock
}

/// Rejects requests whose Origin does not match the Host and effective port.
pub(crate) fn same_origin(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| (StatusCode::FORBIDDEN, "Origin required".into()))?;
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| (StatusCode::FORBIDDEN, "Host required".into()))?;
    let url = reqwest::Url::parse(origin)
        .map_err(|_| (StatusCode::FORBIDDEN, "Invalid origin".into()))?;
    let host_url = reqwest::Url::parse(&format!("{}://{host}/", url.scheme()))
        .map_err(|_| (StatusCode::FORBIDDEN, "Invalid host".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host() != host_url.host()
        || url.port_or_known_default() != host_url.port_or_known_default()
        || !url.username().is_empty()
        || url.password().is_some()
        || !host_url.username().is_empty()
        || host_url.password().is_some()
        || host_url.path() != "/"
        || host_url.query().is_some()
        || host_url.fragment().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err((StatusCode::FORBIDDEN, "Origin mismatch".into()));
    }
    Ok(())
}

/// Creates a bad-request response with the supplied message.
pub(crate) fn error(message: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, message.into())
}

/// Requires a configured provider of the expected kind.
pub(crate) async fn require_provider(
    store: &crate::server::settings::SettingsStore,
    id: &str,
    kind: &str,
) -> Result<(), (StatusCode, String)> {
    let provider = crate::provider::registry::get(&store.database, id)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load provider".into(),
            )
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Provider not found".into()))?;
    if provider.kind != kind {
        return Err(error("Provider kind does not match login"));
    }
    Ok(())
}

/// Requires an enabled credential belonging to a provider of the expected kind.
pub(crate) async fn require_credential(
    store: &crate::server::settings::SettingsStore,
    provider_id: &str,
    credential_id: &str,
    kind: &str,
) -> Result<(), (StatusCode, String)> {
    require_provider(store, provider_id, kind).await?;
    let credential = crate::provider::credentials::get(store, provider_id, credential_id)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load credential".into(),
            )
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Credential not found".into()))?;
    if !credential.enabled {
        return Err(error("Credential is disabled"));
    }
    Ok(())
}

/// Credential isolation and origin validation checks.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::same_origin;
    use axum::http::{HeaderMap, HeaderValue, header};

    /// Deleted credentials cannot be restored by stale login state.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn credentials_are_isolated_and_cannot_be_resurrected() {
        use crate::provider::antigravity::auth::{AuthManager, Credentials};
        use crate::{database::Database, server::settings::SettingsStore};
        use axum::{
            body::{Body, to_bytes},
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;

        let database = Database::open(":memory:").await.unwrap();
        let store = SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .await
            .unwrap();
        let providers = crate::provider::registry::router(store.clone());
        for kind in ["agy", "codex"] {
            let request = Request::builder()
                .method("POST")
                .uri("/api/providers")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"kind":"{kind}"}}"#)))
                .unwrap();
            assert_eq!(
                providers.clone().oneshot(request).await.unwrap().status(),
                StatusCode::CREATED
            );
        }
        let credentials = crate::provider::credentials::router(store.clone());
        let mut ids = Vec::new();
        for name in ["first", "second"] {
            let request = Request::builder()
                .method("POST")
                .uri("/api/providers/agy/credentials")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"name":"{name}","weight":1,"enabled":true}}"#
                )))
                .unwrap();
            let response = credentials.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            ids.push(value["id"].as_str().unwrap().to_string());
            let creds = Credentials {
                access_token: name.into(),
                refresh_token: None,
                project_id: name.into(),
                expires_at: None,
                email: None,
            };
            AuthManager::save(&store, "agy", ids.last().unwrap(), &creds)
                .await
                .unwrap();
        }
        let first = AuthManager::load(store.clone(), "agy", &ids[0])
            .await
            .unwrap()
            .unwrap();
        let second = AuthManager::load(store.clone(), "agy", &ids[1])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.get_credentials().await.unwrap().access_token, "first");
        assert_eq!(
            second.get_credentials().await.unwrap().access_token,
            "second"
        );
        let replacement = Credentials {
            access_token: "replacement".into(),
            refresh_token: None,
            project_id: "replacement".into(),
            expires_at: None,
            email: None,
        };
        AuthManager::save(&store, "agy", &ids[0], &replacement)
            .await
            .unwrap();
        assert_eq!(
            first.get_credentials().await.unwrap().access_token,
            "replacement"
        );
        assert_eq!(
            second.get_credentials().await.unwrap().access_token,
            "second"
        );
        assert!(
            AuthManager::save(&store, "codex", &ids[0], &replacement)
                .await
                .is_err()
        );
        assert!(
            AuthManager::load(store.clone(), "codex", &ids[0])
                .await
                .is_err()
        );
        assert!(
            crate::provider::codex::auth::CodexAuthManager::load(store.clone(), "codex", &ids[0])
                .await
                .is_err()
        );

        let request = Request::builder()
            .method("PUT")
            .uri(format!("/api/providers/agy/credentials/{}", ids[0]))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"first","weight":1,"enabled":false}"#))
            .unwrap();
        assert!(
            credentials
                .clone()
                .oneshot(request)
                .await
                .unwrap()
                .status()
                .is_success()
        );
        let late = Credentials {
            access_token: "late".into(),
            refresh_token: None,
            project_id: "late".into(),
            expires_at: None,
            email: None,
        };
        assert!(first.get_credentials().await.is_err());
        assert!(
            AuthManager::save(&store, "agy", &ids[0], &late)
                .await
                .is_err()
        );

        let request = Request::builder()
            .method("DELETE")
            .uri(format!("/api/providers/agy/credentials/{}", ids[1]))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            credentials.clone().oneshot(request).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert!(second.get_credentials().await.is_err());
        assert!(
            AuthManager::save(&store, "agy", &ids[1], &late)
                .await
                .is_err()
        );
        assert!(
            store
                .secret_get(&format!("credential.{}.credential", ids[1]))
                .await
                .unwrap()
                .is_none()
        );

        let request = Request::builder()
            .method("DELETE")
            .uri("/api/providers/agy")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            providers.clone().oneshot(request).await.unwrap().status(),
            StatusCode::CONFLICT
        );
        let request = Request::builder()
            .method("DELETE")
            .uri(format!("/api/providers/agy/credentials/{}", ids[0]))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            credentials.oneshot(request).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );

        let request = Request::builder()
            .method("DELETE")
            .uri("/api/providers/agy")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            providers.oneshot(request).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert!(
            AuthManager::save(&store, "agy", &ids[0], &late)
                .await
                .is_err()
        );
        assert!(
            store
                .secret_get(&format!("credential.{}.credential", ids[0]))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Origin checks use Host rather than forwarded headers.
    #[test]
    fn origin_checks_public_host_without_trusting_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://example.com"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("example.com:443"));
        assert!(same_origin(&headers).is_ok());
        headers.insert(header::HOST, HeaderValue::from_static("internal:8080"));
        headers.insert("x-forwarded-host", HeaderValue::from_static("example.com"));
        assert!(same_origin(&headers).is_err());
    }
}

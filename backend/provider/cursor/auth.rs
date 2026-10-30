use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::provider::login::{credential_lock, require_credential};
use crate::server::settings::SettingsStore;

/// Cursor OAuth token refresh endpoint.
const REFRESH_URL: &str = "https://api2.cursor.sh/oauth/token";
/// Public Cursor CLI OAuth client identifier.
const CLIENT_ID: &str = env!("CURSOR_OAUTH_CLIENT_ID");

/// Encrypted OAuth secret stored for a Cursor credential.
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Credentials {
    /// Bearer token for Cursor requests.
    pub(super) access_token: String,
    /// Refresh token retained across token rotation.
    pub(super) refresh_token: String,
    /// JWT expiration in Unix milliseconds.
    pub(super) expires_at: i64,
}

impl Credentials {
    /// Determines expiration from the token or a one-hour fallback.
    pub(super) fn new(access_token: String, refresh_token: String) -> Self {
        let now = now_ms();
        let expires_at = access_token
            .split('.')
            .nth(1)
            .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| value.get("exp").and_then(serde_json::Value::as_i64))
            .and_then(|exp| exp.checked_mul(1000))
            .unwrap_or_else(|| now.saturating_add(3_600_000));
        Self {
            access_token,
            refresh_token,
            expires_at,
        }
    }
}

/// Current Unix time in milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Returns a current token, serializing refreshes across requests for a credential.
pub(super) async fn access_token(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
) -> Result<String, String> {
    let lock = credential_lock(credential).await;
    let _guard = lock.lock().await;
    require_credential(store, provider, credential, "cursor")
        .await
        .map_err(|(_, message)| message)?;
    let secret = store
        .secret_get(&format!("credential.{credential}.credential"))
        .await?
        .ok_or_else(|| "Cursor credential has not been authorized".to_string())?;
    let mut credentials: Credentials =
        serde_json::from_str(&secret).map_err(|e| format!("Invalid Cursor credential: {e}"))?;
    if now_ms().saturating_add(300_000) >= credentials.expires_at {
        credentials = refresh(&reqwest::Client::new(), REFRESH_URL, &credentials).await?;
        save(store, provider, credential, &credentials).await?;
    }
    Ok(credentials.access_token)
}

/// Persists a credential using the shared encrypted secret store.
pub(super) async fn save(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
    creds: &Credentials,
) -> Result<(), String> {
    let secret = serde_json::to_string(creds)
        .map_err(|e| format!("Failed to serialize Cursor credentials: {e}"))?;
    crate::provider::credentials::save_oauth(store, provider, credential, "cursor", &secret).await
}

/// Cursor's refresh response.
#[derive(Deserialize)]
struct RefreshResponse {
    /// Replacement access token.
    access_token: Option<String>,
    /// Replacement refresh token when rotated.
    refresh_token: Option<String>,
    /// Indicates that Cursor ended the session.
    #[serde(rename = "shouldLogout")]
    should_logout: Option<bool>,
}

/// Exchanges a refresh token without exposing it in errors.
async fn refresh(
    client: &reqwest::Client,
    url: &str,
    credentials: &Credentials,
) -> Result<Credentials, String> {
    let response = client.post(url).json(&json!({"grant_type":"refresh_token", "client_id":CLIENT_ID, "refresh_token":credentials.refresh_token}))
        .timeout(std::time::Duration::from_secs(30)).send().await.map_err(|e| format!("Cursor refresh failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Cursor refresh failed: {}", response.status()));
    }
    let body: RefreshResponse = response
        .json()
        .await
        .map_err(|e| format!("Invalid Cursor refresh response: {e}"))?;
    if body.should_logout == Some(true) {
        return Err("Cursor session ended; authorize again".into());
    }
    let access = body
        .access_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Cursor refresh returned no access token".to_string())?;
    let refresh = body
        .refresh_token
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| credentials.refresh_token.clone());
    Ok(Credentials::new(access, refresh))
}

/// OAuth refresh tests.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};

    /// Retains a refresh token when Cursor does not rotate it.
    #[tokio::test]
    async fn refresh_preserves_unrotated_token() {
        let router = Router::new().route(
            "/",
            post(|Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["grant_type"], "refresh_token");
                assert_eq!(body["refresh_token"], "old-refresh");
                Json(json!({"access_token":"new-access"}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let original = Credentials::new("old-access".into(), "old-refresh".into());
        let refreshed = refresh(&reqwest::Client::new(), &url, &original)
            .await
            .unwrap();
        assert_eq!(refreshed.access_token, "new-access");
        assert_eq!(refreshed.refresh_token, "old-refresh");
        server.abort();
    }
}

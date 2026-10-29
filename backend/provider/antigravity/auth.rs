use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use tokio::sync::RwLock;
use tracing::info;

use crate::provider::login::CredentialError;
use crate::server::settings::SettingsStore;

pub(crate) async fn oauth_client_id(store: &SettingsStore) -> Result<String, String> {
    store
        .setting_get("oauth_client_id")
        .await?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Google OAuth client ID is not configured".to_string())
}

pub(crate) async fn oauth_client_secret(store: &SettingsStore) -> Result<String, String> {
    store
        .secret_get("oauth_client_secret")
        .await?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Google OAuth client secret is not configured".to_string())
}

pub(crate) const OAUTH_CALLBACK_PORT: u16 = 51123;
pub(crate) const OAUTH_CALLBACK_PATH: &str = "/oauth-callback";
pub(crate) const OAUTH_CALLBACK_URL: &str = "http://127.0.0.1:51123/oauth-callback";
pub(crate) const OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub(crate) const OAUTH_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

pub(super) const CLOUD_CODE_ASSIST_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
pub(super) const ANTIGRAVITY_USER_AGENT: &str =
    "antigravity/hub/2.19.1 (aidev_client; os_type=linux; arch=x64; cl=963137146)";

#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Credentials {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) project_id: String,
    pub(crate) expires_at: Option<i64>, // Milliseconds since UNIX epoch
    pub(crate) email: Option<String>,
}

#[derive(Clone)]
pub(crate) struct AuthManager {
    creds: Arc<RwLock<Credentials>>,
    store: SettingsStore,
    provider_id: String,
    credential_id: String,
}

#[derive(Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    expires_in: i64,
}

impl AuthManager {
    pub(crate) async fn load(
        store: SettingsStore,
        provider_id: &str,
        credential_id: &str,
    ) -> Result<Option<Self>, String> {
        let lock = crate::provider::login::credential_lock(credential_id).await;
        let _guard = lock.lock().await;
        crate::provider::login::require_credential(&store, provider_id, credential_id, "agy")
            .await
            .map_err(|(_, message)| message)?;
        let creds = match store
            .secret_get(&format!("credential.{credential_id}.credential"))
            .await?
        {
            Some(json) => Some(
                serde_json::from_str(&json)
                    .map_err(|e| format!("Failed to parse stored Antigravity credentials: {e}"))?,
            ),
            None => None,
        };
        Ok(creds.map(|creds| Self {
            creds: Arc::new(RwLock::new(creds)),
            store,
            provider_id: provider_id.to_string(),
            credential_id: credential_id.to_string(),
        }))
    }

    pub(crate) async fn get_credentials(&self) -> Result<Credentials, CredentialError> {
        let lock = crate::provider::login::credential_lock(&self.credential_id).await;
        let _guard = lock.lock().await;
        crate::provider::login::require_credential(
            &self.store,
            &self.provider_id,
            &self.credential_id,
            "agy",
        )
        .await
        .map_err(|(_, message)| message)?;
        let stored = self
            .store
            .secret_get(&format!("credential.{}.credential", self.credential_id))
            .await?
            .ok_or_else(|| "Antigravity credentials no longer exist in the store".to_string())?;
        let current: Credentials = serde_json::from_str(&stored)
            .map_err(|e| format!("Failed to parse stored Antigravity credentials: {e}"))?;
        *self.creds.write().await = current;
        let needs_refresh = {
            let r = self.creds.read().await;
            r.expires_at.is_some_and(|exp| {
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
                // Refresh if within 60 seconds of expiration.
                now_ms.saturating_add(60_000) >= exp
            })
        };

        if needs_refresh {
            self.refresh().await?;
        }

        let r = self.creds.read().await;
        Ok(r.clone())
    }

    async fn refresh(&self) -> Result<(), CredentialError> {
        let current = self.creds.read().await.clone();
        let refresh_token = current.refresh_token.clone();

        let refresh_token = match refresh_token {
            Some(t) if !t.is_empty() => t,
            _ => return Ok(()), // No refresh token available
        };

        info!("Refreshing Google Antigravity access token...");

        let client = reqwest::Client::new();
        let client_id = oauth_client_id(&self.store).await?;
        let client_secret = oauth_client_secret(&self.store).await?;
        let params = [
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", &refresh_token),
            ("grant_type", "refresh_token"),
        ];

        let resp = client
            .post(OAUTH_TOKEN_URL)
            .form(&params)
            .send()
            .await
            .map_err(|e| format!("Failed to send token refresh request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CredentialError::upstream(
                status,
                format!("Token refresh failed ({status}): {body}"),
            ));
        }

        let token_resp: TokenRefreshResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse token response: {e}"))?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let new_expires_at = token_resp
            .expires_in
            .checked_mul(1000)
            .and_then(|duration| now_ms.checked_add(duration))
            .ok_or_else(|| "Token expiration is out of range".to_string())?;

        let mut updated = current;
        updated.access_token = token_resp.access_token;
        updated.expires_at = Some(new_expires_at);
        Self::save(
            &self.store,
            &self.provider_id,
            &self.credential_id,
            &updated,
        )
        .await?;
        *self.creds.write().await = updated;
        info!("Access token refreshed successfully.");
        Ok(())
    }

    pub(crate) async fn save(
        store: &SettingsStore,
        provider_id: &str,
        credential_id: &str,
        creds: &Credentials,
    ) -> Result<(), String> {
        let json = serde_json::to_string(creds)
            .map_err(|e| format!("Failed to serialize Antigravity credentials: {e}"))?;
        crate::provider::credentials::save_oauth(store, provider_id, credential_id, "agy", &json)
            .await
    }
}

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use tokio::sync::RwLock;
use tracing::info;

use crate::provider::login::CredentialError;
use crate::server::settings::SettingsStore;

/// Public Google OAuth application credentials embedded at build time.
pub(crate) const OAUTH_CLIENT_ID: &str = env!("AGY_OAUTH_CLIENT_ID");
/// Public Google OAuth application secret embedded at build time.
pub(crate) const OAUTH_CLIENT_SECRET: &str = env!("AGY_OAUTH_CLIENT_SECRET");

/// Port used by the local Google OAuth callback listener.
pub(crate) const OAUTH_CALLBACK_PORT: u16 = 51123;
/// Path served by the local OAuth callback listener.
pub(crate) const OAUTH_CALLBACK_PATH: &str = "/oauth-callback";
/// Redirect URI registered for the local OAuth callback.
pub(crate) const OAUTH_CALLBACK_URL: &str = "http://127.0.0.1:51123/oauth-callback";
/// Google endpoint for authorization-code exchanges and token refreshes.
pub(crate) const OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Google OAuth scopes requested for Antigravity access.
pub(crate) const OAUTH_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

/// Cloud Code Assist API endpoint used by Antigravity.
pub(super) const CLOUD_CODE_ASSIST_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
/// User agent sent to Cloud Code Assist.
pub(super) const ANTIGRAVITY_USER_AGENT: &str =
    "antigravity/hub/2.19.1 (aidev_client; os_type=linux; arch=x64; cl=963137146)";

/// Stored Google credentials and associated Cloud Code Assist project.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Credentials {
    /// Google OAuth access token.
    pub(crate) access_token: String,
    /// Optional token used to refresh access.
    pub(crate) refresh_token: Option<String>,
    /// Cloud Code Assist project identifier.
    pub(crate) project_id: String,
    /// Access-token expiration in milliseconds since the Unix epoch.
    pub(crate) expires_at: Option<i64>, // Milliseconds since UNIX epoch
    /// Email address associated with the Google account, when available.
    pub(crate) email: Option<String>,
}

/// Loads, refreshes, and persists credentials for one provider credential.
#[derive(Clone)]
pub(crate) struct AuthManager {
    /// Current credentials shared by manager clones.
    creds: Arc<RwLock<Credentials>>,
    /// Store containing account credentials.
    store: SettingsStore,
    /// Provider that owns the credential.
    provider_id: String,
    /// Identifier of the credential in the settings store.
    credential_id: String,
}

/// Fields returned by Google's token refresh endpoint.
#[derive(Deserialize)]
struct TokenRefreshResponse {
    /// Refreshed access token.
    access_token: String,
    /// Access-token lifetime in seconds.
    expires_in: i64,
}

impl AuthManager {
    /// Loads stored credentials after validating their provider association.
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

    /// Reloads credentials and refreshes an access token nearing expiration.
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

    /// Refreshes and saves the access token when a refresh token is available.
    async fn refresh(&self) -> Result<(), CredentialError> {
        self.refresh_at(OAUTH_TOKEN_URL).await
    }

    /// Refreshes using the supplied token endpoint.
    pub(super) async fn refresh_at(&self, token_url: &str) -> Result<(), CredentialError> {
        let current = self.creds.read().await.clone();
        let refresh_token = current.refresh_token.clone();

        let refresh_token = match refresh_token {
            Some(t) if !t.is_empty() => t,
            _ => return Ok(()), // No refresh token available
        };

        info!("Refreshing Google Antigravity access token...");

        let client = reqwest::Client::new();
        let params = [
            ("client_id", OAUTH_CLIENT_ID),
            ("client_secret", OAUTH_CLIENT_SECRET),
            ("refresh_token", &refresh_token),
            ("grant_type", "refresh_token"),
        ];

        let resp = client
            .post(token_url)
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

    /// Serializes and stores credentials for the specified provider credential.
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

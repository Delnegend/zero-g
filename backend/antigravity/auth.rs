use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use tokio::sync::RwLock;
use tracing::info;

use crate::server::settings::SettingsStore;
use crate::util::config_base_dir;

pub(crate) async fn migrate_oauth_settings(store: &SettingsStore) -> Result<(), String> {
    if store
        .setting_get("oauth_client_id.imported")
        .await?
        .is_none()
    {
        if store.setting_get("oauth_client_id").await?.is_none()
            && let Ok(value) = std::env::var("ZERO_G_OAUTH_CLIENT_ID")
            && !value.trim().is_empty()
        {
            store.setting_put("oauth_client_id", &value).await?;
        }
        store.setting_put("oauth_client_id.imported", "1").await?;
    }
    if store
        .setting_get("oauth_client_secret.imported")
        .await?
        .is_none()
    {
        if store.secret_get("oauth_client_secret").await?.is_none()
            && let Ok(value) = std::env::var("ZERO_G_OAUTH_CLIENT_SECRET")
            && !value.trim().is_empty()
        {
            store.secret_put("oauth_client_secret", &value).await?;
        }
        store
            .setting_put("oauth_client_secret.imported", "1")
            .await?;
    }
    Ok(())
}

pub(crate) async fn oauth_client_id(store: &SettingsStore) -> Result<String, String> {
    migrate_oauth_settings(store).await?;
    store
        .setting_get("oauth_client_id")
        .await?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Google OAuth client ID is not configured".to_string())
}

pub(crate) async fn oauth_client_secret(store: &SettingsStore) -> Result<String, String> {
    migrate_oauth_settings(store).await?;
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

pub(crate) const CLOUD_CODE_ASSIST_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
pub(crate) const ANTIGRAVITY_USER_AGENT: &str =
    "antigravity/hub/2.19.1 (aidev_client; os_type=linux; arch=x64; cl=963137146)";

#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Credentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub project_id: String,
    pub expires_at: Option<i64>, // Milliseconds since UNIX epoch
    pub email: Option<String>,
}

#[derive(Clone)]
pub(crate) struct AuthManager {
    creds: Arc<RwLock<Credentials>>,
    store: SettingsStore,
}

#[derive(Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    expires_in: i64,
}

impl AuthManager {
    pub(crate) async fn load(store: SettingsStore) -> Result<Option<Self>, String> {
        let creds = match store.secret_get("provider.antigravity").await? {
            Some(json) => Some(
                serde_json::from_str(&json)
                    .map_err(|e| format!("Failed to parse stored Antigravity credentials: {e}"))?,
            ),
            None if store
                .secret_get("provider.antigravity.imported")
                .await?
                .is_none() =>
            {
                let path = Self::get_config_path();
                match fs::read_to_string(&path) {
                    Ok(content) => {
                        let creds = serde_json::from_str(&content).map_err(|e| {
                            format!("Failed to parse legacy credentials {}: {e}", path.display())
                        })?;
                        Self::save(&store, &creds).await?;
                        info!("Imported Antigravity credentials from {}", path.display());
                        Some(creds)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        store
                            .secret_put("provider.antigravity.imported", "1")
                            .await?;
                        None
                    }
                    Err(e) => {
                        return Err(format!(
                            "Failed to read legacy credentials {}: {e}",
                            path.display()
                        ));
                    }
                }
            }
            None => None,
        };
        Ok(creds.map(|creds| Self {
            creds: Arc::new(RwLock::new(creds)),
            store,
        }))
    }

    fn get_config_path() -> PathBuf {
        if let Ok(custom) =
            std::env::var("ZERO_G_CREDENTIALS_AGY").or_else(|_| std::env::var("ZERO_G_CREDENTIALS"))
            && !custom.trim().is_empty()
        {
            return PathBuf::from(custom);
        }
        if Path::new("credentials-agy.json").exists() {
            return PathBuf::from("credentials-agy.json");
        }
        if let Some(base) = config_base_dir() {
            return base.join("zero-g/credentials-agy.json");
        }
        PathBuf::from("credentials-agy.json")
    }

    pub(crate) async fn get_credentials(&self) -> Result<Credentials, String> {
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

    pub(crate) async fn refresh(&self) -> Result<(), String> {
        let stored = self
            .store
            .secret_get("provider.antigravity")
            .await?
            .ok_or_else(|| "Antigravity credentials no longer exist in the store".to_string())?;
        let current: Credentials = serde_json::from_str(&stored)
            .map_err(|e| format!("Failed to parse stored Antigravity credentials: {e}"))?;
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
            return Err(format!("Token refresh failed ({status}): {body}"));
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
        Self::save(&self.store, &updated).await?;
        *self.creds.write().await = updated;
        info!("Access token refreshed successfully.");
        Ok(())
    }

    pub(crate) async fn save(store: &SettingsStore, creds: &Credentials) -> Result<(), String> {
        store.secret_get("provider.antigravity").await?;
        let json = serde_json::to_string(creds)
            .map_err(|e| format!("Failed to serialize Antigravity credentials: {e}"))?;
        store.secret_put("provider.antigravity", &json).await?;
        store.secret_put("provider.antigravity.imported", "1").await
    }
}

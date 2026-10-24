use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::info;

pub(crate) fn oauth_client_id() -> Result<String, String> {
    std::env::var("OAUTH_CLIENT_ID")
        .map_err(|_| "OAUTH_CLIENT_ID not set. Please set it in .env or environment.".to_string())
}

pub(crate) fn oauth_client_secret() -> Result<String, String> {
    std::env::var("OAUTH_CLIENT_SECRET").map_err(|_| {
        "OAUTH_CLIENT_SECRET not set. Please set it in .env or environment.".to_string()
    })
}
pub(crate) const OAUTH_CALLBACK_PORT: u16 = 51121;
pub(crate) const OAUTH_CALLBACK_PATH: &str = "/oauth-callback";
pub(crate) const OAUTH_CALLBACK_URL: &str = "http://127.0.0.1:51121/oauth-callback";
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Credentials {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>, // Milliseconds since UNIX epoch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Clone)]
pub(crate) struct AuthManager {
    creds: Arc<RwLock<Credentials>>,
    storage_path: Option<PathBuf>,
}

#[derive(Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    expires_in: i64,
}

impl AuthManager {
    pub(crate) fn load() -> Result<Self, String> {
        // 1. Try local file credentials.json or ~/.config/zero-g/credentials.json
        if let Some(creds) = Self::load_from_file() {
            let storage_path = Self::get_config_path();
            info!("Loaded credentials from file: {:?}", storage_path);
            return Ok(Self {
                creds: Arc::new(RwLock::new(creds)),
                storage_path: Some(storage_path),
            });
        }

        // 2. Try environment variables
        if let Some(creds) = Self::load_from_env() {
            info!("Loaded credentials from environment variables");
            return Ok(Self {
                creds: Arc::new(RwLock::new(creds)),
                storage_path: None,
            });
        }

        Err(
            "No Google Antigravity credentials found. Run `zero-g login` to authenticate."
                .to_string(),
        )
    }
    fn get_config_path() -> PathBuf {
        if Path::new("credentials.json").exists() {
            return PathBuf::from("credentials.json");
        }
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(&home).join(".config/zero-g/credentials.json");
            if p.exists() {
                return p;
            }
            let gap_p = PathBuf::from(&home).join(".config/gap/credentials.json");
            if gap_p.exists() {
                return gap_p;
            }
            return p;
        }
        PathBuf::from("credentials.json")
    }

    fn load_from_file() -> Option<Credentials> {
        let paths = [
            PathBuf::from("credentials.json"),
            std::env::var("HOME")
                .map(|h| PathBuf::from(&h).join(".config/zero-g/credentials.json"))
                .unwrap_or_default(),
            std::env::var("HOME")
                .map(|h| PathBuf::from(&h).join(".config/gap/credentials.json"))
                .unwrap_or_default(),
        ];

        for path in &paths {
            if path.exists()
                && let Ok(content) = fs::read_to_string(path)
                && let Ok(creds) = serde_json::from_str::<Credentials>(&content)
            {
                return Some(creds);
            }
        }
        None
    }

    fn load_from_env() -> Option<Credentials> {
        let access_token = std::env::var("ANTIGRAVITY_ACCESS_TOKEN").ok()?;
        let project_id = std::env::var("ANTIGRAVITY_PROJECT_ID").ok()?;
        let refresh_token = std::env::var("ANTIGRAVITY_REFRESH_TOKEN").ok();
        Some(Credentials {
            access_token,
            refresh_token,
            project_id,
            expires_at: None,
            email: None,
        })
    }

    pub(crate) async fn get_credentials(&self) -> Result<Credentials, String> {
        let needs_refresh = {
            let r = self.creds.read().await;
            if let Some(exp) = r.expires_at {
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64);
                // Refresh if within 60 seconds of expiration
                now_ms + 60_000 >= exp
            } else {
                false
            }
        };

        if needs_refresh {
            self.refresh().await?;
        }

        let r = self.creds.read().await;
        Ok(r.clone())
    }

    pub(crate) async fn refresh(&self) -> Result<(), String> {
        let refresh_token = {
            let r = self.creds.read().await;
            r.refresh_token.clone()
        };

        let refresh_token = match refresh_token {
            Some(t) if !t.is_empty() => t,
            _ => return Ok(()), // No refresh token available
        };

        info!("Refreshing Google Antigravity access token...");

        let client = reqwest::Client::new();
        let client_id = oauth_client_id()?;
        let client_secret = oauth_client_secret()?;
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
            .map_or(0, |d| d.as_millis() as i64);
        let new_expires_at = now_ms + token_resp.expires_in * 1000;

        {
            let mut w = self.creds.write().await;
            w.access_token = token_resp.access_token;
            w.expires_at = Some(new_expires_at);
        }

        // Persist update
        self.save_updated_credentials().await;
        info!("Access token refreshed successfully.");
        Ok(())
    }

    async fn save_updated_credentials(&self) {
        let creds = self.creds.read().await.clone();

        if let Some(storage_path) = &self.storage_path
            && let Ok(json) = serde_json::to_string_pretty(&creds)
        {
            if let Some(parent) = storage_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(storage_path, json);
        }
    }

    pub(crate) fn save_to_file(creds: &Credentials) -> Result<PathBuf, String> {
        let path = Self::get_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir {}: {e}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(creds)
            .map_err(|e| format!("Failed to serialize credentials: {e}"))?;
        fs::write(&path, json)
            .map_err(|e| format!("Failed to write credentials to {}: {e}", path.display()))?;
        Ok(path)
    }
}

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::util::{config_base_dir, restrict_file_permissions};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::info;

pub(crate) const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(crate) const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub(crate) const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(crate) const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
pub(crate) const CODEX_CLIENT_VERSION: &str = "0.159.0";
pub(crate) const CODEX_CALLBACK_PORT: u16 = 1455;
pub(crate) const CODEX_CALLBACK_PATH: &str = "/auth/callback";
pub(crate) const CODEX_CALLBACK_URL: &str = "http://localhost:1455/auth/callback";
pub(crate) const CODEX_DEVICE_USERCODE_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub(crate) const CODEX_DEVICE_TOKEN_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/token";
pub(crate) const CODEX_DEVICE_AUTH_URL: &str = "https://auth.openai.com/codex/device";
pub(crate) const CODEX_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
pub(crate) const CODEX_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

const JWT_CLAIM_AUTH: &str = "https://api.openai.com/auth";
const JWT_CLAIM_PROFILE: &str = "https://api.openai.com/profile";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CodexCredentials {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub residency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>, // Milliseconds since UNIX epoch
}

#[derive(Clone)]
pub(crate) struct CodexAuthManager {
    creds: Arc<RwLock<CodexCredentials>>,
    storage_path: PathBuf,
}

#[derive(Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: i64,
}

pub(crate) struct TokenProfile {
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub residency: Option<String>,
}

pub(crate) fn decode_jwt_profile(access_token: &str, id_token: Option<&str>) -> TokenProfile {
    let access_payload = decode_jwt_payload(access_token);
    let id_payload = id_token.and_then(decode_jwt_payload);

    let get_claim =
        |payload: &Option<serde_json::Value>, namespace: &str, key: &str| -> Option<String> {
            let p = payload.as_ref()?;
            let ns = p.get(namespace)?;
            let val = ns.get(key)?;
            val.as_str()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };

    let account_id = get_claim(&access_payload, JWT_CLAIM_AUTH, "chatgpt_account_id")
        .or_else(|| get_claim(&id_payload, JWT_CLAIM_AUTH, "chatgpt_account_id"));

    let email = get_claim(&access_payload, JWT_CLAIM_PROFILE, "email")
        .or_else(|| get_claim(&id_payload, JWT_CLAIM_PROFILE, "email"))
        .map(|e| e.to_lowercase());

    let plan_type = get_claim(&access_payload, JWT_CLAIM_AUTH, "chatgpt_plan_type")
        .or_else(|| get_claim(&id_payload, JWT_CLAIM_AUTH, "chatgpt_plan_type"))
        .map(|p| p.to_lowercase());

    let residency = get_claim(&access_payload, JWT_CLAIM_AUTH, "chatgpt_data_residency")
        .or_else(|| get_claim(&access_payload, JWT_CLAIM_AUTH, "chatgpt_compute_residency"))
        .filter(|r| r.to_lowercase() != "no_constraint");

    TokenProfile {
        account_id,
        email,
        plan_type,
        residency,
    }
}

fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let unpadded = parts[1];
    let decoded = URL_SAFE_NO_PAD
        .decode(unpadded)
        .or_else(|_| {
            let pad = match unpadded.len() % 4 {
                2 => "==",
                3 => "=",
                _ => "",
            };
            URL_SAFE_NO_PAD.decode(format!("{unpadded}{pad}"))
        })
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

impl CodexAuthManager {
    pub(crate) fn load() -> Result<Self, String> {
        let storage_path = Self::get_config_path();
        if let Some(creds) = Self::load_from_file(&storage_path) {
            info!("Loaded Codex credentials from file: {:?}", storage_path);
            return Ok(Self {
                creds: Arc::new(RwLock::new(creds)),
                storage_path,
            });
        }

        // Also check OPENAI_CODEX_OAUTH_TOKEN env var
        if let Ok(token) = std::env::var("OPENAI_CODEX_OAUTH_TOKEN")
            .or_else(|_| std::env::var("ZERO_G_CODEX_TOKEN"))
            && !token.trim().is_empty()
        {
            let profile = decode_jwt_profile(&token, None);
            let creds = CodexCredentials {
                access_token: token,
                refresh_token: None,
                account_id: profile.account_id,
                email: profile.email,
                plan_type: profile.plan_type,
                residency: profile.residency,
                expires_at: None,
            };
            return Ok(Self {
                creds: Arc::new(RwLock::new(creds)),
                storage_path,
            });
        }

        Err(
            "No OpenAI Codex credentials found. Run `zero-g login codex` to authenticate."
                .to_string(),
        )
    }

    pub(crate) fn get_config_path() -> PathBuf {
        if let Ok(custom) = std::env::var("ZERO_G_CREDENTIALS_CODEX")
            && !custom.trim().is_empty()
        {
            return PathBuf::from(custom);
        }
        if Path::new("credentials-codex.json").exists() {
            return PathBuf::from("credentials-codex.json");
        }
        if let Some(base) = config_base_dir() {
            return base.join("zero-g/credentials-codex.json");
        }
        PathBuf::from("credentials-codex.json")
    }

    fn load_from_file(path: &Path) -> Option<CodexCredentials> {
        if path.exists()
            && let Ok(content) = fs::read_to_string(path)
            && let Ok(creds) = serde_json::from_str::<CodexCredentials>(&content)
        {
            return Some(creds);
        }
        None
    }

    pub(crate) async fn get_credentials(&self) -> Result<CodexCredentials, String> {
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

        let Some(rt) = refresh_token else {
            return Err("Cannot refresh Codex token: no refresh_token present".to_string());
        };

        info!("Refreshing OpenAI Codex access token...");
        let client = reqwest::Client::new();
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", rt.as_str()),
            ("client_id", CODEX_CLIENT_ID),
        ];

        let resp = client
            .post(CODEX_TOKEN_URL)
            .form(&params)
            .send()
            .await
            .map_err(|e| format!("Failed to refresh Codex token: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Codex token refresh returned {status}: {body}"));
        }

        let token_data = resp
            .json::<TokenRefreshResponse>()
            .await
            .map_err(|e| format!("Failed to parse Codex token refresh response: {e}"))?;

        let profile = decode_jwt_profile(&token_data.access_token, token_data.id_token.as_deref());

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let new_expires_at = now_ms + token_data.expires_in * 1000;

        {
            let mut w = self.creds.write().await;
            w.access_token = token_data.access_token;
            if let Some(new_rt) = token_data.refresh_token {
                w.refresh_token = Some(new_rt);
            }
            if profile.account_id.is_some() {
                w.account_id = profile.account_id;
            }
            if profile.email.is_some() {
                w.email = profile.email;
            }
            if profile.plan_type.is_some() {
                w.plan_type = profile.plan_type;
            }
            if profile.residency.is_some() {
                w.residency = profile.residency;
            }
            w.expires_at = Some(new_expires_at);
        }

        self.save_updated_credentials().await;
        info!("OpenAI Codex access token refreshed successfully.");
        Ok(())
    }

    async fn save_updated_credentials(&self) {
        let creds = self.creds.read().await.clone();
        if let Ok(json) = serde_json::to_string_pretty(&creds) {
            if let Some(parent) = self.storage_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::write(&self.storage_path, &json).is_ok() {
                restrict_file_permissions(&self.storage_path);
            }
        }
    }

    pub(crate) fn save_to_file(creds: &CodexCredentials) -> Result<PathBuf, String> {
        let path = Self::get_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir {}: {e}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(creds)
            .map_err(|e| format!("Failed to serialize Codex credentials: {e}"))?;
        fs::write(&path, &json).map_err(|e| {
            format!(
                "Failed to write Codex credentials to {}: {e}",
                path.display()
            )
        })?;
        restrict_file_permissions(&path);
        Ok(path)
    }
}


#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_jwt_profile() {
        // Header: {"alg":"none"} -> eyJhbGciOiJub25lIn0
        // Payload:
        // {
        //   "https://api.openai.com/auth": {
        //     "chatgpt_account_id": "acc_12345",
        //     "chatgpt_plan_type": "pro",
        //     "chatgpt_data_residency": "us"
        //   },
        //   "https://api.openai.com/profile": {
        //     "email": "USER@example.com"
        //   }
        // }
        let claims_json = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc_12345",
                "chatgpt_plan_type": "pro",
                "chatgpt_data_residency": "us"
            },
            "https://api.openai.com/profile": {
                "email": "USER@example.com"
            }
        });
        let claims_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims_json).unwrap());
        let fake_jwt = format!("eyJhbGciOiJub25lIn0.{claims_b64}.sig");

        let profile = decode_jwt_profile(&fake_jwt, None);
        assert_eq!(profile.account_id.as_deref(), Some("acc_12345"));
        assert_eq!(profile.plan_type.as_deref(), Some("pro"));
        assert_eq!(profile.email.as_deref(), Some("user@example.com"));
        assert_eq!(profile.residency.as_deref(), Some("us"));
    }

    #[test]
    fn test_decode_jwt_residency_no_constraint_ignored() {
        let claims_json = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc_999",
                "chatgpt_data_residency": "no_constraint"
            }
        });
        let claims_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims_json).unwrap());
        let fake_jwt = format!("eyJhbGciOiJub25lIn0.{claims_b64}.sig");

        let profile = decode_jwt_profile(&fake_jwt, None);
        assert_eq!(profile.account_id.as_deref(), Some("acc_999"));
        assert_eq!(profile.residency, None);
    }
}

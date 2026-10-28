use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::server::settings::SettingsStore;
use crate::util::config_base_dir;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use tokio::sync::RwLock;
use tracing::info;

pub(crate) const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(super) const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(super) const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
pub(super) const CODEX_CLIENT_VERSION: &str = "0.159.0";
pub(crate) const CODEX_DEVICE_USERCODE_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub(crate) const CODEX_DEVICE_TOKEN_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/token";
pub(crate) const CODEX_DEVICE_AUTH_URL: &str = "https://auth.openai.com/codex/device";
pub(crate) const CODEX_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

const JWT_CLAIM_AUTH: &str = "https://api.openai.com/auth";
const JWT_CLAIM_PROFILE: &str = "https://api.openai.com/profile";

#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CodexCredentials {
    pub(super) access_token: String,
    pub(super) refresh_token: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) plan_type: Option<String>,
    pub(super) residency: Option<String>,
    pub(super) expires_at: Option<i64>, // Milliseconds since UNIX epoch
}

#[derive(Clone)]
pub(crate) struct CodexAuthManager {
    creds: Arc<RwLock<CodexCredentials>>,
    store: SettingsStore,
}

#[derive(Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: i64,
}

pub(super) struct TokenProfile {
    pub(super) account_id: Option<String>,
    pub(super) email: Option<String>,
    pub(super) plan_type: Option<String>,
    pub(super) residency: Option<String>,
}

pub(super) fn decode_jwt_profile(access_token: &str, id_token: Option<&str>) -> TokenProfile {
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
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let unpadded = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
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
    pub(crate) async fn load(store: SettingsStore) -> Result<Option<Self>, String> {
        let creds = match store.secret_get("provider.codex").await? {
            Some(json) => Some(
                serde_json::from_str(&json)
                    .map_err(|e| format!("Failed to parse stored Codex credentials: {e}"))?,
            ),
            None if store.secret_get("provider.codex.imported").await?.is_none() => {
                let path = Self::get_config_path();
                match fs::read_to_string(&path) {
                    Ok(content) => {
                        let creds = serde_json::from_str(&content).map_err(|e| {
                            format!(
                                "Failed to parse legacy Codex credentials {}: {e}",
                                path.display()
                            )
                        })?;
                        Self::save(&store, &creds).await?;
                        info!("Imported Codex credentials from {}", path.display());
                        Some(creds)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        store.secret_put("provider.codex.imported", "1").await?;
                        None
                    }
                    Err(e) => {
                        return Err(format!(
                            "Failed to read legacy Codex credentials {}: {e}",
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

    pub(crate) async fn get_credentials(&self) -> Result<CodexCredentials, String> {
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

    async fn refresh(&self) -> Result<(), String> {
        let stored = self
            .store
            .secret_get("provider.codex")
            .await?
            .ok_or_else(|| "Codex credentials no longer exist in the store".to_string())?;
        let mut updated: CodexCredentials = serde_json::from_str(&stored)
            .map_err(|e| format!("Failed to parse stored Codex credentials: {e}"))?;
        let refresh_token = updated.refresh_token.clone();

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
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let new_expires_at = token_data
            .expires_in
            .checked_mul(1000)
            .and_then(|duration| now_ms.checked_add(duration))
            .ok_or_else(|| "Token expiration is out of range".to_string())?;

        updated.access_token = token_data.access_token;
        if let Some(new_rt) = token_data.refresh_token {
            updated.refresh_token = Some(new_rt);
        }
        if profile.account_id.is_some() {
            updated.account_id = profile.account_id;
        }
        if profile.email.is_some() {
            updated.email = profile.email;
        }
        if profile.plan_type.is_some() {
            updated.plan_type = profile.plan_type;
        }
        if profile.residency.is_some() {
            updated.residency = profile.residency;
        }
        updated.expires_at = Some(new_expires_at);
        Self::save(&self.store, &updated).await?;
        *self.creds.write().await = updated;
        info!("OpenAI Codex access token refreshed successfully.");
        Ok(())
    }

    pub(crate) async fn save(
        store: &SettingsStore,
        creds: &CodexCredentials,
    ) -> Result<(), String> {
        store.secret_get("provider.codex").await?;
        let json = serde_json::to_string(creds)
            .map_err(|e| format!("Failed to serialize Codex credentials: {e}"))?;
        store.secret_put("provider.codex", &json).await?;
        store.secret_put("provider.codex.imported", "1").await
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

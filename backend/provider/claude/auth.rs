use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::provider::login::{credential_lock, require_credential};
use crate::server::settings::SettingsStore;

/// Public OAuth client used by Claude Code.
pub(super) const CLIENT_ID: &str = env!("CLAUDE_OAUTH_CLIENT_ID");
/// Token exchange and refresh endpoint.
pub(super) const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
/// OAuth callback registered for the Claude Code client.
pub(super) const REDIRECT_URI: &str = "http://localhost:54545/callback";

/// Stored credentials, encrypted by the settings store.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Credentials {
    /// OAuth access token.
    pub(super) access_token: String,
    /// Refresh token rotated by Anthropic.
    pub(super) refresh_token: String,
    /// Expiration as Unix milliseconds.
    pub(super) expires_at: i64,
    /// Account identifier.
    pub(super) account_id: Option<String>,
    /// Account email address.
    pub(super) email: Option<String>,
    /// Subscription organization identifier.
    pub(super) organization_id: Option<String>,
    /// Subscription organization name.
    pub(super) organization_name: Option<String>,
}

/// OAuth token exchange response.
#[derive(Deserialize)]
pub(super) struct TokenResponse {
    /// Newly issued access token.
    access_token: String,
    /// Replacement refresh token.
    refresh_token: Option<String>,
    /// Access token lifetime in seconds.
    expires_in: i64,
    /// Optional account identity.
    account: Option<Account>,
    /// Optional organization identity.
    organization: Option<Organization>,
}

/// Account identity returned by OAuth.
#[derive(Deserialize)]
struct Account {
    /// Account UUID.
    uuid: Option<String>,
    /// Account email address.
    email_address: Option<String>,
}
/// Organization identity returned by OAuth.
#[derive(Deserialize)]
struct Organization {
    /// Organization UUID.
    uuid: Option<String>,
    /// Organization display name.
    name: Option<String>,
}

impl TokenResponse {
    /// Merge an exchange response with existing credential metadata.
    pub(super) fn credentials(self, previous: Option<&Credentials>) -> Result<Credentials, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?;
        let now = i64::try_from(now.as_millis()).map_err(|e| e.to_string())?;
        let expires_at = self
            .expires_in
            .checked_mul(1000)
            .and_then(|v| now.checked_add(v))
            .ok_or("Invalid token expiration")?;
        let refresh_token = self
            .refresh_token
            .or_else(|| previous.as_ref().map(|p| p.refresh_token.clone()))
            .ok_or("Missing refresh token")?;
        Ok(Credentials {
            access_token: self.access_token,
            refresh_token,
            expires_at,
            account_id: self
                .account
                .as_ref()
                .and_then(|a| a.uuid.clone())
                .or_else(|| previous.as_ref().and_then(|p| p.account_id.clone())),
            email: self
                .account
                .and_then(|a| a.email_address)
                .or_else(|| previous.as_ref().and_then(|p| p.email.clone())),
            organization_id: previous
                .as_ref()
                .and_then(|p| p.organization_id.clone())
                .or_else(|| self.organization.as_ref().and_then(|o| o.uuid.clone())),
            organization_name: previous
                .as_ref()
                .and_then(|p| p.organization_name.clone())
                .or_else(|| self.organization.and_then(|o| o.name)),
        })
    }
}

/// Exchange or refresh a token using the public JSON OAuth endpoint.
pub(super) async fn exchange(
    url: &str,
    body: serde_json::Value,
    refresh: bool,
) -> Result<TokenResponse, String> {
    let client = super::client::http()?;
    let mut request = client.post(url).json(&body);
    if refresh {
        request = request.header("anthropic-beta", "oauth-2025-04-20").header(
            "user-agent",
            "anthropic-sdk-typescript/0.71.2 userOAuthProvider",
        );
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("Claude token request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Claude token request failed: {}",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map_err(|e| format!("Invalid Claude token response: {e}"))
}

/// Load a credential and serialize refreshes across requests and login.
pub(super) async fn load(
    store: &SettingsStore,
    provider: &str,
    id: &str,
) -> Result<Credentials, String> {
    let lock = credential_lock(id).await;
    let _guard = lock.lock().await;
    require_credential(store, provider, id, "claude")
        .await
        .map_err(|(_, e)| e)?;
    let secret = store
        .secret_get(&format!("credential.{id}.credential"))
        .await?
        .ok_or("Claude credential is not connected")?;
    let creds: Credentials =
        serde_json::from_str(&secret).map_err(|e| format!("Invalid Claude credential: {e}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    let now = i64::try_from(now.as_millis()).map_err(|e| e.to_string())?;
    if now.saturating_add(300_000) < creds.expires_at {
        return Ok(creds);
    }
    let tokens = exchange(TOKEN_URL, json!({"grant_type":"refresh_token", "refresh_token":creds.refresh_token, "client_id":CLIENT_ID}), true).await?;
    let updated = tokens.credentials(Some(&creds))?;
    save(store, provider, id, &updated).await?;
    Ok(updated)
}

/// Save credentials through the shared encrypted OAuth store.
pub(super) async fn save(
    store: &SettingsStore,
    provider: &str,
    id: &str,
    creds: &Credentials,
) -> Result<(), String> {
    let json = serde_json::to_string(creds).map_err(|e| e.to_string())?;
    crate::provider::credentials::save_oauth(store, provider, id, "claude", &json).await
}

/// Credential exchange tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::HeaderMap, routing::post};
    use serde_json::Value;

    /// Refresh requests rotate tokens while preserving organization identity.
    #[tokio::test]
    async fn refresh_sends_json_and_rotates_token() {
        let app = Router::new().route("/token", post(|headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
            assert_eq!(headers.get("anthropic-beta").unwrap(), "oauth-2025-04-20");
            assert_eq!(body.get("grant_type").and_then(Value::as_str), Some("refresh_token"));
            assert_eq!(body.get("refresh_token").and_then(Value::as_str), Some("old"));
            assert_eq!(body.get("client_id").and_then(Value::as_str), Some(CLIENT_ID));
            Json(json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let previous = Credentials {
            access_token: "old-access".into(),
            refresh_token: "old".into(),
            expires_at: 0,
            account_id: Some("account".into()),
            email: None,
            organization_id: Some("org".into()),
            organization_name: None,
        };
        let response = exchange(
            &format!("http://{addr}/token"),
            json!({"grant_type":"refresh_token","refresh_token":"old","client_id":CLIENT_ID}),
            true,
        )
        .await
        .unwrap();
        let updated = response.credentials(Some(&previous)).unwrap();
        assert_eq!(updated.access_token, "new-access");
        assert_eq!(updated.refresh_token, "new-refresh");
        assert_eq!(updated.organization_id.as_deref(), Some("org"));
    }
}

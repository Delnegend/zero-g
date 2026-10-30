use crate::server::settings::SettingsStore;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Public xAI CLI OAuth client identifier.
pub(super) const CLIENT_ID: &str = env!("GROK_OAUTH_CLIENT_ID");
/// Device authorization endpoint.
pub(super) const DEVICE_URL: &str = "https://auth.x.ai/oauth2/device/code";
/// `OpenID` Connect endpoint discovery URL.
const DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";

/// Encrypted OAuth credential payload.
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Credentials {
    /// Access token used for inference.
    pub(super) access_token: String,
    /// Refresh token used to renew access.
    pub(super) refresh_token: String,
    /// Expiration as Unix seconds.
    pub(super) expires_at: u64,
}

/// Token response from the xAI OAuth server.
#[derive(Deserialize)]
pub(super) struct TokenResponse {
    /// New access token.
    access_token: String,
    /// Optional rotated refresh token.
    refresh_token: Option<String>,
    /// Access-token lifetime in seconds.
    expires_in: Option<u64>,
}
impl TokenResponse {
    /// Validates and converts an OAuth token response.
    pub(super) fn credentials(self, previous: Option<String>) -> Result<Credentials, String> {
        let refresh_token = self
            .refresh_token
            .or(previous)
            .filter(|s| !s.is_empty())
            .ok_or("Missing refresh token")?;
        if self.access_token.is_empty() {
            return Err("Missing access token".into());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid system clock")?
            .as_secs();
        let expires_at = now
            .checked_add(self.expires_in.unwrap_or(3600))
            .ok_or("Invalid token expiry")?;
        Ok(Credentials {
            access_token: self.access_token,
            refresh_token,
            expires_at,
        })
    }
}

/// Creates a timeout-bounded client that refuses redirects.
pub(super) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "Failed to initialize xAI client".into())
}

/// Discovers and validates the xAI OAuth token endpoint.
pub(super) async fn token_endpoint(client: &reqwest::Client) -> Result<String, String> {
    let value: serde_json::Value = client
        .get(DISCOVERY_URL)
        .send()
        .await
        .map_err(|_| "xAI discovery failed")?
        .error_for_status()
        .map_err(|_| "xAI discovery failed")?
        .json()
        .await
        .map_err(|_| "Invalid xAI discovery response")?;
    let endpoint = value
        .get("token_endpoint")
        .and_then(serde_json::Value::as_str)
        .ok_or("Missing xAI token endpoint")?;
    let url = reqwest::Url::parse(endpoint).map_err(|_| "Invalid xAI token endpoint")?;
    let host = url.host_str().unwrap_or("");
    if url.scheme() != "https"
        || !(host == "x.ai" || host.ends_with(".x.ai"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err("Invalid xAI token endpoint".into());
    }
    Ok(endpoint.to_owned())
}

/// Exchanges a refresh token for updated credentials.
async fn refresh(
    client: &reqwest::Client,
    endpoint: &str,
    previous: String,
) -> Result<Credentials, String> {
    let response = client
        .post(endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", previous.as_str()),
        ])
        .send()
        .await
        .map_err(|_| "xAI refresh failed")?;
    if !response.status().is_success() {
        return Err(format!("xAI refresh returned HTTP {}", response.status()));
    }
    let token: TokenResponse = response
        .json()
        .await
        .map_err(|_| "Invalid xAI refresh response")?;
    token.credentials(Some(previous))
}

/// Loads and refreshes the provider-bound credential when necessary.
pub(super) async fn access_token(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
    kind: &str,
) -> Result<String, String> {
    let lock = crate::provider::login::credential_lock(credential).await;
    let _guard = lock.lock().await;
    crate::provider::login::require_credential(store, provider, credential, kind)
        .await
        .map_err(|(_, e)| e)?;
    let stored = store
        .secret_get(&format!("credential.{credential}.credential"))
        .await?
        .ok_or("Credential is not connected")?;
    let mut creds: Credentials =
        serde_json::from_str(&stored).map_err(|_| "Invalid stored xAI credentials")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid system clock")?
        .as_secs();
    if now.saturating_add(60) >= creds.expires_at {
        let client = client()?;
        let endpoint = token_endpoint(&client).await?;
        creds = refresh(&client, &endpoint, creds.refresh_token).await?;
        let json =
            serde_json::to_string(&creds).map_err(|_| "Failed to serialize xAI credentials")?;
        crate::provider::credentials::save_oauth(store, provider, credential, kind, &json).await?;
    }
    Ok(creds.access_token)
}

/// OAuth token exchange behavior tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{Json, Router, body::Bytes, routing::post};
    use tokio::sync::mpsc;

    /// Refresh requests carry the grant and retain an unrotated refresh token.
    #[tokio::test]
    async fn refresh_sends_oauth_grant_and_preserves_unrotated_token() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let app = Router::new().route(
            "/",
            post(move |body: Bytes| {
                let tx = tx.clone();
                async move {
                    tx.send(String::from_utf8(body.to_vec()).unwrap()).unwrap();
                    Json(serde_json::json!({"access_token":"new-access","expires_in":3600}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let creds = refresh(
            &client().unwrap(),
            &format!("http://{addr}/"),
            "old-refresh".into(),
        )
        .await
        .unwrap();
        assert_eq!(creds.access_token, "new-access");
        assert_eq!(creds.refresh_token, "old-refresh");
        let body = rx.recv().await.unwrap();
        let params: std::collections::HashMap<_, _> =
            reqwest::Url::parse(&format!("http://localhost/?{body}"))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
        assert_eq!(
            params.get("grant_type").map(String::as_str),
            Some("refresh_token")
        );
        assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
        assert_eq!(
            params.get("refresh_token").map(String::as_str),
            Some("old-refresh")
        );
        server.abort();
    }
}

//! `CoDev` sign-in and gateway key provisioning.
//!
//! `CoDev` Code is an `OpenCode` fork whose login works in two steps:
//! 1. OAuth 2 authorization code + PKCE against the operator's SSO wrapper, which
//!    yields an SSO access token and a refresh token.
//! 2. The SSO access token is traded at the `CoDev` backend for a `LiteLLM`
//!    gateway API key, and the backend reports which gateway URL to use.
//!
//! Inference then goes to the gateway's OpenAI-compatible `/v1` API with that
//! key. The key can be revoked or rotated, so a rejected key is re-issued from
//! a fresh SSO token before the credential is given up on.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::skip_serializing_none;
use tracing::info;

use crate::provider::login::CredentialError;
use crate::server::settings::SettingsStore;

/// Public OAuth client identifier used by the `CoDev` Code CLI.
pub(super) const CLIENT_ID: &str = env!("CODEV_OAUTH_CLIENT_ID");
/// Environment variable naming the domain that operates `CoDev`.
pub(super) const DOMAIN_VAR: &str = "ZERO_G_CODEV_PROVIDER";
/// Browser authorization endpoint.
pub(super) const AUTHORIZE_PATH: &str = "/sso-wrapper/authorize";
/// Token exchange and refresh endpoint.
const TOKEN_PATH: &str = "/sso-wrapper/token";
/// `OpenID` user info endpoint.
const USERINFO_PATH: &str = "/sso-wrapper/userinfo";
/// Hosted page the CLI uses when the user pastes the callback URL by hand.
pub(super) const REDIRECT_PATH: &str = "/codev/oauth/success";
/// OAuth scopes requested by the CLI.
pub(super) const SCOPE: &str = "openid profile email offline_access";
/// `CoDev` backend that issues gateway keys and reports the gateway URL.
const BACKEND_PATH: &str = "/codev-backend";
/// Upper bound on each SSO or backend call.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Stored state for one `CoDev` account.
#[skip_serializing_none]
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Credentials {
    /// SSO bearer token, used only to talk to the `CoDev` backend.
    access_token: String,
    /// SSO refresh token.
    refresh_token: Option<String>,
    /// SSO access-token expiry in milliseconds since the Unix epoch.
    expires_at: i64,
    /// Account email from the SSO user info, shown on the dashboard.
    email: Option<String>,
    /// `LiteLLM` gateway key issued by the `CoDev` backend.
    api_key: Option<String>,
    /// Gateway base URL reported by the `CoDev` backend.
    gateway_url: Option<String>,
}

/// What a request needs to reach the gateway.
pub(super) struct Gateway {
    /// Gateway bearer key.
    pub(super) api_key: String,
    /// Gateway OpenAI-compatible base, ending in `/v1`.
    pub(super) base: String,
}

/// SSO token endpoint response.
#[derive(Deserialize)]
struct TokenResponse {
    /// New SSO access token.
    access_token: String,
    /// Rotated refresh token, if issued.
    refresh_token: Option<String>,
    /// Access-token lifetime in seconds.
    expires_in: i64,
}

/// Builds the service origin from an operator domain such as `example.com`.
fn origin_for(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels_ok = domain.split('.').count() >= 2
        && domain.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    labels_ok.then(|| format!("https://netmind.{domain}"))
}

/// Returns the full URL of a service path, using the domain from the environment.
pub(super) fn url(path: &str) -> Result<String, String> {
    let domain = std::env::var(DOMAIN_VAR).unwrap_or_default();
    origin_for(&domain)
        .map(|origin| format!("{origin}{path}"))
        .ok_or_else(|| {
            format!("CoDev is not configured: set {DOMAIN_VAR} to the operator's domain")
        })
}

/// Current time in milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Appends `/v1` to a gateway URL the way the CLI does.
pub(super) fn api_base(gateway: &str) -> String {
    let trimmed = gateway.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/v1")
    }
}

/// Reads an upstream error body into a short message.
async fn failure(what: &str, response: reqwest::Response) -> CredentialError {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    let detail = body
        .get("error_description")
        .or_else(|| body.get("error"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("request failed"));
    CredentialError::upstream(status, format!("CoDev {what} failed ({status}): {detail}"))
}

/// Posts a form to the SSO token endpoint.
async fn token(form: &[(&str, &str)]) -> Result<TokenResponse, CredentialError> {
    let response = crate::util::HTTP_CLIENT
        .post(url(TOKEN_PATH)?)
        .timeout(TIMEOUT)
        .form(form)
        .send()
        .await
        .map_err(|_| "Unable to contact CoDev SSO".to_string())?;
    if !response.status().is_success() {
        return Err(failure("token request", response).await);
    }
    response
        .json()
        .await
        .map_err(|_| "Invalid CoDev SSO token response".to_string().into())
}

/// Looks up the signed-in account's email.
async fn email(access_token: &str) -> Option<String> {
    let response = crate::util::HTTP_CLIENT
        .get(url(USERINFO_PATH).ok()?)
        .timeout(TIMEOUT)
        .bearer_auth(access_token)
        .send()
        .await
        .ok()?;
    let body: Value = response.error_for_status().ok()?.json().await.ok()?;
    body.get("email")
        .and_then(Value::as_str)
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty())
}

/// Calls a `CoDev` backend endpoint with the SSO token.
async fn backend(path: &str, access_token: &str) -> Result<Value, CredentialError> {
    let response = crate::util::HTTP_CLIENT
        .post(format!("{}/{path}", url(BACKEND_PATH)?))
        .timeout(TIMEOUT)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| "Unable to contact the CoDev backend".to_string())?;
    if !response.status().is_success() {
        return Err(failure(path, response).await);
    }
    response
        .json()
        .await
        .map_err(|_| format!("Invalid CoDev {path} response").into())
}

impl Credentials {
    /// Builds credentials from a token response, keeping the old refresh token if none was issued.
    fn apply(&mut self, tokens: TokenResponse) -> Result<(), CredentialError> {
        self.access_token = tokens.access_token;
        if tokens.refresh_token.is_some() {
            self.refresh_token = tokens.refresh_token;
        }
        self.expires_at = tokens
            .expires_in
            .checked_mul(1000)
            .and_then(|ms| now_ms().checked_add(ms))
            .ok_or_else(|| "CoDev token expiry is out of range".to_string())?;
        Ok(())
    }

    /// Refreshes the SSO token if it expires within a minute.
    async fn ensure_fresh(&mut self) -> Result<(), CredentialError> {
        if now_ms().saturating_add(60_000) < self.expires_at {
            return Ok(());
        }
        let refresh = self
            .refresh_token
            .clone()
            .ok_or_else(|| "CoDev session expired; sign in again".to_string())?;
        info!("Refreshing CoDev SSO token");
        let tokens = token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", CLIENT_ID),
        ])
        .await?;
        self.apply(tokens)
    }

    /// Asks the `CoDev` backend for a new gateway key and the gateway URL.
    async fn provision(&mut self) -> Result<(), CredentialError> {
        let key = backend("auth/exchange", &self.access_token).await?;
        let key = key
            .get("api_key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| "CoDev backend issued no gateway key".to_string())?;
        let config = backend("config", &self.access_token).await?;
        let gateway = config
            .get("gatewayUrl")
            .and_then(Value::as_str)
            .filter(|url| reqwest::Url::parse(url).is_ok_and(|url| url.scheme() == "https"))
            .ok_or_else(|| "CoDev backend returned no usable gateway URL".to_string())?;
        self.api_key = Some(key.to_owned());
        self.gateway_url = Some(gateway.to_owned());
        Ok(())
    }

    /// The gateway key and base, if both are known.
    fn gateway(&self) -> Option<Gateway> {
        Some(Gateway {
            api_key: self.api_key.clone()?,
            base: api_base(self.gateway_url.as_deref()?),
        })
    }
}

/// Completes a dashboard sign-in: trades the code for SSO tokens, then provisions a gateway key.
pub(super) async fn sign_in(code: &str, verifier: &str) -> Result<Credentials, CredentialError> {
    let tokens = token(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &url(REDIRECT_PATH)?),
        ("client_id", CLIENT_ID),
        ("code_verifier", verifier),
    ])
    .await?;
    let mut creds = Credentials {
        access_token: String::new(),
        refresh_token: None,
        expires_at: 0,
        email: None,
        api_key: None,
        gateway_url: None,
    };
    creds.apply(tokens)?;
    creds.email = email(&creds.access_token).await;
    creds.provision().await?;
    Ok(creds)
}

/// Persists a credential's state.
pub(super) async fn save(
    store: &SettingsStore,
    provider_id: &str,
    credential_id: &str,
    creds: &Credentials,
) -> Result<(), String> {
    let json = serde_json::to_string(creds)
        .map_err(|e| format!("Failed to serialize CoDev credentials: {e}"))?;
    crate::provider::credentials::save_oauth(store, provider_id, credential_id, "codev", &json)
        .await
}

/// Returns the gateway key and URL for a credential.
///
/// With `reissue`, the stored key is treated as rejected: the SSO token is
/// refreshed if needed and the backend is asked for a new key.
pub(super) async fn gateway(
    store: &SettingsStore,
    provider_id: &str,
    credential_id: &str,
    reissue: bool,
) -> Result<Option<Gateway>, CredentialError> {
    let lock = crate::provider::login::credential_lock(credential_id).await;
    let _guard = lock.lock().await;
    let Some(mut creds) = crate::provider::credentials::load_oauth::<Credentials>(
        store,
        provider_id,
        credential_id,
        "codev",
        "CoDev",
    )
    .await?
    else {
        return Ok(None);
    };
    if !reissue && let Some(gateway) = creds.gateway() {
        return Ok(Some(gateway));
    }
    creds.ensure_fresh().await?;
    creds.provision().await?;
    save(store, provider_id, credential_id, &creds).await?;
    Ok(creds.gateway())
}

#[cfg(test)]
mod tests {
    use super::{api_base, origin_for};

    /// The operator domain becomes the service host; anything that is not a plain domain is refused.
    #[test]
    fn origin_comes_from_a_plain_domain() {
        assert_eq!(
            origin_for(" Example.COM. ").as_deref(),
            Some("https://netmind.example.com")
        );
        for bad in [
            "",
            "localhost",
            "example.com/path",
            "a..b",
            "-a.com",
            "user@example.com",
        ] {
            assert_eq!(origin_for(bad), None, "{bad}");
        }
    }

    /// Gateway URLs gain exactly one `/v1`.
    #[test]
    fn api_base_appends_v1_once() {
        assert_eq!(api_base("https://gw.example"), "https://gw.example/v1");
        assert_eq!(api_base("https://gw.example/"), "https://gw.example/v1");
        assert_eq!(api_base("https://gw.example/v1/"), "https://gw.example/v1");
    }
}

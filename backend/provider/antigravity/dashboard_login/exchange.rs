//! Google OAuth callback validation and credential exchange.

use super::{AntigravityFinishResponse, OAuthFlows};
use crate::provider::antigravity::auth::{self as agy, AuthManager, Credentials};
use crate::provider::login::{credential_lock, error};
use crate::server::settings::SettingsStore;
use axum::{Json, http::StatusCode};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Token fields returned by Google's authorization-code exchange.
#[derive(Deserialize)]
struct GoogleToken {
    /// Google OAuth access token.
    access_token: String,
    /// Optional token for refreshing access.
    refresh_token: Option<String>,
    /// Access-token lifetime in seconds.
    expires_in: i64,
}

/// Validates the local redirect URL and extracts its state and authorization code.
pub(super) fn parse_google_callback(
    callback_url: &str,
) -> Result<(String, String), (StatusCode, String)> {
    let url = reqwest::Url::parse(callback_url).map_err(|_| error("Invalid callback URL"))?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port() != Some(agy::OAUTH_CALLBACK_PORT)
        || url.path() != agy::OAUTH_CALLBACK_PATH
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(error("Callback URL does not match the expected redirect"));
    }
    let mut state = None;
    let mut code = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "state" if state.is_none() => state = Some(value.into_owned()),
            "code" if code.is_none() => code = Some(value.into_owned()),
            "error" => return Err(error("Google authorization was denied")),
            "state" | "code" => return Err(error("Duplicate callback parameter")),
            _ => {}
        }
    }
    let state = state
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error("Missing OAuth state"))?;
    let code = code
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error("Missing authorization code"))?;
    Ok((state, code))
}

/// Exchanges the code, discovers the project, and stores Google credentials.
pub(super) async fn finish_google_exchange(
    settings: &SettingsStore,
    flows: &OAuthFlows,
    provider_id: &str,
    credential_id: &str,
    code: &str,
    verifier: &str,
) -> Result<Json<AntigravityFinishResponse>, (StatusCode, String)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| error(format!("Failed to create OAuth client: {e}")))?;
    let response = client
        .post(&flows.token_url)
        .form(&[
            ("client_id", agy::OAUTH_CLIENT_ID),
            ("client_secret", agy::OAUTH_CLIENT_SECRET),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", agy::OAUTH_CALLBACK_URL),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|e| error(format!("Token request failed: {e}")))?;
    if !response.status().is_success() {
        return Err(error(format!(
            "Google token exchange failed: {}",
            response.status()
        )));
    }
    let token: GoogleToken = response
        .json()
        .await
        .map_err(|e| error(format!("Invalid token response: {e}")))?;
    if token.access_token.trim().is_empty()
        || token
            .refresh_token
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        || token.expires_in <= 0
    {
        return Err(error(
            "Google token response is missing valid offline credentials",
        ));
    }
    let email = match client
        .get(&flows.userinfo_url)
        .bearer_auth(&token.access_token)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("email").and_then(|v| v.as_str()).map(str::to_string)),
        _ => None,
    };
    let project_id = crate::provider::antigravity::login::fetch_or_onboard_project_at(
        &client,
        &token.access_token,
        &flows.cloud_url,
    )
    .await
    .map_err(error)?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let expires_at = token
        .expires_in
        .checked_mul(1000)
        .and_then(|duration| now_ms.checked_add(duration))
        .ok_or_else(|| error("Token expiration is out of range"))?;
    let creds = Credentials {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        project_id,
        expires_at: Some(expires_at),
        email,
    };
    let lock = credential_lock(credential_id).await;
    let _guard = lock.lock().await;
    AuthManager::save(settings, provider_id, credential_id, &creds)
        .await
        .map_err(error)?;
    Ok(Json(AntigravityFinishResponse::Complete))
}

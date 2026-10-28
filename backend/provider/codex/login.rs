use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::provider::codex::auth::{
    CODEX_CLIENT_ID, CODEX_TOKEN_URL, CodexCredentials, decode_jwt_profile,
};

#[derive(Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: i64,
}

pub(crate) async fn exchange_token(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<CodexCredentials, String> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", CODEX_CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ];

    let token_resp = client
        .post(CODEX_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Failed to exchange authorization code: {e}"))?;

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed ({status}): {body}"));
    }

    let token_data = token_resp
        .json::<TokenExchangeResponse>()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    let profile = decode_jwt_profile(&token_data.access_token, token_data.id_token.as_deref());

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let expires_at = token_data
        .expires_in
        .checked_mul(1000)
        .and_then(|duration| now_ms.checked_add(duration))
        .ok_or_else(|| "Token expiration is out of range".to_string())?;

    Ok(CodexCredentials {
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        account_id: profile.account_id,
        email: profile.email,
        plan_type: profile.plan_type,
        residency: profile.residency,
        expires_at: Some(expires_at),
    })
}

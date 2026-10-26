use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::Query;
use axum::response::Html;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::codex::auth::{
    CODEX_AUTHORIZE_URL, CODEX_CALLBACK_PATH, CODEX_CALLBACK_PORT, CODEX_CALLBACK_URL,
    CODEX_CLIENT_ID, CODEX_DEVICE_AUTH_URL, CODEX_DEVICE_REDIRECT_URI, CODEX_DEVICE_TOKEN_URL,
    CODEX_DEVICE_USERCODE_URL, CODEX_SCOPES, CODEX_TOKEN_URL, CodexCredentials, decode_jwt_profile,
};
use crate::util::urlencode;

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    error: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: i64,
}

#[derive(Deserialize)]
struct DeviceAuthInitResponse {
    device_auth_id: String,
    user_code: String,
    interval: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: Option<String>,
    code_verifier: Option<String>,
}

fn generate_pkce() -> (String, String) {
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    let challenge = URL_SAFE_NO_PAD.encode(hash);
    (verifier, challenge)
}

pub(crate) async fn run_login(device_flow: bool) -> Result<CodexCredentials, String> {
    if device_flow {
        return run_device_login().await;
    }

    match run_browser_login().await {
        Ok(creds) => Ok(creds),
        Err(e) => {
            println!(
                "Browser OAuth login on port {CODEX_CALLBACK_PORT} failed ({e}). Falling back to device code flow...\n"
            );
            run_device_login().await
        }
    }
}

pub(crate) async fn run_browser_login() -> Result<CodexCredentials, String> {
    let (tx, rx) = oneshot::channel::<Result<String, String>>();
    let tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));

    let expected_state = uuid::Uuid::new_v4().to_string();
    let expected_state_clone = expected_state.clone();
    let tx_clone = tx.clone();

    let app = Router::new().route(
        CODEX_CALLBACK_PATH,
        get(move |Query(params): Query<CallbackQuery>| async move {
            let mut sender = tx_clone.lock().await;
            if let Some(s) = sender.take() {
                if params.state.as_deref() != Some(expected_state_clone.as_str()) {
                    let _ = s.send(Err("OAuth state mismatch".to_string()));
                    return Html(
                        "<html><body><h2>Authentication failed</h2><p>State mismatch.</p></body></html>"
                            .to_string(),
                    );
                }
                if let Some(err) = params.error {
                    let _ = s.send(Err(format!("OAuth error: {err}")));
                    return Html(format!(
                        "<html><body><h2>Authentication failed</h2><p>Error: {err}</p></body></html>"
                    ));
                }
                if let Some(code) = params.code {
                    let _ = s.send(Ok(code));
                    return Html(
                        "<html><body style=\"font-family: sans-serif; text-align: center; padding-top: 50px;\"><h2>OpenAI Codex Authentication Successful!</h2><p>You can close this tab and return to your terminal.</p></body></html>".to_string(),
                    );
                }
                let _ = s.send(Err("No code received".to_string()));
            }
            Html("<html><body><h2>Invalid request</h2></body></html>".to_string())
        }),
    );

    let addr = SocketAddr::from(([127, 0, 0, 1], CODEX_CALLBACK_PORT));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind to callback port {CODEX_CALLBACK_PORT}: {e}"))?;

    let (verifier, challenge) = generate_pkce();
    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=omp",
        CODEX_AUTHORIZE_URL,
        urlencode(CODEX_CLIENT_ID),
        urlencode(CODEX_CALLBACK_URL),
        urlencode(CODEX_SCOPES),
        urlencode(&challenge),
        urlencode(&expected_state),
    );

    println!(
        "\nOpen the following URL in your browser to sign in to OpenAI Codex (ChatGPT OAuth):\n\n{auth_url}\n"
    );
    println!("Waiting for OAuth callback on {CODEX_CALLBACK_URL}...");

    let server_handle =
        tokio::spawn(async move { axum::serve(listener, app).await.map_err(|e| e.to_string()) });

    let code = match rx.await {
        Ok(res) => res?,
        Err(_) => return Err("OAuth callback listener was canceled".to_string()),
    };

    server_handle.abort();
    println!("Authorization code received! Exchanging for tokens...");

    exchange_token(&code, &verifier, CODEX_CALLBACK_URL).await
}

pub(crate) async fn run_device_login() -> Result<CodexCredentials, String> {
    let client = reqwest::Client::new();
    let init_resp = client
        .post(CODEX_DEVICE_USERCODE_URL)
        .json(&serde_json::json!({ "client_id": CODEX_CLIENT_ID }))
        .send()
        .await
        .map_err(|e| format!("Failed to initiate device auth: {e}"))?;

    if !init_resp.status().is_success() {
        let status = init_resp.status();
        let body = init_resp.text().await.unwrap_or_default();
        return Err(format!("Device auth initiation failed ({status}): {body}"));
    }

    let init_data: DeviceAuthInitResponse = init_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse device auth response: {e}"))?;

    let interval_secs = init_data
        .interval
        .and_then(|v| {
            if let Some(n) = v.as_u64() {
                Some(n)
            } else if let Some(s) = v.as_str() {
                s.parse::<u64>().ok()
            } else {
                None
            }
        })
        .unwrap_or(5);

    println!("\nDevice Authorization Flow:");
    println!("1. Open: {CODEX_DEVICE_AUTH_URL}");
    println!("2. Enter code: {}\n", init_data.user_code);
    println!("Waiting for authorization in browser...");

    let poll_interval = Duration::from_secs(interval_secs);
    let max_polls = 120; // 10 minutes total

    for _ in 0..max_polls {
        tokio::time::sleep(poll_interval).await;

        let poll_resp = client
            .post(CODEX_DEVICE_TOKEN_URL)
            .json(&serde_json::json!({
                "device_auth_id": init_data.device_auth_id,
                "user_code": init_data.user_code,
            }))
            .send()
            .await
            .map_err(|e| format!("Failed to poll device auth token: {e}"))?;

        let status = poll_resp.status();
        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
            // Still pending
            continue;
        }

        if !status.is_success() {
            let body = poll_resp.text().await.unwrap_or_default();
            return Err(format!("Device token polling failed ({status}): {body}"));
        }

        let token_data: DeviceTokenResponse = poll_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse device token data: {e}"))?;

        if let (Some(code), Some(verifier)) =
            (token_data.authorization_code, token_data.code_verifier)
        {
            println!("Device authorized! Exchanging authorization code for tokens...");
            return exchange_token(&code, &verifier, CODEX_DEVICE_REDIRECT_URI).await;
        }
    }

    Err("Device authorization timed out (user did not complete login in time)".to_string())
}

async fn exchange_token(
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
        .map_or(0, |d| d.as_millis() as i64);
    let expires_at = now_ms + token_data.expires_in * 1000;

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

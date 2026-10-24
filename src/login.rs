use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::extract::Query;
use axum::response::Html;
use axum::routing::get;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;

use crate::auth::{
    ANTIGRAVITY_USER_AGENT, CLOUD_CODE_ASSIST_ENDPOINT, Credentials, OAUTH_CALLBACK_PATH,
    OAUTH_CALLBACK_PORT, OAUTH_CALLBACK_URL, OAUTH_SCOPES, OAUTH_TOKEN_URL, oauth_client_id,
    oauth_client_secret,
};

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
}

#[derive(Deserialize)]
struct UserInfoResponse {
    email: Option<String>,
}

#[derive(Deserialize)]
struct LoadCodeAssistResponse {
    #[serde(default, rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<String>,
    #[serde(default, rename = "currentTier")]
    current_tier: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct OnboardOperation {
    name: Option<String>,
    done: Option<bool>,
    error: Option<serde_json::Value>,
}

pub(crate) async fn run_login() -> Result<Credentials, String> {
    let (tx, rx) = oneshot::channel::<Result<String, String>>();
    let tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));

    let tx_clone = tx.clone();
    let app = Router::new().route(
        OAUTH_CALLBACK_PATH,
        get(move |Query(params): Query<CallbackQuery>| async move {
            let mut sender = tx_clone.lock().await;
            if let Some(s) = sender.take() {
                if let Some(err) = params.error {
                    let _ = s.send(Err(format!("OAuth error: {err}")));
                    return Html(format!(
                        "<html><body><h2>Authentication failed</h2><p>Error: {err}</p></body></html>"
                    ));
                }
                if let Some(code) = params.code {
                    let _ = s.send(Ok(code));
                    return Html(
                        "<html><body style=\"font-family: sans-serif; text-align: center; padding-top: 50px;\"><h2>Authentication successful!</h2><p>You can close this tab and return to your terminal.</p></body></html>".to_string(),
                    );
                }
                let _ = s.send(Err("No code received".to_string()));
            }
            Html("<html><body><h2>Invalid request</h2></body></html>".to_string())
        }),
    );

    let addr = SocketAddr::from(([127, 0, 0, 1], OAUTH_CALLBACK_PORT));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind to callback port {OAUTH_CALLBACK_PORT}: {e}"))?;

    let scopes_encoded = OAUTH_SCOPES.join(" ");
    let client_id = oauth_client_id()?;
    let client_secret = oauth_client_secret()?;
    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
        urlencoding::encode(&client_id),
        urlencoding::encode(OAUTH_CALLBACK_URL),
        urlencoding::encode(&scopes_encoded)
    );

    println!(
        "\nOpen the following URL in your browser to sign in to Google Antigravity:\n\n{auth_url}\n"
    );
    println!("Waiting for OAuth callback on http://127.0.0.1:{OAUTH_CALLBACK_PORT}...");

    let server_handle =
        tokio::spawn(async move { axum::serve(listener, app).await.map_err(|e| e.to_string()) });

    let code = match rx.await {
        Ok(res) => res?,
        Err(_) => return Err("OAuth listener was canceled".to_string()),
    };

    server_handle.abort();

    println!("Authorization code received! Exchanging for tokens...");

    let client = reqwest::Client::new();
    let token_params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("code", &code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", OAUTH_CALLBACK_URL),
    ];

    let token_resp = client
        .post(OAUTH_TOKEN_URL)
        .form(&token_params)
        .send()
        .await
        .map_err(|e| format!("Failed to request token: {e}"))?;

    if !token_resp.status().is_success() {
        let err = token_resp.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {err}"));
    }

    let token_data: TokenExchangeResponse = token_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let expires_at = now_ms + token_data.expires_in * 1000;

    // Fetch user email
    let user_info_resp = client
        .get("https://www.googleapis.com/oauth2/v1/userinfo?alt=json")
        .header(
            "Authorization",
            format!("Bearer {}", token_data.access_token),
        )
        .send()
        .await;
    let mut email = None;
    if let Ok(resp) = user_info_resp
        && let Ok(info) = resp.json::<UserInfoResponse>().await
    {
        email = info.email;
    }

    println!("Resolving Cloud Code Assist project...");
    let project_id = discover_project(&client, &token_data.access_token).await?;

    let creds = Credentials {
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        project_id,
        expires_at: Some(expires_at),
        email,
    };

    Ok(creds)
}

async fn discover_project(client: &reqwest::Client, access_token: &str) -> Result<String, String> {
    let load_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal:loadCodeAssist");
    let resp = client
        .post(&load_url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&json!({
            "metadata": { "ideType": "ANTIGRAVITY" }
        }))
        .send()
        .await
        .map_err(|e| format!("loadCodeAssist request failed: {e}"))?;

    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(format!("loadCodeAssist failed: {err}"));
    }

    let mut data: LoadCodeAssistResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse loadCodeAssist: {e}"))?;

    if data.current_tier.is_none() {
        println!("Provisioning Antigravity tier...");
        onboard_user(client, access_token).await?;

        // Re-query after onboarding
        let resp = client
            .post(&load_url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", "application/json")
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .json(&json!({
                "metadata": { "ideType": "ANTIGRAVITY" }
            }))
            .send()
            .await
            .map_err(|e| format!("Second loadCodeAssist failed: {e}"))?;

        data = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse second loadCodeAssist: {e}"))?;
    }

    if let Some(pid) = data.cloudaicompanion_project
        && !pid.is_empty()
    {
        return Ok(pid);
    }

    Err("loadCodeAssist did not return a valid cloudaicompanionProject".to_string())
}

async fn onboard_user(client: &reqwest::Client, access_token: &str) -> Result<(), String> {
    let onboard_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal:onboardUser");
    let resp = client
        .post(&onboard_url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&json!({
            "tierId": "free-tier",
            "metadata": { "ideType": "ANTIGRAVITY" }
        }))
        .send()
        .await
        .map_err(|e| format!("onboardUser request failed: {e}"))?;

    let mut op: OnboardOperation = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse onboardUser response: {e}"))?;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);

    while op.done != Some(true) {
        if std::time::Instant::now() >= deadline {
            return Err("onboardUser timed out after 30 seconds".to_string());
        }

        tokio::time::sleep(Duration::from_secs(1)).await;

        let op_name = op.name.as_deref().unwrap_or_default();
        if op_name.is_empty() {
            return Err("onboardUser returned operation with no name".to_string());
        }

        let op_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal/{op_name}");
        let poll_resp = client
            .get(&op_url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .send()
            .await
            .map_err(|e| format!("Failed to poll operation: {e}"))?;

        op = poll_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse operation status: {e}"))?;
    }

    if let Some(err) = op.error {
        return Err(format!("onboardUser operation failed: {err}"));
    }

    Ok(())
}

mod urlencoding {
    use std::fmt::Write;

    pub(super) fn encode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                out.push(b as char);
            } else {
                let _ = write!(out, "%{b:02X}");
            }
        }
        out
    }
}

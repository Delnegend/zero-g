use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::extract::Query;
use axum::response::Html;
use axum::routing::get;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;

use crate::antigravity::auth::{
    ANTIGRAVITY_USER_AGENT, CLOUD_CODE_ASSIST_ENDPOINT, Credentials, OAUTH_CALLBACK_PATH,
    OAUTH_CALLBACK_PORT, OAUTH_CALLBACK_URL, OAUTH_SCOPES, OAUTH_TOKEN_URL, oauth_client_id,
    oauth_client_secret,
};
use crate::server::settings::SettingsStore;
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

pub(crate) async fn run_login(store: &SettingsStore) -> Result<Credentials, String> {
    let (tx, rx) = oneshot::channel::<Result<String, String>>();
    let tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));

    let expected_state = uuid::Uuid::new_v4().to_string();
    let expected_state_clone = expected_state.clone();
    let tx_clone = tx.clone();
    let app = Router::new().route(
        OAUTH_CALLBACK_PATH,
        get(move |Query(params): Query<CallbackQuery>| async move {
            let mut sender = tx_clone.lock().await;
            if let Some(s) = sender.take() {
                if params.state.as_deref() != Some(expected_state_clone.as_str()) {
                    let _ = s.send(Err("OAuth state mismatch".to_string()));
                    return Html("<html><body><h2>Authentication failed</h2><p>State mismatch.</p></body></html>".to_string());
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
    let client_id = oauth_client_id(store).await?;
    let client_secret = oauth_client_secret(store).await?;
    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&access_type=offline&prompt=consent",
        urlencode(&client_id),
        urlencode(OAUTH_CALLBACK_URL),
        urlencode(&scopes_encoded),
        urlencode(&expected_state)
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
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed ({status}): {body}"));
    }

    let token_data: TokenExchangeResponse = token_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
        + token_data.expires_in * 1000;

    println!("Fetching account identity...");
    let user_info_resp = client
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .bearer_auth(&token_data.access_token)
        .send()
        .await;

    let email = match user_info_resp {
        Ok(resp) if resp.status().is_success() => resp
            .json::<UserInfoResponse>()
            .await
            .ok()
            .and_then(|u| u.email),
        _ => None,
    };

    println!("Discovering Cloud Code Assist project...");
    let project_id =
        match fetch_or_onboard_project(&client, &token_data.access_token, email.as_deref()).await {
            Ok(pid) => pid,
            Err(e) => {
                return Err(format!("Failed to configure companion project: {e}"));
            }
        };

    Ok(Credentials {
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        project_id,
        expires_at: Some(expires_at),
        email,
    })
}

pub(crate) async fn fetch_or_onboard_project(
    client: &reqwest::Client,
    access_token: &str,
    email: Option<&str>,
) -> Result<String, String> {
    let load_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal:loadCodeAssist");
    let load_body = json!({
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });

    let load_resp = client
        .post(&load_url)
        .bearer_auth(access_token)
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&load_body)
        .send()
        .await
        .map_err(|e| format!("Failed to call loadCodeAssist: {e}"))?;

    if load_resp.status().is_success() {
        let load_data: LoadCodeAssistResponse = load_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse loadCodeAssist response: {e}"))?;

        if let Some(pid) = load_data.cloudaicompanion_project
            && !pid.is_empty()
        {
            return Ok(pid);
        }

        if let Some(tier) = load_data.current_tier
            && let Some(name) = tier.get("name").and_then(|n| n.as_str())
            && !name.is_empty()
        {
            return Ok(name.to_string());
        }
    }

    println!("Attempting to provision a companion project via onboardUser...");
    onboard_user(client, access_token).await?;

    let retry_resp = client
        .post(&load_url)
        .bearer_auth(access_token)
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&load_body)
        .send()
        .await
        .map_err(|e| format!("loadCodeAssist retry failed: {e}"))?;

    if retry_resp.status().is_success() {
        let load_data: LoadCodeAssistResponse = retry_resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse loadCodeAssist retry: {e}"))?;

        if let Some(pid) = load_data.cloudaicompanion_project
            && !pid.is_empty()
        {
            return Ok(pid);
        }
    }

    if let Some(email) = email {
        let clean = email.replace(['@', '.'], "-");
        let fallback = format!("ag-{clean}");
        println!("Using derived project name fallback: {fallback}");
        return Ok(fallback);
    }

    Err("Could not discover or onboard project ID".to_string())
}

async fn onboard_user(client: &reqwest::Client, access_token: &str) -> Result<(), String> {
    let onboard_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal:onboardUser");
    let onboard_body = json!({
        "tierType": "TIER_FREE"
    });

    let resp = client
        .post(&onboard_url)
        .bearer_auth(access_token)
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .json(&onboard_body)
        .send()
        .await
        .map_err(|e| format!("onboardUser request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("onboardUser endpoint failed ({status}): {body}"));
    }

    let mut op: OnboardOperation = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse onboardUser response: {e}"))?;

    let poll_interval = Duration::from_secs(2);
    let max_polls = 15;

    for _ in 0..max_polls {
        if op.done == Some(true) {
            break;
        }

        tokio::time::sleep(poll_interval).await;

        let Some(op_name) = &op.name else {
            break;
        };

        let poll_url = format!("{CLOUD_CODE_ASSIST_ENDPOINT}/v1internal/{op_name}");
        let poll_resp = client
            .get(&poll_url)
            .bearer_auth(access_token)
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .send()
            .await
            .map_err(|e| format!("Failed to poll operation: {e}"))?;

        if !poll_resp.status().is_success() {
            break;
        }

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

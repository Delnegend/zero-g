use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    provider::{credentials, registry},
    server::settings::SettingsStore,
    storage::{ProviderCredential, ProviderRecord, Secret},
};

#[derive(Debug, Serialize)]
pub(crate) struct QuotaWindow {
    pub(crate) label: String,
    pub(crate) remaining_percent: Option<f64>,
    pub(crate) reset_at: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum QuotaSnapshot {
    Quota { windows: Vec<QuotaWindow> },
    Balance { amount: f64, currency: &'static str },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsoleConnection {
    workspace_id: String,
    session: String,
}

fn session_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
}

async fn connection(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
    input: Option<ConsoleConnection>,
) -> Response {
    if let Some(ref input) = input
        && (!session_valid(&input.session)
            || input.workspace_id.is_empty()
            || input.workspace_id.len() > 256
            || reqwest::header::HeaderValue::from_str(&input.workspace_id).is_err())
    {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid Console session or workspace ID",
        )
            .into_response();
    }
    let session_name = format!("credential.{credential}.console_session");
    let workspace_name = format!("credential.{credential}.console_workspace");
    let encrypted = input
        .as_ref()
        .map(|input| {
            Ok::<_, String>((
                store.encrypt(&session_name, &input.session)?,
                store.encrypt(&workspace_name, &input.workspace_id)?,
            ))
        })
        .transpose();
    let Ok(encrypted) = encrypted else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let lock = crate::provider::login::credential_lock(credential).await;
    let _guard = lock.lock().await;
    let result = store
        .database
        .retry(|| async {
            let mut db = (*store.database.db).clone();
            let mut tx = db.transaction().await?;
            let p = ProviderRecord::filter_by_id(provider)
                .first()
                .exec(&mut tx)
                .await?;
            let c = ProviderCredential::filter_by_id(credential)
                .first()
                .exec(&mut tx)
                .await?;
            if !matches!(p, Some(p) if !p.deleted && p.kind == "zen")
                || !matches!(c, Some(c) if c.provider_id == provider)
            {
                return Ok(false);
            }
            if let Some((session, workspace)) = &encrypted {
                Secret::upsert_by_name(session_name.as_str())
                    .value(session.clone())
                    .exec(&mut tx)
                    .await?;
                Secret::upsert_by_name(workspace_name.as_str())
                    .value(workspace.clone())
                    .exec(&mut tx)
                    .await?;
            } else {
                Secret::delete_by_name(&mut tx, &session_name).await?;
                Secret::delete_by_name(&mut tx, &workspace_name).await?;
            }
            tx.commit().await?;
            Ok(true)
        })
        .await;
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn save_console(
    State(store): State<SettingsStore>,
    Path((provider, credential)): Path<(String, String)>,
    Json(input): Json<ConsoleConnection>,
) -> Response {
    connection(&store, &provider, &credential, Some(input)).await
}

async fn delete_console(
    State(store): State<SettingsStore>,
    Path((provider, credential)): Path<(String, String)>,
) -> Response {
    connection(&store, &provider, &credential, None).await
}

fn parse_balance(value: &Value) -> Result<QuotaSnapshot, String> {
    let amount = value
        .get("balanceMicroCents")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        })
        .filter(|v| v.is_finite())
        .ok_or("OpenCode Console did not return a balance")?
        / 100_000_000.0;
    Ok(QuotaSnapshot::Balance {
        amount,
        currency: "USD",
    })
}

async fn zen_balance(store: &SettingsStore, credential: &str) -> Result<QuotaSnapshot, String> {
    let session = store
        .secret_get(&format!("credential.{credential}.console_session"))
        .await?
        .ok_or("Connect an OpenCode Console session to see the balance")?;
    let workspace = store
        .secret_get(&format!("credential.{credential}.console_workspace"))
        .await?
        .ok_or("Connect an OpenCode Console workspace to see the balance")?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Unable to create Console client")?;
    let response = client
        .get("https://opencode.ai/console/api/billing/status")
        .header(
            reqwest::header::COOKIE,
            format!("__Host-console_session={session}"),
        )
        .header("x-org-id", workspace)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "Unable to contact OpenCode Console")?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenCode Console returned HTTP {}; reconnect the session if it expired",
            response.status().as_u16()
        ));
    }
    let value = response
        .json::<Value>()
        .await
        .map_err(|_| "Invalid OpenCode Console balance response")?;
    parse_balance(&value)
}

fn parse_go(value: &Value) -> Result<QuotaSnapshot, String> {
    let usage = value
        .get("usage")
        .ok_or("Invalid OpenCode Go usage response")?;
    let mut windows = Vec::new();
    for (field, label) in [
        ("rolling", "5 hours"),
        ("weekly", "7 days"),
        ("monthly", "Monthly"),
    ] {
        let window = usage
            .get(field)
            .ok_or("Incomplete OpenCode Go usage response")?;
        let used = window
            .get("percent")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .ok_or("Invalid OpenCode Go usage percentage")?;
        let reset_at = window
            .get("resetsAt")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp());
        windows.push(QuotaWindow {
            label: label.to_string(),
            remaining_percent: Some(100.0 - used.clamp(0.0, 100.0)),
            reset_at,
        });
    }
    Ok(QuotaSnapshot::Quota { windows })
}

async fn go_usage(store: &SettingsStore, credential: &str) -> Result<QuotaSnapshot, String> {
    let key = store
        .secret_get(&format!("credential.{credential}.api_key"))
        .await?
        .ok_or("OpenCode Go API key is not connected")?;
    if key.is_empty() {
        return Err("OpenCode Go requires an API key".into());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Unable to create OpenCode Go client")?;
    let response = client
        .get("https://opencode.ai/zen/go/v1/usage")
        .bearer_auth(key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "Unable to contact OpenCode Go")?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenCode Go returned HTTP {}",
            response.status().as_u16()
        ));
    }
    parse_go(
        &response
            .json::<Value>()
            .await
            .map_err(|_| "Invalid OpenCode Go usage response")?,
    )
}

async fn usage(
    State(store): State<SettingsStore>,
    Path((provider_id, credential_id)): Path<(String, String)>,
) -> Response {
    let provider = match registry::get(&store.database, &provider_id).await {
        Ok(Some(value)) => value,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match credentials::get(&store, &provider_id, &credential_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
    let result = match provider.kind.as_str() {
        "codex" => crate::provider::codex::quota::fetch(&store, &provider_id, &credential_id).await,
        "agy" => {
            crate::provider::antigravity::quota::fetch(&store, &provider_id, &credential_id).await
        }
        "zen" => zen_balance(&store, &credential_id).await,
        "go" => go_usage(&store, &credential_id).await,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                "Usage is not available for this provider",
            )
                .into_response();
        }
    };
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(message) => (StatusCode::BAD_GATEWAY, message).into_response(),
    }
}

pub(crate) fn router(store: SettingsStore) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/usage",
            get(usage),
        )
        .route(
            "/api/providers/{id}/credentials/{credential_id}/console",
            put(save_console).delete(delete_console),
        )
        .with_state(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn console_credentials_are_scoped_encrypted_and_deleted() {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*store.database.db).clone();
        toasty::create!(ProviderRecord {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            kind: "zen".to_string(),
            base_url: "https://example.com/v1".to_string(),
            deleted: false
        })
        .exec(&mut db)
        .await
        .unwrap();
        toasty::create!(ProviderCredential {
            id: "credential".to_string(),
            provider_id: "zen".to_string(),
            name: "Account".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut db)
        .await
        .unwrap();
        let create = || ConsoleConnection {
            workspace_id: "workspace".into(),
            session: "private-session".into(),
        };
        assert_eq!(
            connection(&store, "other", "credential", Some(create()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            connection(
                &store,
                "zen",
                "credential",
                Some(ConsoleConnection {
                    workspace_id: "workspace".into(),
                    session: "bad;session".into()
                })
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            connection(&store, "zen", "credential", Some(create()))
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
        let secret = Secret::filter_by_name("credential.credential.console_session")
            .first()
            .exec(&mut db)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(secret.value, b"private-session");
        assert_eq!(
            store
                .secret_get("credential.credential.console_session")
                .await
                .unwrap()
                .as_deref(),
            Some("private-session")
        );
        let response = credentials::router(store.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/providers/zen/credentials/credential")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            store
                .secret_get("credential.credential.console_session")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .secret_get("credential.credential.console_workspace")
                .await
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn balance_uses_actual_microcents() {
        let value = serde_json::json!({"balanceMicroCents":"123456789"});
        assert!(
            matches!(parse_balance(&value).unwrap(), QuotaSnapshot::Balance {amount, ..} if (amount-1.234_567_89).abs()<1e-9)
        );
        assert!(parse_balance(&serde_json::json!({"availableMicroCents":"1"})).is_err());
    }
    #[test]
    fn go_usage_preserves_remaining_and_rejects_missing_windows() {
        let value = serde_json::json!({"usage":{"rolling":{"percent":25,"resetsAt":"2026-01-01T00:00:00Z"},"weekly":{"percent":80},"monthly":{"percent":101}}});
        let QuotaSnapshot::Quota { windows } = parse_go(&value).unwrap() else {
            panic!()
        };
        assert_eq!(
            windows
                .iter()
                .map(|x| x.remaining_percent)
                .collect::<Vec<_>>(),
            vec![Some(75.0), Some(20.0), Some(0.0)]
        );
        assert!(windows[0].reset_at.is_some());
        assert!(parse_go(&serde_json::json!({"usage":{"rolling":{"percent":25}}})).is_err());
    }
}

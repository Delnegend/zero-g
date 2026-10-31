/// Dashboard metadata from `OpenCode` model records.
pub(crate) mod catalog;

/// HTTP routes for Zen models and chat completions.
pub(crate) mod routes;

use crate::database::transaction_with_retry;
use crate::provider::quota::QuotaSnapshot;
use crate::server::settings::SettingsStore;
use crate::storage::Secret;
use axum::Json;
use axum::response::{IntoResponse, Response};
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::put,
};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

/// `OpenCode` Console credentials used to retrieve a workspace balance.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConsoleConnection {
    /// Console workspace identifier sent as the organization header.
    pub(crate) workspace_id: String,
    /// Console session cookie value.
    pub(crate) session: String,
}

/// Check that a session is a bounded, valid cookie value.
fn session_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
}

/// Save or remove encrypted Console credentials for a Zen provider credential.
pub(crate) async fn connection(
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
    let result = transaction_with_retry!(store.database, |mut tx| {
        if !crate::provider::credentials::owns_auxiliary_secret(
            &mut tx, provider, credential, "zen",
        )
        .await?
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
    });
    match result {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Zen Console connection API.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(save_console, delete_console),
    components(schemas(ConsoleConnection))
)]
pub(crate) struct Api;

/// Save a Console connection from the dashboard.
#[utoipa::path(put, path = "/api/providers/{id}/credentials/{credential_id}/console", operation_id = "saveZenConsole", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = ConsoleConnection, responses((status = 204)))]
async fn save_console(
    State(store): State<SettingsStore>,
    Path((provider, credential)): Path<(String, String)>,
    Json(input): Json<ConsoleConnection>,
) -> Response {
    connection(&store, &provider, &credential, Some(input)).await
}

/// Remove a Console connection from the dashboard.
#[utoipa::path(delete, path = "/api/providers/{id}/credentials/{credential_id}/console", operation_id = "deleteZenConsole", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 204)))]
async fn delete_console(
    State(store): State<SettingsStore>,
    Path((provider, credential)): Path<(String, String)>,
) -> Response {
    connection(&store, &provider, &credential, None).await
}
/// Build dashboard routes for managing Console connections.
pub(crate) fn dashboard_router(app: crate::server::AppState) -> Router {
    Router::new()
        .route(
            "/api/providers/{id}/credentials/{credential_id}/console",
            put(save_console).delete(delete_console),
        )
        .with_state(app.settings)
}

/// Convert Console microcents to a USD balance snapshot.
pub(crate) fn parse_balance(value: &Value) -> Result<QuotaSnapshot, String> {
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

/// Fetch the Console balance using stored workspace and session credentials.
pub(crate) async fn zen_balance(
    store: &SettingsStore,
    credential: &str,
) -> Result<QuotaSnapshot, String> {
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

/// Zen provider backend.
pub(crate) struct Backend;

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "zen",
        label: "Zen (OpenCode)",
        oauth: false,
        api_key: true,
        fixed_id: Some("zen"),
        fixed_name: Some("OpenCode Zen"),
        keyless: true,
        default_base_url: Some(routes::BASE_URL),
        legacy: super::common::LegacyPolicy {
            key: None,
            same_base: false,
        },
    };
    fn router(settings: &SettingsStore) -> axum::Router<crate::server::AppState> {
        routes::router(settings)
    }
    async fn discover(
        _state: &crate::server::AppState,
        _provider: &super::registry::Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        let permissions = crate::server::settings::Permissions {
            providers: vec![],
            models: vec![],
        };
        let value = routes::fetch_models(routes::MODELS_URL, &permissions)
            .await
            .map_err(|_| "Unable to discover Zen models".to_string())?;
        let models = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or("Invalid Zen model list")?;
        Ok(models
            .iter()
            .filter_map(|model| serde_json::from_value(model.clone()).ok())
            .collect())
    }
}

impl super::common::QuotaProvider for Backend {
    async fn quota(
        store: &SettingsStore,
        _provider: &str,
        credential: &str,
    ) -> Result<QuotaSnapshot, String> {
        zen_balance(store, credential).await
    }
}

impl super::common::DashboardProvider for Backend {
    fn dashboard_router(state: crate::server::AppState) -> Router {
        dashboard_router(state)
    }
}

use std::{sync::LazyLock, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::put,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    database::transaction_with_retry,
    provider::{common, openai, quota::QuotaSnapshot, registry::Provider},
    server::{AppState, settings::SettingsStore},
    storage::{ProviderCredential, ProviderRecord, Secret},
};

/// `OpenRouter`'s fixed inference endpoint.
pub(crate) const BASE_URL: &str = "https://openrouter.ai/api/v1";
/// Account credit endpoint requiring a management key.
const CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";

/// `OpenRouter` provider backend.
pub(crate) struct Backend;

impl openai::ModelEndpoint for Backend {
    async fn request(_provider: &Provider, key: &str) -> Result<reqwest::Response, &'static str> {
        openai::send(
            BASE_URL,
            key,
            "models",
            None,
            Method::GET,
            &HeaderMap::new(),
            None,
        )
        .await
    }
}

impl common::Provider for Backend {
    const DESCRIPTOR: common::ProviderDescriptor = common::ProviderDescriptor {
        value: "openrouter",
        label: "OpenRouter",
        oauth: false,
        api_key: true,
        fixed_id: Some("openrouter"),
        fixed_name: Some("OpenRouter"),
        keyless: false,
        default_base_url: Some(BASE_URL),
        legacy: common::LegacyPolicy {
            key: None,
            same_base: false,
        },
    };

    fn router(settings: &SettingsStore) -> Router<AppState> {
        openai::routes::router_for(settings, "openrouter", Some(BASE_URL))
    }

    async fn discover(
        state: &AppState,
        provider: &Provider,
    ) -> Result<Vec<crate::openai::Model>, String> {
        openai::discover::<Self>(&state.settings, provider).await
    }
}

/// Management key used only for querying `OpenRouter` account credits.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManagementKey {
    /// `OpenRouter` management API key.
    management_key: String,
}

/// `OpenRouter` management key API.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(save_management_key, delete_management_key),
    components(schemas(ManagementKey))
)]
pub(crate) struct Api;

/// Store or delete a management key atomically with credential ownership checks.
async fn connection(
    store: &SettingsStore,
    provider: &str,
    credential: &str,
    input: Option<ManagementKey>,
) -> Response {
    if let Some(input) = &input
        && (input.management_key.is_empty()
            || input.management_key.len() > 4096
            || reqwest::header::HeaderValue::from_str(&format!("Bearer {}", input.management_key))
                .is_err())
    {
        return (StatusCode::BAD_REQUEST, "Invalid management key").into_response();
    }
    let name = format!("credential.{credential}.openrouter_management_key");
    let Ok(encrypted) = input
        .as_ref()
        .map(|input| store.encrypt(&name, &input.management_key))
        .transpose()
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let lock = crate::provider::login::credential_lock(credential).await;
    let _guard = lock.lock().await;
    let result = transaction_with_retry!(store.database, |mut tx| {
        let p = ProviderRecord::filter_by_id(provider)
            .first()
            .exec(&mut tx)
            .await?;
        let c = ProviderCredential::filter_by_id(credential)
            .first()
            .exec(&mut tx)
            .await?;
        if !matches!(p, Some(p) if !p.deleted && p.kind == "openrouter")
            || !matches!(c, Some(c) if c.provider_id == provider)
        {
            return Ok(false);
        }
        if let Some(value) = &encrypted {
            Secret::upsert_by_name(name.as_str())
                .value(value.clone())
                .exec(&mut tx)
                .await?;
        } else {
            Secret::delete_by_name(&mut tx, &name).await?;
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

/// Save the management key for an `OpenRouter` credential.
#[utoipa::path(put, path = "/api/providers/{id}/credentials/{credential_id}/openrouter/management-key", operation_id = "saveOpenrouterManagementKey", params(("id" = String, Path), ("credential_id" = String, Path)), request_body = ManagementKey, responses((status = 204)))]
async fn save_management_key(
    State(store): State<SettingsStore>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<ManagementKey>,
) -> Response {
    if let Err(err) = crate::provider::login::same_origin(&headers) {
        return err.into_response();
    }
    connection(&store, &provider, &credential, Some(input)).await
}

/// Delete the management key for an `OpenRouter` credential.
#[utoipa::path(delete, path = "/api/providers/{id}/credentials/{credential_id}/openrouter/management-key", operation_id = "deleteOpenrouterManagementKey", params(("id" = String, Path), ("credential_id" = String, Path)), responses((status = 204)))]
async fn delete_management_key(
    State(store): State<SettingsStore>,
    Path((provider, credential)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(err) = crate::provider::login::same_origin(&headers) {
        return err.into_response();
    }
    connection(&store, &provider, &credential, None).await
}

impl common::DashboardProvider for Backend {
    fn dashboard_router(state: AppState) -> Router {
        Router::new()
            .route(
                "/api/providers/{id}/credentials/{credential_id}/openrouter/management-key",
                put(save_management_key).delete(delete_management_key),
            )
            .with_state(state.settings)
    }
}

/// Shared client for account credit requests.
static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
});

/// Parse purchased credits and spending into a remaining USD balance.
fn parse_balance(value: &Value) -> Result<QuotaSnapshot, String> {
    let data = value
        .get("data")
        .ok_or("Invalid OpenRouter credits response")?;
    let number = |name| {
        data.get(name)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .ok_or("Invalid OpenRouter credits response")
    };
    let amount = number("total_credits")? - number("total_usage")?;
    if !amount.is_finite() {
        return Err("Invalid OpenRouter credits response".into());
    }
    Ok(QuotaSnapshot::Balance {
        amount,
        currency: "USD",
    })
}

/// Read a credits response without exposing upstream error bodies.
async fn read_balance(response: reqwest::Response) -> Result<QuotaSnapshot, String> {
    if !response.status().is_success() {
        return Err(format!(
            "OpenRouter credits returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let value = response
        .json::<Value>()
        .await
        .map_err(|_| "Invalid OpenRouter credits response")?;
    parse_balance(&value)
}

impl common::QuotaProvider for Backend {
    async fn quota(
        store: &SettingsStore,
        provider: &str,
        credential: &str,
    ) -> Result<QuotaSnapshot, String> {
        // Do not allow an unscoped credential identifier to access another provider's secret.
        let p = crate::provider::registry::get(&store.database, provider)
            .await
            .map_err(|_| "Failed to load provider")?;
        if !matches!(p, Some(p) if p.kind == "openrouter")
            || crate::provider::credentials::get(store, provider, credential)
                .await
                .map_err(|_| "Failed to load credential")?
                .is_none()
        {
            return Err("OpenRouter credential not found".into());
        }
        let key = store
            .secret_get(&format!(
                "credential.{credential}.openrouter_management_key"
            ))
            .await
            .map_err(|_| "Failed to load OpenRouter management key")?
            .ok_or("Connect an OpenRouter management key to see the balance")?;
        let client = CLIENT
            .as_ref()
            .map_err(|_| "Unable to create OpenRouter client")?;
        let response = client
            .get(CREDITS_URL)
            .bearer_auth(key)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| "Unable to contact OpenRouter credits")?;
        read_balance(response).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{database::Database, provider::common::QuotaProvider};
    use axum::{body::Body, http::Request};
    use serde_json::json;
    use tower::ServiceExt;

    #[test]
    fn credits_require_both_finite_numbers_and_compute_balance() {
        assert!(matches!(
            parse_balance(&json!({"data":{"total_credits": 12.5, "total_usage": 3.25}})).unwrap(),
            QuotaSnapshot::Balance { amount, currency: "USD" } if (amount - 9.25).abs() < 1e-9
        ));
        for value in [
            json!({}),
            json!({"data":{"total_credits":12}}),
            json!({"data":{"total_credits":"12", "total_usage":1}}),
            json!({"data":{"total_credits":12, "total_usage":null}}),
            json!({"data":{"total_credits":1e308, "total_usage":-1e308}}),
        ] {
            assert!(parse_balance(&value).is_err());
        }
    }

    #[tokio::test]
    async fn upstream_errors_do_not_expose_response_body() {
        let app = Router::new().route(
            "/",
            axum::routing::get(|| async {
                (StatusCode::UNAUTHORIZED, "sensitive upstream details")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let error = read_balance(response).await.unwrap_err();
        assert_eq!(error, "OpenRouter credits returned HTTP 401");
        server.abort();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines, clippy::literal_string_with_formatting_args)]
    async fn management_keys_are_scoped_encrypted_and_same_origin() {
        let store = SettingsStore::new(
            Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        let mut db = (*store.database.db).clone();
        for (id, kind) in [("openrouter", "openrouter"), ("other", "openai")] {
            toasty::create!(ProviderRecord {
                id: id.to_string(),
                name: id.to_string(),
                kind: kind.to_string(),
                base_url: String::new(),
                deleted: false
            })
            .exec(&mut db)
            .await
            .unwrap();
        }
        for (id, provider) in [("one", "openrouter"), ("two", "other")] {
            toasty::create!(ProviderCredential {
                id: id.to_string(),
                provider_id: provider.to_string(),
                name: id.to_string(),
                weight: 1,
                enabled: true
            })
            .exec(&mut db)
            .await
            .unwrap();
        }
        let app = Router::new()
            .route(
                "/api/providers/{id}/credentials/{credential_id}/openrouter/management-key",
                put(save_management_key).delete(delete_management_key),
            )
            .with_state(store.clone());
        let request = |provider: &str, credential: &str, origin: &str| {
            Request::builder()
                .method("PUT")
                .uri(format!(
                    "/api/providers/{provider}/credentials/{credential}/openrouter/management-key"
                ))
                .header("host", "localhost")
                .header("origin", origin)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"management_key":"private-key"}"#))
                .unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(request("openrouter", "one", "https://evil.example"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request("openrouter", "two", "http://localhost"))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.clone()
                .oneshot(request("other", "two", "http://localhost"))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.clone()
                .oneshot(request("openrouter", "one", "http://localhost"))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let name = "credential.one.openrouter_management_key";
        let secret = Secret::filter_by_name(name)
            .first()
            .exec(&mut db)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(secret.value, b"private-key");
        assert_eq!(
            store.secret_get(name).await.unwrap().as_deref(),
            Some("private-key")
        );
        assert!(Backend::quota(&store, "other", "one").await.is_err());
        assert!(Backend::quota(&store, "openrouter", "two").await.is_err());
        let delete = Request::builder()
            .method("DELETE")
            .uri("/api/providers/openrouter/credentials/one/openrouter/management-key")
            .header("host", "localhost")
            .header("origin", "http://localhost")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.oneshot(delete).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert!(store.secret_get(name).await.unwrap().is_none());
    }
}

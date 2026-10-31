/// Native `OpenRouter` Decisions API forwarding.
mod decisions;

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
    storage::Secret,
};

/// `OpenRouter`'s fixed inference endpoint.
pub(crate) const BASE_URL: &str = "https://openrouter.ai/api/v1";
/// `OpenRouter` API root used for endpoints outside `/api/v1`.
const API_ROOT: &str = "https://openrouter.ai/api";
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
            Some("output_modalities=text,decisions"),
            Method::GET,
            &HeaderMap::new(),
            None,
        )
        .await
    }

    fn models_from_list(value: &Value) -> Vec<crate::openai::Model> {
        value
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let mut entry = entry.clone();
                if let Some(fields) = entry.as_object_mut()
                    && fields.get("reasoning").is_some_and(Value::is_object)
                    && let Some(reasoning) = fields.remove("reasoning")
                {
                    fields.insert("openrouter_reasoning".into(), reasoning);
                }
                serde_json::from_value::<crate::openai::Model>(entry).ok()
            })
            .filter(|model| !model.id.is_empty())
            .collect()
    }
}

/// Extract catalog metadata from an `OpenRouter` discovery record.
pub(crate) fn catalog_info(model: &crate::openai::Model) -> common::catalog::CatalogInfo {
    use common::catalog::{CatalogInfo, CatalogPrices};

    fn strings(value: &Value) -> Option<Vec<String>> {
        value
            .as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect()
    }

    fn price(value: Option<&Value>) -> Option<f64> {
        let per_token = value?.as_str()?.parse::<f64>().ok()?;
        let per_million = per_token * 1_000_000.0;
        (per_token >= 0.0 && per_million.is_finite()).then_some(per_million)
    }

    let extra = &model.extra;
    let parameters = extra.get("supported_parameters").and_then(strings);
    let reasoning = extra.get("openrouter_reasoning").and_then(Value::as_object);
    let pricing = extra
        .get("pricing")
        .filter(|value| value.is_object())
        .map(|raw| CatalogPrices {
            input: price(raw.get("prompt")),
            output: price(raw.get("completion")),
            cache_read: price(raw.get("input_cache_read")),
            cache_write: price(raw.get("input_cache_write")),
            source: "openrouter",
            raw: raw.clone(),
        });
    CatalogInfo {
        context_window: extra.get("context_length").and_then(Value::as_u64),
        max_output_tokens: extra
            .get("top_provider")
            .and_then(|value| value.get("max_completion_tokens"))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        reasoning: model.reasoning.or_else(|| {
            (reasoning.is_some()
                || parameters.as_ref().is_some_and(|parameters| {
                    parameters.iter().any(|parameter| {
                        parameter == "reasoning" || parameter == "include_reasoning"
                    })
                }))
            .then_some(true)
        }),
        tool_call: parameters
            .as_ref()
            .map(|parameters| parameters.iter().any(|parameter| parameter == "tools")),
        input_modalities: extra
            .get("architecture")
            .and_then(|value| value.get("input_modalities"))
            .and_then(strings),
        reasoning_efforts: reasoning
            .and_then(|value| value.get("supported_efforts"))
            .and_then(strings),
        pricing,
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
            .merge(decisions::router(settings, API_ROOT))
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
        if !crate::provider::credentials::owns_auxiliary_secret(
            &mut tx,
            provider,
            credential,
            "openrouter",
        )
        .await?
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
    use crate::storage::{ProviderCredential, ProviderRecord};
    use crate::{database::Database, provider::common::QuotaProvider};
    use axum::{body::Body, http::Request};
    use serde_json::json;
    use tower::ServiceExt;

    #[test]
    fn discovery_preserves_openrouter_reasoning_and_catalog_metadata() {
        use crate::provider::openai::ModelEndpoint;

        let reasoning = json!({
            "default_effort": "medium", "default_enabled": false,
            "mandatory": false, "supported_efforts": ["high", "medium", "low"]
        });
        let pricing = json!({
            "prompt": "0.000002", "completion": "0.000004",
            "input_cache_read": "0.0000002", "input_cache_write": "0",
            "overrides": [{"min_prompt_tokens": 100_000, "prompt": "0.000001"}]
        });
        let list = json!({"data": [{
            "id": "vendor/nested/model:free",
            "context_length": 131_072,
            "top_provider": {"max_completion_tokens": 8192},
            "architecture": {"input_modalities": ["text", "image"]},
            "supported_parameters": ["tools", "reasoning"],
            "reasoning": reasoning,
            "pricing": pricing,
            "permission": [{"id": "permission-id"}],
            "other": {"preserved": true}
        }]});
        let models = Backend::models_from_list(&list);
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.id, "vendor/nested/model:free");
        assert_eq!(model.extra["openrouter_reasoning"], reasoning);
        assert_eq!(model.extra["other"], json!({"preserved": true}));
        assert_eq!(model.extra["permission"], json!([{"id": "permission-id"}]));
        let info = catalog_info(model);
        assert_eq!(info.context_window, Some(131_072));
        assert_eq!(info.max_output_tokens, Some(8192));
        assert_eq!(info.reasoning, Some(true));
        assert_eq!(info.tool_call, Some(true));
        assert_eq!(info.input_modalities.unwrap(), ["text", "image"]);
        assert_eq!(info.reasoning_efforts.unwrap(), ["high", "medium", "low"]);
        let prices = info.pricing.unwrap();
        assert_eq!(prices.input, Some(2.0));
        assert_eq!(prices.output, Some(4.0));
        assert!((prices.cache_read.unwrap() - 0.2).abs() < 1e-12);
        assert_eq!(prices.cache_write, Some(0.0));
        assert_eq!(prices.source, "openrouter");
        assert_eq!(prices.raw, pricing);
    }

    #[test]
    fn missing_and_malformed_catalog_values_are_not_invented() {
        use crate::provider::openai::ModelEndpoint;

        let models = Backend::models_from_list(&json!({"data": [
            {"id": "missing"},
            {"id": "invalid", "reasoning": false, "supported_parameters": [],
             "architecture": {"input_modalities": ["text", 1]},
             "context_length": -1, "top_provider": {"max_completion_tokens": 4_294_967_296_u64},
             "pricing": {"prompt": "-1", "completion": "NaN",
                         "input_cache_read": "1e308", "input_cache_write": 0}},
            {"id": "zero", "pricing": {"prompt": "0", "completion": "bad"}},
            {"id": "reasoning-param", "supported_parameters": ["reasoning"]}
        ]}));
        let missing = catalog_info(&models[0]);
        assert!(missing.reasoning.is_none());
        assert!(missing.tool_call.is_none());
        assert!(missing.pricing.is_none());
        let invalid = catalog_info(&models[1]);
        assert_eq!(invalid.reasoning, Some(false));
        assert_eq!(invalid.tool_call, Some(false));
        assert!(invalid.context_window.is_none());
        assert!(invalid.max_output_tokens.is_none());
        assert!(invalid.input_modalities.is_none());
        let prices = invalid.pricing.unwrap();
        assert!(prices.input.is_none());
        assert!(prices.output.is_none());
        assert!(prices.cache_read.is_none());
        assert!(prices.cache_write.is_none());
        let zero = catalog_info(&models[2]).pricing.unwrap();
        assert_eq!(zero.input, Some(0.0));
        assert!(zero.output.is_none());
        assert_eq!(catalog_info(&models[3]).reasoning, Some(true));
    }

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

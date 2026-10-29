use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::provider::common::proxy::{ProxyError, check_model, error, settings_error};
use crate::provider::{openai, registry::Provider};
use crate::server::AppState;
use crate::server::settings::{self, Permissions, SettingsStore};

/// Official OpenAI-compatible Zen inference endpoint.
pub(super) const BASE_URL: &str = "https://opencode.ai/inference/openai/v1";
/// Official catalog of Zen model IDs.
pub(super) const MODELS_URL: &str = "https://opencode.ai/inference/v1/models";
/// Community metadata supplements, but never defines, the official model list.
const METADATA_URL: &str = "https://models.dev/api.json?type=all";
/// Maximum accepted size of the metadata catalog.
const METADATA_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Freshness window for successfully fetched metadata.
const METADATA_TTL: Duration = Duration::from_hours(1);
/// Retry delay after a metadata fetch fails.
const METADATA_FAILURE_TTL: Duration = Duration::from_secs(60);

/// Metadata indexed by the provider's model ID.
type Metadata = serde_json::Map<String, Value>;

/// Community catalog fields needed for Zen discovery.
#[derive(serde::Deserialize)]
struct MetadataCatalog {
    /// `OpenCode` provider metadata.
    opencode: MetadataProvider,
}

/// Models published for one provider.
#[derive(serde::Deserialize)]
struct MetadataProvider {
    /// Records keyed by model ID.
    models: Metadata,
}
/// Last fetch time and its optional result.
type MetadataCache = Option<(Instant, Option<Arc<Metadata>>)>;
/// Single-flight cache for the shared metadata catalog.
static METADATA_CACHE: LazyLock<Mutex<MetadataCache>> = LazyLock::new(|| Mutex::new(None));

/// Fetch only the `OpenCode` section, bounding request time and catalog size.
async fn load_metadata(url: &str) -> Result<Metadata, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(|error| error.to_string())?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|size| usize::try_from(size).map_or(true, |size| size > METADATA_MAX_BYTES))
    {
        return Err("catalog exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len().saturating_add(chunk.len()) > METADATA_MAX_BYTES {
            return Err("catalog exceeds size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let catalog: MetadataCatalog =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    Ok(catalog.opencode.models)
}

/// Share one bounded fetch across requests, including a short backoff on failure.
async fn metadata() -> Option<Arc<Metadata>> {
    let mut cache = METADATA_CACHE.lock().await;
    if let Some((fetched, data)) = &*cache {
        let ttl = if data.is_some() {
            METADATA_TTL
        } else {
            METADATA_FAILURE_TTL
        };
        if fetched.elapsed() < ttl {
            return data.clone();
        }
    }
    let data = match load_metadata(METADATA_URL).await {
        Ok(data) => Some(Arc::new(data)),
        Err(error) => {
            tracing::warn!("Zen Models.dev metadata unavailable: {error}");
            None
        }
    };
    *cache = Some((Instant::now(), data.clone()));
    data
}

/// Attach the exact matched metadata and populate only valid, missing model fields.
fn enrich_models(value: &mut Value, metadata: &Metadata) {
    let Some(models) = value.get_mut("data").and_then(Value::as_array_mut) else {
        return;
    };
    for model in models {
        let Some(entry) = model.as_object_mut() else {
            continue;
        };
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(info) = metadata.get(id).filter(|info| info.is_object()) else {
            continue;
        };
        let fields = [
            ("name", info.get("name").filter(|v| v.is_string())),
            (
                "description",
                info.get("description").filter(|v| v.is_string()),
            ),
            (
                "reasoning",
                info.get("reasoning").filter(|v| v.is_boolean()),
            ),
            (
                "context_window",
                info.pointer("/limit/context")
                    .filter(|v| v.as_u64().is_some()),
            ),
            (
                "max_output_tokens",
                info.pointer("/limit/output")
                    .filter(|v| v.as_u64().is_some_and(|n| u32::try_from(n).is_ok())),
            ),
        ];
        for (key, field) in fields {
            if entry.get(key).is_none_or(Value::is_null)
                && let Some(field) = field
            {
                entry.insert(key.to_string(), field.clone());
            }
        }
        entry.insert("zen_metadata".into(), info.clone());
    }
}

/// Build authenticated Zen model and inference routes.
pub(crate) fn router(settings: &SettingsStore) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models_handler))
        .route("/v1/models/", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/chat/completions/", post(chat_handler))
        .route("/v1/responses", post(responses_handler))
        .route("/v1/responses/", post(responses_handler))
        .layer(axum::middleware::from_fn_with_state(
            (settings.clone(), "zen"),
            settings::virtual_key_auth,
        ))
}

/// Fetch and filter the current Zen model list.
pub(super) async fn fetch_models(
    url: &str,
    permissions: &Permissions,
) -> Result<Value, ProxyError> {
    let mut value = fetch_official_models(url, permissions).await?;
    if let Some(metadata) = metadata().await {
        enrich_models(&mut value, &metadata);
    }
    Ok(value)
}

/// Fetch and filter IDs from Zen independently of optional community metadata.
async fn fetch_official_models(url: &str, permissions: &Permissions) -> Result<Value, ProxyError> {
    let response = reqwest::Client::new().get(url).send().await.map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "Unable to fetch Zen models",
            "upstream_error",
            "bad_gateway",
        )
    })?;
    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Unable to fetch Zen models",
            "upstream_error",
            "bad_gateway",
        ));
    }
    let mut value: Value = response.json().await.map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "Invalid Zen model list",
            "upstream_error",
            "bad_gateway",
        )
    })?;
    let data = value
        .get_mut("data")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "Invalid Zen model list",
                "upstream_error",
                "bad_gateway",
            )
        })?;
    data.retain(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| permissions.allows_model(id))
    });
    Ok(value)
}

/// Return models allowed by the virtual key.
async fn models_handler(Extension(permissions): Extension<Permissions>) -> Response {
    match fetch_models(MODELS_URL, &permissions).await {
        Ok(models) => Json(models).into_response(),
        Err(error) => error.into_response(),
    }
}

/// Pass authorized inference requests to Zen unchanged.
async fn inference(
    state: AppState,
    permissions: Permissions,
    provider: Provider,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    path: &str,
) -> Response {
    inference_with_base(
        &state.settings,
        &permissions,
        &provider,
        BASE_URL,
        (query.as_deref(), &headers),
        body,
        path,
    )
    .await
}

/// Forward inference to a given Zen-compatible base, after checking model permissions.
async fn inference_with_base(
    settings: &SettingsStore,
    permissions: &Permissions,
    provider: &Provider,
    base: &str,
    request: (Option<&str>, &HeaderMap),
    body: Bytes,
    path: &str,
) -> Response {
    if let Err(response) = check_model(&body, permissions) {
        return response.into_response();
    }
    match send_upstream(settings, provider, base, path, request.0, request.1, body).await {
        Ok(response) => openai::forward(response),
        Err(error) => error.into_response(),
    }
}

/// Handle an OpenAI-compatible chat request.
async fn chat_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        query,
        headers,
        body,
        "chat/completions",
    )
    .await
}

/// Handle an OpenAI-compatible Responses request.
async fn responses_handler(
    State(state): State<AppState>,
    Extension(permissions): Extension<Permissions>,
    Extension(provider): Extension<Provider>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    inference(
        state,
        permissions,
        provider,
        query,
        headers,
        body,
        "responses",
    )
    .await
}

/// Forward a request with weighted Zen credentials.
async fn send_upstream(
    settings: &SettingsStore,
    provider: &Provider,
    base: &str,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<reqwest::Response, ProxyError> {
    let credentials = crate::provider::credentials::list(settings, &provider.id)
        .await
        .map_err(|_| settings_error())?;
    let mut last = None;
    for credential in credentials {
        let key = settings
            .secret_get(&format!("credential.{}.api_key", credential.id))
            .await
            .map_err(|_| settings_error())?
            .ok_or_else(settings_error)?;
        let response = openai::send_with_headers(
            base,
            &key,
            path,
            query,
            Method::POST,
            headers,
            Some(body.clone()),
            true,
        )
        .await
        .map_err(|message| {
            error(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "bad_gateway",
            )
        })?;
        if !matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            return Ok(response);
        }
        last = Some(response);
    }
    last.ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No connected Zen credentials",
            "upstream_error",
            "no_credentials",
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn model_discovery_keeps_free_ids_and_filters_permissions_without_losing_metadata() {
        let app = Router::new().route("/models", get(|| async {
            Json(json!({"object":"list","data":[
                {"id":"gpt-paid","extra":1}, {"id":"new-free"}, {"id":"model:free"}, {"id":"big-pickle"}, {"id":"excluded"}
            ]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/models", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let permissions = Permissions {
            providers: vec![],
            models: vec![
                "gpt-paid".into(),
                "new-free".into(),
                "model:free".into(),
                "big-pickle".into(),
            ],
        };
        let mut models = fetch_official_models(&url, &permissions).await.unwrap();
        let metadata = json!({"gpt-paid":{"name":"Paid"}, "new-free":{"name":"Free"},
            "model:free":{"name":"Free colon"}, "excluded":{"name":"Excluded"}, "catalog-only":{"name":"Other"}});
        enrich_models(&mut models, metadata.as_object().unwrap());
        assert_eq!(
            models["data"],
            json!([
                {"id":"gpt-paid", "extra":1, "name":"Paid", "zen_metadata":{"name":"Paid"}},
                {"id":"new-free", "name":"Free", "zen_metadata":{"name":"Free"}},
                {"id":"model:free", "name":"Free colon", "zen_metadata":{"name":"Free colon"}},
                {"id":"big-pickle"}
            ])
        );
        server.abort();
    }

    #[test]
    fn metadata_enrichment_uses_exact_ids_and_preserves_unknowns_and_units() {
        let mut list = json!({"data":[
            {"id":"paid", "name":"Official", "extra":1},
            {"id":"incomplete"},
            {"id":"unknown"},
            {"id":"PAID"}
        ]});
        let catalog = json!({
            "paid":{"id":"different", "name":"Community", "description":"A model",
                "reasoning":false, "limit":{"context":0,"output":0},
                "cost":{"input":0,"output":1.25}, "tool_call":true},
            "incomplete":{"name":123,"reasoning":"yes","limit":{"context":-1,"output":4_294_967_296_u64}},
            "catalog-only":{"name":"Not advertised"}
        });
        enrich_models(&mut list, catalog.as_object().unwrap());
        let paid = &list["data"][0];
        assert_eq!(paid["id"], "paid");
        assert_eq!(paid["name"], "Official");
        assert_eq!(paid["description"], "A model");
        assert_eq!(paid["context_window"], 0);
        assert_eq!(paid["max_output_tokens"], 0);
        assert_eq!(paid["reasoning"], false);
        assert_eq!(paid["extra"], 1);
        assert_eq!(paid["zen_metadata"], catalog["paid"]);
        assert_eq!(paid["zen_metadata"]["cost"]["output"], 1.25);
        let typed: crate::openai::Model = serde_json::from_value(paid.clone()).unwrap();
        assert_eq!(typed.extra["zen_metadata"]["cost"]["input"], 0);
        assert_eq!(
            list["data"][1],
            json!({
                "id":"incomplete", "zen_metadata":catalog["incomplete"]
            })
        );
        assert_eq!(list["data"][2], json!({"id":"unknown"}));
        assert_eq!(list["data"][3], json!({"id":"PAID"}));
        assert_eq!(list["data"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn metadata_fetch_selects_opencode_and_rejects_bad_catalogs() {
        let app = Router::new()
            .route(
                "/catalog",
                get(|| async {
                    Json(
                        json!({"opencode":{"models":{"exact":{"cost":{"input":2.5}}}},
                    "other":{"models":{"other-id":{}}}}),
                    )
                }),
            )
            .route("/invalid", get(|| async { Json(json!({"other":{}})) }))
            .route(
                "/oversize",
                get(|| async {
                    (
                        [(
                            axum::http::header::CONTENT_LENGTH,
                            (METADATA_MAX_BYTES + 1).to_string(),
                        )],
                        "",
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let metadata = load_metadata(&format!("{base}/catalog")).await.unwrap();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata["exact"]["cost"]["input"], 2.5);
        assert!(load_metadata(&format!("{base}/invalid")).await.is_err());
        assert!(load_metadata(&format!("{base}/oversize")).await.is_err());
        server.abort();
    }

    #[tokio::test]
    // Keep the shared upstream and credential setup with the request scenarios.
    #[allow(clippy::too_many_lines)]
    async fn inference_passes_both_apis_and_free_ids_through_with_auth() {
        use crate::storage::{ProviderCredential, ProviderRecord, Secret};
        let store = SettingsStore::new(
            crate::database::Database::open(":memory:").await.unwrap(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        )
        .await
        .unwrap();
        toasty::create!(ProviderRecord {
            id: "zen".to_string(),
            name: "Zen".to_string(),
            kind: "zen".to_string(),
            base_url: String::new(),
            deleted: false
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        toasty::create!(ProviderCredential {
            id: "key".to_string(),
            provider_id: "zen".to_string(),
            name: "key".to_string(),
            weight: 1,
            enabled: true
        })
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
        let name = "credential.key.api_key";
        Secret::upsert_by_name(name)
            .value(store.encrypt(name, "secret").unwrap())
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        let handler = |RawQuery(query): RawQuery, headers: HeaderMap, body: Bytes| async move {
            assert_eq!(query.as_deref(), Some("x=1"));
            if headers.contains_key("x-keyless-test") {
                assert!(!headers.contains_key("authorization"));
            } else {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer secret");
            }
            assert_eq!(headers.get("x-custom").unwrap(), "test");
            assert!(!headers.contains_key("cookie"));
            assert!(!headers.contains_key("x-api-key"));
            assert!(
                body == Bytes::from_static(
                    b"{\"model\":\"model:free\",\"stream\":true,\"custom\":1}"
                ) || body
                    == Bytes::from_static(b"{\"model\":\"gpt-paid\",\"stream\":true,\"custom\":1}")
            );
            (StatusCode::OK, "data: [DONE]\n\n")
        };
        let app = Router::new()
            .route("/v1/responses", post(handler))
            .route("/v1/chat/completions", post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = Provider {
            id: "zen".into(),
            name: "Zen".into(),
            kind: "zen".into(),
            base_url: "http://untrusted.invalid/v1".into(),
        };
        let body = Bytes::from_static(b"{\"model\":\"model:free\",\"stream\":true,\"custom\":1}");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer virtual-key".parse().unwrap());
        headers.insert("cookie", "session=private".parse().unwrap());
        headers.insert("x-api-key", "private".parse().unwrap());
        headers.insert("x-custom", "test".parse().unwrap());
        let permissions = Permissions {
            providers: vec![],
            models: vec!["model:free".into(), "gpt-paid".into()],
        };
        for request_body in [
            body.clone(),
            Bytes::from_static(b"{\"model\":\"gpt-paid\",\"stream\":true,\"custom\":1}"),
        ] {
            for path in ["responses", "chat/completions"] {
                let response = inference_with_base(
                    &store,
                    &permissions,
                    &provider,
                    &base,
                    (Some("x=1"), &headers),
                    request_body.clone(),
                    path,
                )
                .await;
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                assert_eq!(bytes, "data: [DONE]\n\n");
            }
        }
        Secret::upsert_by_name(name)
            .value(store.encrypt(name, "").unwrap())
            .exec(&mut (*store.database.db).clone())
            .await
            .unwrap();
        headers.insert("x-keyless-test", "true".parse().unwrap());
        let response = inference_with_base(
            &store,
            &permissions,
            &provider,
            &base,
            (Some("x=1"), &headers),
            body.clone(),
            "chat/completions",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes, "data: [DONE]\n\n");
        let denied = Permissions {
            providers: vec![],
            models: vec!["other".into()],
        };
        let response = inference_with_base(
            &store,
            &denied,
            &provider,
            &base,
            (None, &headers),
            body,
            "responses",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        server.abort();
    }
}

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use prost::Message;
use serde_json::json;

use crate::openai::Model;
use crate::provider::common::errors::UpstreamError;
use crate::provider::registry::Provider;
use crate::server::AppState;

/// Default Cursor API endpoint.
const DEFAULT_BASE_URL: &str = "https://api2.cursor.sh";
/// Cursor CLI build advertised to the service.
const CLIENT_VERSION: &str = "cli-2026.09.02-c22c1a3";
/// Account-scoped runnable model RPC.
const USABLE_PATH: &str = "/agent.v1.AgentService/GetUsableModels";
/// Rich model metadata RPC.
const AVAILABLE_PATH: &str = "/aiserver.v1.AiService/AvailableModels";

/// Account-scoped usable model response. Unknown protobuf fields are ignored.
#[derive(Clone, PartialEq, Message)]
struct UsableResponse {
    /// Runnable account models.
    #[prost(message, repeated, tag = "1")]
    models: Vec<UsableModel>,
}

/// Model details returned by the agent service.
#[derive(Clone, PartialEq, Message)]
struct UsableModel {
    /// Runnable model identifier.
    #[prost(string, tag = "1")]
    model_id: String,
    /// Presence indicates thinking support.
    #[prost(message, optional, tag = "2")]
    thinking_details: Option<ThinkingDetails>,
    /// Display model identifier.
    #[prost(string, tag = "3")]
    display_model_id: String,
    /// Full display name.
    #[prost(string, tag = "4")]
    display_name: String,
    /// Short display name.
    #[prost(string, tag = "5")]
    display_name_short: String,
    /// Alternative model identifiers.
    #[prost(string, repeated, tag = "6")]
    aliases: Vec<String>,
    /// Whether this is a max-mode model.
    #[prost(bool, optional, tag = "7")]
    max_mode: Option<bool>,
}

/// Presence marker for thinking support.
#[derive(Clone, PartialEq, Message)]
struct ThinkingDetails {}

/// Rich model discovery request.
#[derive(Clone, PartialEq, Message)]
struct AvailableRequest {
    /// Include long-context models.
    #[prost(bool, tag = "2")]
    include_long_context_models: bool,
    /// Request parameterized models.
    #[prost(bool, optional, tag = "5")]
    use_model_parameters: Option<bool>,
    /// Request text without markdown.
    #[prost(bool, optional, tag = "7")]
    do_not_use_markdown: Option<bool>,
    /// Include cloud agent effort modes.
    #[prost(bool, optional, tag = "12")]
    use_cloud_agent_effort_modes: Option<bool>,
}

/// Rich available model response.
#[derive(Clone, PartialEq, Message)]
struct AvailableResponse {
    /// Legacy list of model names.
    #[prost(string, repeated, tag = "1")]
    model_names: Vec<String>,
    /// Detailed model entries.
    #[prost(message, repeated, tag = "2")]
    models: Vec<AvailableModel>,
}

/// Relevant subset of rich model metadata.
#[derive(Clone, PartialEq, Message)]
struct AvailableModel {
    /// Base model identifier.
    #[prost(string, tag = "1")]
    name: String,
    /// Whether this model only supports chat.
    #[prost(bool, optional, tag = "4")]
    is_chat_only: Option<bool>,
    /// Whether this model supports agents.
    #[prost(bool, optional, tag = "5")]
    supports_agent: Option<bool>,
    /// Whether this model supports thinking.
    #[prost(bool, optional, tag = "9")]
    supports_thinking: Option<bool>,
    /// Whether this model supports images.
    #[prost(bool, optional, tag = "10")]
    supports_images: Option<bool>,
    /// Standard context limit.
    #[prost(uint32, optional, tag = "15")]
    context_token_limit: Option<u32>,
    /// Max-mode context limit.
    #[prost(uint32, optional, tag = "16")]
    context_token_limit_for_max_mode: Option<u32>,
    /// User-facing display name.
    #[prost(string, optional, tag = "17")]
    client_display_name: Option<String>,
    /// Server-side model name.
    #[prost(string, optional, tag = "18")]
    server_model_name: Option<String>,
    /// Available model configurations.
    #[prost(message, repeated, tag = "30")]
    variants: Vec<Variant>,
    /// Whether the model is hidden.
    #[prost(bool, optional, tag = "35")]
    is_hidden: Option<bool>,
    /// Legacy runnable identifiers.
    #[prost(string, repeated, tag = "36")]
    legacy_slugs: Vec<String>,
    /// Alternative identifiers.
    #[prost(string, repeated, tag = "37")]
    id_aliases: Vec<String>,
    /// Whether the model requires data retention.
    #[prost(bool, optional, tag = "46")]
    requires_data_retention: Option<bool>,
}

/// One rich model configuration.
#[derive(Clone, PartialEq, Message)]
struct Variant {
    /// Variant display name.
    #[prost(string, optional, tag = "2")]
    display_name: Option<String>,
    /// Whether this variant uses max mode.
    #[prost(bool, optional, tag = "3")]
    is_max_mode: Option<bool>,
    /// Parameterized representation of the variant.
    #[prost(string, optional, tag = "9")]
    string_representation: Option<String>,
    /// Legacy runnable variant identifier.
    #[prost(string, optional, tag = "11")]
    legacy_slug: Option<String>,
}

/// Discover the catalog for one eligible account, trying another only for
/// authentication or rate-limit failures, as for the other provider pools.
pub(crate) async fn fetch_catalog_pool(
    state: &AppState,
    provider: &Provider,
    identity: Option<&crate::provider::common::request_identity::RequestIdentity>,
) -> Result<Vec<Model>, String> {
    let credentials = crate::provider::credentials::list(&state.settings, &provider.id).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to initialize Cursor client: {e}"))?;
    let base = if provider.base_url.is_empty() {
        DEFAULT_BASE_URL
    } else {
        &provider.base_url
    };
    let mut last = None;
    for credential in credentials {
        let token =
            match super::auth::access_token(&state.settings, &provider.id, &credential.id).await {
                Ok(token) => token,
                Err(error) => {
                    last = Some(error);
                    continue;
                }
            };
        match fetch_models(&client, base, &token).await {
            Ok(models) => {
                if let Some(identity) = identity {
                    identity.set_credential(&credential.id, &credential.name);
                }
                return Ok(models);
            }
            Err(error) if error.retryable() => last = Some(error.message),
            Err(error) => return Err(error.message),
        }
    }
    Err(last.unwrap_or_else(|| {
        "Cursor is not configured. Authenticate on the dashboard Providers page.".to_string()
    }))
}

/// Execute an authenticated unary protobuf request.
async fn unary(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
    body: Vec<u8>,
) -> Result<Vec<u8>, UpstreamError> {
    let response = client
        .post(format!("{}{path}", base.trim_end_matches('/')))
        .bearer_auth(token)
        .header("content-type", "application/proto")
        .header("connect-protocol-version", "1")
        .header("x-cursor-client-type", "cli")
        .header("x-cursor-client-version", CLIENT_VERSION)
        .header("x-ghost-mode", "true")
        .header("x-request-id", uuid::Uuid::new_v4().to_string())
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Cursor model discovery request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(UpstreamError {
            status: Some(status),
            message: format!("Cursor model discovery returned {status}"),
        });
    }
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("Failed to read Cursor model discovery response: {e}").into())
}

/// Join the account's usable models with available metadata.
async fn fetch_models(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<Vec<Model>, UpstreamError> {
    let request = AvailableRequest {
        include_long_context_models: true,
        use_model_parameters: Some(true),
        do_not_use_markdown: Some(true),
        use_cloud_agent_effort_modes: Some(true),
    };
    let (usable, available) = tokio::join!(
        unary(client, base, USABLE_PATH, token, Vec::new()),
        unary(client, base, AVAILABLE_PATH, token, request.encode_to_vec()),
    );
    let usable = usable.and_then(|bytes| {
        UsableResponse::decode(bytes.as_slice())
            .map_err(|e| format!("Invalid Cursor usable models response: {e}").into())
    });
    let available = available.and_then(|bytes| {
        AvailableResponse::decode(bytes.as_slice())
            .map_err(|e| format!("Invalid Cursor available models response: {e}").into())
    });
    match (usable, available) {
        (Ok(usable), Ok(available)) => Ok(normalize(Some(usable), Some(available))),
        (Ok(usable), Err(_)) => Ok(normalize(Some(usable), None)),
        (Err(_), Ok(available)) => Ok(normalize(None, Some(available))),
        (Err(first), Err(second)) => Err(if first.retryable() {
            first
        } else if second.retryable() {
            second
        } else {
            first
        }),
    }
}

/// Trim a string and discard empty values.
fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// Enumerate identifiers associated with a rich model.
fn rich_ids(model: &AvailableModel) -> impl Iterator<Item = &str> {
    std::iter::once(model.name.as_str())
        .chain(model.legacy_slugs.iter().map(String::as_str))
        .chain(model.id_aliases.iter().map(String::as_str))
        .chain(model.variants.iter().flat_map(|v| {
            [v.legacy_slug.as_deref(), v.string_representation.as_deref()]
                .into_iter()
                .flatten()
        }))
}

/// Exclude models unavailable to the agent or incompatible with ghost mode.
fn permitted(model: &AvailableModel) -> bool {
    model.supports_agent != Some(false)
        && model.is_chat_only != Some(true)
        && model.is_hidden != Some(true)
        && model.requires_data_retention != Some(true)
}

/// Apply available-model metadata to a catalog entry.
fn enrich(entry: &mut Model, rich: &AvailableModel, max_mode: bool) {
    let display = rich
        .client_display_name
        .as_deref()
        .and_then(nonempty)
        .or_else(|| rich.server_model_name.as_deref().and_then(nonempty));
    if entry.name.is_none() {
        entry.name = display.clone();
    }
    if entry.display_name.is_none() {
        entry.display_name = display;
    }
    if entry.reasoning.is_none() {
        entry.reasoning = rich.supports_thinking;
    }
    entry.context_window = if max_mode {
        rich.context_token_limit_for_max_mode
            .or(rich.context_token_limit)
    } else {
        rich.context_token_limit
    }
    .map(u64::from);
    if let Some(images) = rich.supports_images {
        entry.extra.insert("supports_images".into(), json!(images));
    }
}

/// Normalize usable identifiers and their matching rich metadata.
fn normalize(usable: Option<UsableResponse>, available: Option<AvailableResponse>) -> Vec<Model> {
    let mut result = BTreeMap::new();
    let rich = available.map_or_else(Vec::new, |response| response.models);
    if let Some(usable) = usable {
        for model in usable.models {
            let Some(id) = nonempty(&model.model_id) else {
                continue;
            };
            let matching = rich
                .iter()
                .find(|entry| rich_ids(entry).any(|candidate| candidate.trim() == id));
            if matching.is_some_and(|entry| !permitted(entry)) {
                continue;
            }
            let display =
                nonempty(&model.display_name).or_else(|| nonempty(&model.display_name_short));
            let mut entry = Model {
                id: id.clone(),
                object: Some("model".into()),
                owned_by: Some("cursor".into()),
                name: display.clone(),
                display_name: display,
                reasoning: model.thinking_details.as_ref().map(|_| true),
                ..Model::default()
            };
            if !model.aliases.is_empty() {
                entry.extra.insert("aliases".into(), json!(model.aliases));
            }
            if let Some(value) = model.max_mode {
                entry.extra.insert("max_mode".into(), json!(value));
            }
            if let Some(details) = matching {
                enrich(&mut entry, details, model.max_mode == Some(true));
            }
            result.entry(id).or_insert(entry);
        }
    } else {
        for model in rich.iter().filter(|model| permitted(model)) {
            let mut ids = HashSet::new();
            if model.variants.is_empty() {
                if let Some(id) = nonempty(&model.name) {
                    ids.insert(id);
                }
            } else {
                for variant in &model.variants {
                    if let Some(id) = variant
                        .legacy_slug
                        .as_deref()
                        .and_then(nonempty)
                        .or_else(|| variant.string_representation.as_deref().and_then(nonempty))
                    {
                        ids.insert(id);
                    }
                }
                if ids.is_empty()
                    && let Some(id) = nonempty(&model.name)
                {
                    ids.insert(id);
                }
            }
            for id in ids {
                let variant = model.variants.iter().find(|v| {
                    v.legacy_slug.as_deref() == Some(id.as_str())
                        || v.string_representation.as_deref() == Some(id.as_str())
                });
                let display = variant
                    .and_then(|v| v.display_name.as_deref())
                    .and_then(nonempty);
                let mut entry = Model {
                    id: id.clone(),
                    object: Some("model".into()),
                    owned_by: Some("cursor".into()),
                    name: display.clone(),
                    display_name: display,
                    ..Model::default()
                };
                enrich(
                    &mut entry,
                    model,
                    variant.and_then(|v| v.is_max_mode) == Some(true),
                );
                result.entry(id).or_insert(entry);
            }
        }
    }
    result.into_values().collect()
}

/// Deterministic mock-upstream discovery tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Bytes,
        http::{HeaderMap, StatusCode},
        routing::post,
    };

    /// Account credentials and protobuf metadata are sent and decoded.
    #[tokio::test]
    async fn discovers_account_models_and_enriches_metadata() {
        let app = Router::new()
            .route(
                USABLE_PATH,
                post(|headers: HeaderMap, body: Bytes| async move {
                    assert_eq!(headers["authorization"], "Bearer account-token");
                    assert_eq!(headers["content-type"], "application/proto");
                    assert!(body.is_empty());
                    UsableResponse {
                        models: vec![UsableModel {
                            model_id: "wire-id".into(),
                            display_name: "Display".into(),
                            ..Default::default()
                        }],
                    }
                    .encode_to_vec()
                }),
            )
            .route(
                AVAILABLE_PATH,
                post(|headers: HeaderMap, body: Bytes| async move {
                    assert_eq!(headers["authorization"], "Bearer account-token");
                    let request = AvailableRequest::decode(body).unwrap();
                    assert!(request.include_long_context_models);
                    assert_eq!(request.use_model_parameters, Some(true));
                    AvailableResponse {
                        models: vec![AvailableModel {
                            name: "base".into(),
                            legacy_slugs: vec!["wire-id".into()],
                            supports_agent: Some(true),
                            supports_thinking: Some(true),
                            supports_images: Some(true),
                            context_token_limit: Some(200_000),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }
                    .encode_to_vec()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let models = fetch_models(&reqwest::Client::new(), &base, "account-token")
            .await
            .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "wire-id");
        assert_eq!(models[0].reasoning, Some(true));
        assert_eq!(models[0].context_window, Some(200_000));
        assert_eq!(models[0].extra["supports_images"], true);
        server.abort();
    }

    /// Available models provide a fallback when the usable RPC fails.
    #[tokio::test]
    async fn falls_back_to_available_and_propagates_failures() {
        let app = Router::new()
            .route(USABLE_PATH, post(|| async { StatusCode::UNAUTHORIZED }))
            .route(
                AVAILABLE_PATH,
                post(|| async {
                    AvailableResponse {
                        models: vec![
                            AvailableModel {
                                name: "visible".into(),
                                ..Default::default()
                            },
                            AvailableModel {
                                name: "hidden".into(),
                                is_hidden: Some(true),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    }
                    .encode_to_vec()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let models = fetch_models(&reqwest::Client::new(), &base, "token")
            .await
            .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "visible");
        server.abort();
    }

    /// Retryable errors and successful empty catalogs remain distinct.
    #[tokio::test]
    async fn failed_calls_preserve_retryable_status_and_empty_catalog_is_valid() {
        let app = Router::new()
            .route(USABLE_PATH, post(|| async { StatusCode::UNAUTHORIZED }))
            .route(
                AVAILABLE_PATH,
                post(|| async { StatusCode::TOO_MANY_REQUESTS }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let error = fetch_models(&reqwest::Client::new(), &base, "token")
            .await
            .unwrap_err();
        assert_eq!(error.status, Some(StatusCode::UNAUTHORIZED));
        assert!(error.retryable());
        server.abort();

        let app = Router::new()
            .route(USABLE_PATH, post(|| async { Vec::<u8>::new() }))
            .route(
                AVAILABLE_PATH,
                post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert!(
            fetch_models(&reqwest::Client::new(), &base, "token")
                .await
                .unwrap()
                .is_empty()
        );
        server.abort();
    }
}

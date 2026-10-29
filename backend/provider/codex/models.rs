use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::openai::{ModelEntry, ModelListResponse};
use crate::provider::codex::auth::{CODEX_BASE_URL, CODEX_CLIENT_VERSION, CodexCredentials};
use crate::provider::common::errors::UpstreamError;
use crate::provider::registry::Provider;
use crate::server::AppState;

#[derive(Deserialize)]
struct CodexModelsResponse {
    #[serde(default)]
    models: Option<Vec<CodexModelEntry>>,
    #[serde(default)]
    data: Option<Vec<CodexModelEntry>>,
}

#[derive(Serialize, Deserialize)]
struct CodexModelEntry {
    slug: Option<String>,
    id: Option<String>,
    visibility: Option<String>,
    display_name: Option<String>,
    default_reasoning_level: Option<String>,
    supported_reasoning_levels: Option<Vec<CodexReasoningLevel>>,
    context_window: Option<u64>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Serialize, Deserialize)]
struct CodexReasoningLevel {
    effort: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

pub(crate) async fn fetch_codex_models(
    creds: &CodexCredentials,
) -> Result<ModelListResponse, UpstreamError> {
    Ok(fetch_models(creds, false).await?.0)
}

pub(crate) async fn fetch_catalog_pool(
    state: &AppState,
    provider: &Provider,
) -> Result<(ModelListResponse, Vec<Value>), String> {
    crate::provider::codex::routes::with_pool(state, provider, |creds| async move {
        fetch_models(&creds, true).await
    })
    .await
    .map_err(|e| e.message)
}

// Fetch and normalize one upstream response together so both views stay consistent.
#[allow(clippy::too_many_lines)]
async fn fetch_models(
    creds: &CodexCredentials,
    include_catalog: bool,
) -> Result<(ModelListResponse, Vec<Value>), UpstreamError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Failed to initialize Codex client")?;

    let url = format!("{CODEX_BASE_URL}/codex/models?client_version={CODEX_CLIENT_VERSION}");
    let mut req = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "omp")
        .header("version", CODEX_CLIENT_VERSION)
        .header("accept", "application/json");

    if let Some(account_id) = &creds.account_id {
        req = req.header("chatgpt-account-id", account_id);
    }
    if let Some(residency) = &creds.residency {
        req = req.header("x-openai-internal-codex-residency", residency);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Failed to request Codex models: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(UpstreamError {
            status: Some(status),
            message: format!("Codex models returned {status}: {body}"),
        });
    }

    let parsed: CodexModelsResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Codex models response: {e}"))?;

    let entries = parsed
        .models
        .or(parsed.data)
        .ok_or("Invalid Codex model list")?;
    let mut models = Vec::new();
    let mut catalog = Vec::new();

    for entry in entries {
        let mut raw = if include_catalog {
            Some(serde_json::to_value(&entry).map_err(|_| "Failed to serialize Codex model")?)
        } else {
            None
        };
        let slug = entry.slug.or(entry.id);
        let Some(model_id) = slug else { continue };

        if let Some(vis) = &entry.visibility
            && (vis.eq_ignore_ascii_case("hide") || vis.eq_ignore_ascii_case("hidden"))
        {
            continue;
        }
        let supported_efforts: Option<Vec<String>> = entry
            .supported_reasoning_levels
            .map(|levels| {
                levels
                    .into_iter()
                    .filter_map(|l| l.effort)
                    .filter(|e| !e.trim().is_empty())
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty());

        let has_reasoning = supported_efforts.is_some()
            || entry
                .default_reasoning_level
                .as_deref()
                .is_some_and(|l| l != "none");

        let default_effort = entry.default_reasoning_level.filter(|l| l != "none");

        let model = ModelEntry {
            id: model_id,
            object: "model".to_string(),
            created: 1_700_000_000,
            owned_by: "openai-codex".to_string(),
            name: entry.display_name.clone(),
            display_name: entry.display_name,
            reasoning: if has_reasoning { Some(true) } else { None },
            default_reasoning_effort: default_effort,
            supported_reasoning_efforts: supported_efforts,
            context_window: entry.context_window,
            max_output_tokens: None,
        };
        if let Some(raw) = &mut raw
            && let (Some(raw), Value::Object(normalized)) = (
                raw.as_object_mut(),
                serde_json::to_value(&model).map_err(|_| "Failed to serialize Codex model")?,
            )
        {
            raw.extend(normalized.into_iter().filter(|(_, value)| !value.is_null()));
        }
        if let Some(raw) = raw {
            catalog.push(raw);
        }
        models.push(model);
    }
    Ok((
        ModelListResponse {
            object: "list".to_string(),
            data: models,
        },
        catalog,
    ))
}

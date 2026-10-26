use serde::Deserialize;

use crate::codex::auth::{CODEX_BASE_URL, CODEX_CLIENT_VERSION, CodexAuthManager};
use crate::openai::{ModelEntry, ModelListResponse};

#[derive(Deserialize)]
struct CodexModelsResponse {
    #[serde(default)]
    models: Option<Vec<CodexModelEntry>>,
    #[serde(default)]
    data: Option<Vec<CodexModelEntry>>,
}

#[derive(Deserialize)]
struct CodexModelEntry {
    slug: Option<String>,
    id: Option<String>,
    visibility: Option<String>,
}

pub(crate) async fn fetch_codex_models(
    auth: &CodexAuthManager,
) -> Result<ModelListResponse, String> {
    let creds = auth.get_credentials().await?;
    let client = reqwest::Client::new();

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
        return Err(format!("Codex models returned {status}: {body}"));
    }

    let parsed: CodexModelsResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Codex models response: {e}"))?;

    let entries = parsed.models.or(parsed.data).unwrap_or_default();
    let mut models = Vec::new();

    for entry in entries {
        let slug = entry.slug.or(entry.id);
        let Some(model_id) = slug else { continue };

        if let Some(vis) = &entry.visibility
            && (vis.eq_ignore_ascii_case("hide") || vis.eq_ignore_ascii_case("hidden"))
        {
            continue;
        }

        models.push(ModelEntry {
            id: model_id,
            object: "model".to_string(),
            created: 1_700_000_000,
            owned_by: "openai-codex".to_string(),
        });
    }

    Ok(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}

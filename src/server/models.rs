use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::Value;
use tracing::warn;

use crate::openai::{ModelEntry, ModelListResponse};
use crate::server::AppState;

pub(crate) async fn list_models_handler(
    State(state): State<AppState>,
) -> Result<Json<ModelListResponse>, (StatusCode, Json<Value>)> {
    let creds = state.auth.get_credentials().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::server::error_body(
                &e,
                "server_error",
                Some("internal_error"),
            )),
        )
    })?;

    let models = state.client.fetch_models(&creds).await.unwrap_or_else(|e| {
        warn!("Failed to fetch live models from Antigravity: {e}; falling back to standard list");
        vec![
            (
                "claude-sonnet-5-5-medium".to_string(),
                crate::antigravity::AntigravityModelInfo {
                    display_name: Some("Claude Sonnet 5.5 (Medium)".to_string()),
                    supports_images: Some(true),
                    supports_thinking: Some(true),
                    max_tokens: Some(200_000),
                    max_output_tokens: Some(64_000),
                    disabled: None,
                },
            ),
            (
                "claude-sonnet-5-5-high".to_string(),
                crate::antigravity::AntigravityModelInfo {
                    display_name: Some("Claude Sonnet 5.5 (High)".to_string()),
                    supports_images: Some(true),
                    supports_thinking: Some(true),
                    max_tokens: Some(200_000),
                    max_output_tokens: Some(64_000),
                    disabled: None,
                },
            ),
            (
                "claude-opus-5-5-medium".to_string(),
                crate::antigravity::AntigravityModelInfo {
                    display_name: Some("Claude Opus 5.5 (Medium)".to_string()),
                    supports_images: Some(true),
                    supports_thinking: Some(true),
                    max_tokens: Some(200_000),
                    max_output_tokens: Some(64_000),
                    disabled: None,
                },
            ),
            (
                "gemini-3.8-flash-low".to_string(),
                crate::antigravity::AntigravityModelInfo {
                    display_name: Some("Gemini 3.8 Flash (Low)".to_string()),
                    supports_images: Some(true),
                    supports_thinking: Some(true),
                    max_tokens: Some(1_000_000),
                    max_output_tokens: Some(65_536),
                    disabled: None,
                },
            ),
            (
                "gemini-4-argon-low".to_string(),
                crate::antigravity::AntigravityModelInfo {
                    display_name: Some("Gemini 4 Argon (Low)".to_string()),
                    supports_images: Some(true),
                    supports_thinking: Some(true),
                    max_tokens: Some(1_000_000),
                    max_output_tokens: Some(65_535),
                    disabled: None,
                },
            ),
            (
                "gpt-oss-120b-medium".to_string(),
                crate::antigravity::AntigravityModelInfo {
                    display_name: Some("GPT-OSS 120B (Medium)".to_string()),
                    supports_images: Some(false),
                    supports_thinking: Some(true),
                    max_tokens: Some(128_000),
                    max_output_tokens: Some(16_384),
                    disabled: None,
                },
            ),
        ]
    });

    let mut data = Vec::new();

    for alias in crate::antigravity::MODEL_ALIASES {
        data.push(ModelEntry {
            id: alias.to_string(),
            object: "model".to_string(),
            created: 0,
            owned_by: "google-antigravity".to_string(),
        });
    }

    for (id, _) in models {
        if crate::antigravity::MODEL_ALIASES.contains(&id.as_str()) {
            continue;
        }
        data.push(ModelEntry {
            id,
            object: "model".to_string(),
            created: 0,
            owned_by: "google-antigravity".to_string(),
        });
    }

    Ok(Json(ModelListResponse {
        object: "list".to_string(),
        data,
    }))
}

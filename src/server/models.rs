use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::Value;
use tracing::warn;

use crate::openai::{ModelEntry, ModelListResponse};
use crate::server::AppState;

const FALLBACK_MODEL_IDS: &[&str] = &[
    "claude-sonnet-5-5-medium",
    "claude-sonnet-5-5-high",
    "claude-opus-5-5-medium",
    "gemini-3.8-flash-low",
    "gemini-4-argon-low",
    "gpt-oss-120b-medium",
];

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

    let model_ids: Vec<String> = match state.client.fetch_models(&creds).await {
        Ok(models) => models.into_iter().map(|(id, _)| id).collect(),
        Err(e) => {
            warn!(
                "Failed to fetch live models from Antigravity: {e}; falling back to standard list"
            );
            FALLBACK_MODEL_IDS.iter().map(ToString::to_string).collect()
        }
    };

    let mut data = Vec::new();

    for alias in crate::antigravity::MODEL_ALIASES {
        data.push(ModelEntry {
            id: alias.to_string(),
            object: "model".to_string(),
            created: 0,
            owned_by: "google-antigravity".to_string(),
        });
    }

    for id in model_ids {
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

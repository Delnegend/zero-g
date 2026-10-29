use super::{InferenceDiscoveryError, InferenceModelList, error, permissions};
use crate::provider::{catalog, registry};
use crate::server::AppState;
use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::future::join_all;
use serde_json::json;

/// Aggregate permitted model records, without exposing upstream errors.
#[utoipa::path(get, path = "/v1/models", operation_id = "listInferenceModels",
    params(("Authorization" = String, Header, description = "Bearer virtual key")),
    responses((status = 200, body = InferenceModelList),
        (status = 401, description = "Missing or invalid virtual key", body = Value),
        (status = 500, description = "Registry or permissions error", body = Value),
        (status = 502, description = "All provider discoveries failed", body = Value)))]
pub(super) async fn models(State(state): State<AppState>, request: Request) -> Response {
    let permissions = match permissions(&state, request.headers()).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let Ok(providers) = registry::list(&state.settings.database).await else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to load providers",
            "server_error",
            "internal_error",
        );
    };
    let providers: Vec<_> = providers
        .into_iter()
        .filter(|p| permissions.providers.is_empty() || permissions.providers.contains(&p.id))
        .collect();
    let results = join_all(providers.iter().map(|provider| async {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            catalog::discover_permitted(&state, provider, &permissions),
        )
        .await
    }))
    .await;
    let mut data = Vec::new();
    let mut errors = Vec::new();
    let mut succeeded = false;
    for (provider, result) in providers.iter().zip(results) {
        match result {
            Ok(Ok(models)) => {
                succeeded = true;
                for mut model in models {
                    model.id = format!("{}/{}", provider.id, model.id);
                    data.push(model);
                }
            }
            _ => errors.push(InferenceDiscoveryError {
                provider: provider.id.clone(),
                message: "Model discovery failed".into(),
            }),
        }
    }
    if !succeeded && !errors.is_empty() {
        let mut body = super::super::error_body(
            "Model discovery failed",
            "upstream_error",
            Some("discovery_failed"),
        );
        if let Some(object) = body.as_object_mut() {
            object.insert("errors".into(), json!(errors));
        }
        return (StatusCode::BAD_GATEWAY, Json(body)).into_response();
    }
    Json(InferenceModelList {
        object: "list".into(),
        data,
        errors,
    })
    .into_response()
}

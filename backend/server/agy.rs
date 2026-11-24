use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::Value;

use super::{AppState, observability::{CompressedBodies, ObservationLogEntry, insert, now}};
use crate::{database::transaction_with_retry, provider::common::request_identity::RequestIdentity, storage::RequestLog};

/// Maximum accepted observation size, including both captured bodies.
const BODY_LIMIT: usize = 20_000_000;

/// A single captured CLI step; unknown token counts are explicitly null.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    conversation_id: String,
    step_index: i64,
    model: String,
    input_tokens: Value,
    output_tokens: Value,
    request: Value,
    response: Value,
}

/// Strip common credential fields and the presented virtual key from captured JSON.
fn redact(value: &mut Value, token: &str) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let name = key.to_ascii_lowercase().replace('-', "_");
                if matches!(name.as_str(), "authorization" | "proxy_authorization" | "x_api_key" | "api_key" | "apikey" | "access_token" | "refresh_token" | "id_token" | "client_secret" | "password" | "cookie" | "set_cookie" | "credential" | "credentials") {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    redact(value, token);
                }
            }
        }
        Value::Array(items) => for item in items { redact(item, token); },
        Value::String(text) if text.contains(token) => *text = text.replace(token, "[REDACTED]"),
        _ => {}
    }
}

/// Return a nonnegative count, or unknown for explicit JSON null.
fn count(value: &Value) -> Result<Option<i64>, ()> {
    if value.is_null() { return Ok(None); }
    value.as_i64().filter(|n| *n >= 0).map(Some).ok_or(())
}

/// Store an authenticated observation, deduplicating retained logs per key and step.
async fn observe(State(state): State<AppState>, request: Request) -> Response {
    let Some(token) = super::settings::virtual_key(request.headers()).map(str::to_owned) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let identity = RequestIdentity::default();
    let permissions = match state.settings.permissions(&token, Some(&identity)).await {
        Ok(Some(permissions)) => permissions,
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // Provider-scoped keys do not grant access to the independent collector.
    if !permissions.providers.is_empty() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(bytes) = to_bytes(request.into_body(), BODY_LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(mut observation) = serde_json::from_slice::<Observation>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (Ok(input_tokens), Ok(output_tokens)) = (count(&observation.input_tokens), count(&observation.output_tokens)) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if observation.conversation_id.trim().is_empty() || observation.conversation_id.len() > 1024
        || observation.model.trim().is_empty() || observation.model.len() > 1024 || observation.step_index < 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if !permissions.allows_model(&observation.model) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let identity = identity.snapshot();
    let Some(key_id) = identity.virtual_key_id.as_deref() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    // Hash opaque identifiers so neither conversation IDs nor virtual keys appear in paths.
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(key_id.as_bytes());
    digest.update(&[0]);
    digest.update(observation.conversation_id.as_bytes());
    digest.update(&observation.step_index.to_be_bytes());
    let hash = digest.finish().as_ref().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let path = format!("/v1/agy/observations/{hash}");
    redact(&mut observation.request, &token);
    redact(&mut observation.response, &token);
    let log = ObservationLogEntry {
        id: 0, created_at: now(), provider: "agy-cli".into(),
        model: observation.model.replace(&token, "[REDACTED]"), path,
        status: 200, latency_ms: 0, input_tokens, output_tokens,
        virtual_key_id: identity.virtual_key_id, virtual_key_name: identity.virtual_key_name,
        credential_id: None, credential_name: None,
        request_body: Some(observation.request.to_string()), response_body: Some(observation.response.to_string()),
    };
    let Ok(bodies) = CompressedBodies::new(&log) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let result = transaction_with_retry!(state.database, |mut tx| {
        let existing = RequestLog::filter(RequestLog::fields().path().eq(log.path.clone()))
            .first().exec(&mut tx).await?;
        if existing.is_none() { insert(&mut tx, &log, &bodies).await?; }
        tx.commit().await?;
        Ok(())
    });
    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Observation route is outside inference capture and authenticates before reading the body.
pub(super) fn router(state: &AppState) -> Router<AppState> {
    Router::new().route("/v1/agy/observations", post(observe)).with_state(state.clone())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::ProviderRecord;
    use axum::{body::Body, http::{Request, header}};
    use serde_json::json;
    use tower::ServiceExt;

    /// Authentication precedes capture; valid observations persist once with nullable usage.
    #[tokio::test]
    async fn authenticated_observations_are_validated_and_deduplicated() {
        let database = crate::database::Database::open(":memory:").await.unwrap();
        let settings = super::super::settings::SettingsStore::new(
            database.clone(), "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        ).await.unwrap();
        let key_response = Router::new()
            .route("/keys", post(super::super::settings::create_key))
            .with_state(settings.clone())
            .oneshot(Request::builder().method("POST").uri("/keys")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name":"collector"}).to_string())).unwrap())
            .await.unwrap();
        assert_eq!(key_response.status(), StatusCode::CREATED);
        let key_body = to_bytes(key_response.into_body(), 1024 * 1024).await.unwrap();
        let key_json: Value = serde_json::from_slice(&key_body).unwrap();
        let key = key_json["key"].as_str().unwrap();
        toasty::create!(ProviderRecord {
            id: "codex".to_string(), name: "Codex".to_string(), kind: "codex".to_string(),
            base_url: String::new(), keyless: false, deleted: false
        }).exec(&mut (*database.db).clone()).await.unwrap();
        let restricted_response = Router::new()
            .route("/keys", post(super::super::settings::create_key))
            .with_state(settings.clone())
            .oneshot(Request::builder().method("POST").uri("/keys")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name":"restricted", "providers":["codex"]}).to_string())).unwrap())
            .await.unwrap();
        assert_eq!(restricted_response.status(), StatusCode::CREATED);
        let restricted_body = to_bytes(restricted_response.into_body(), 1024 * 1024).await.unwrap();
        let restricted_json: Value = serde_json::from_slice(&restricted_body).unwrap();
        let restricted_key = restricted_json["key"].as_str().unwrap();
        let state = AppState {
            providers: crate::provider::Providers::new(),
            settings,
            database: database.clone(),
            dashboard_auth: super::super::dashboard_auth_config::test_auth().await,
        };
        let app = super::super::create_router(state);
        let observation = json!({"conversation_id":"conversation", "step_index":1,
            "model":"gemini", "input_tokens":null, "output_tokens":12,
            "request":{"authorization":"secret", "message":"hello"},
            "response":{"message":"world"}});
        let send = |token: &str, value: &Value| {
            let request = Request::builder().method("POST").uri("/v1/agy/observations")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(value.to_string())).unwrap();
            app.clone().oneshot(request)
        };
        assert_eq!(send("invalid", &observation).await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(send(restricted_key, &observation).await.unwrap().status(), StatusCode::FORBIDDEN);
        for invalid in [json!(-1), json!(1.5), json!("3")] {
            let mut body = observation.clone();
            body["input_tokens"] = invalid;
            assert_eq!(send(key, &body).await.unwrap().status(), StatusCode::BAD_REQUEST);
        }
        assert_eq!(send(key, &observation).await.unwrap().status(), StatusCode::OK);
        assert_eq!(send(key, &observation).await.unwrap().status(), StatusCode::OK);
        let rows = transaction_with_retry!(database, |mut tx| {
            RequestLog::all().exec(&mut tx).await
        }).unwrap();
        assert_eq!(rows.len(), 1);
        let row = rows.first().unwrap();
        assert_eq!(row.model, "gemini");
        assert!(row.path.starts_with("/v1/agy/observations/"));
        assert_eq!(row.input_tokens, None);
        assert_eq!(row.output_tokens, Some(12));
        let response = super::super::observability::log_detail(
            State(database), axum::extract::Path(row.id),
        ).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap();
        let request: Value = serde_json::from_str(body["request_body"].as_str().unwrap()).unwrap();
        assert_eq!(request["authorization"], "[REDACTED]");
        assert_eq!(request["message"], "hello");
    }
}

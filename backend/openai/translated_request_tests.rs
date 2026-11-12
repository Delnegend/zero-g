use super::*;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::post,
};
use serde_json::json;
use tower::ServiceExt;

async fn chat(Json(req): Json<ChatCompletionRequest>) -> Json<ChatCompletionRequest> {
    Json(req)
}

async fn responses(
    Json(req): Json<crate::responses::ResponseCreateRequest>,
) -> Json<crate::responses::ResponseCreateRequest> {
    Json(req)
}

fn app(codex: bool) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/responses", post(responses))
        .layer(axum::middleware::from_fn_with_state(
            codex,
            validate_translated_request,
        ))
}

async fn send(app: Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn translated_endpoints_reject_constraints_before_deserialization() {
    for codex in [false, true] {
        for (path, base) in [
            (
                "/v1/chat/completions",
                json!({"model":"m","messages":[{"role":"user","content":"hello"}]}),
            ),
            ("/v1/responses", json!({"model":"m","input":"hello"})),
        ] {
            for (field, value) in [
                ("response_format", json!({"type":"json_object"})),
                ("response_format", json!({"type":"text","extra":true})),
                ("stop", json!(["end"])),
                ("n", json!(2)),
                ("parallel_tool_calls", json!(false)),
                ("previous_response_id", json!("resp_1")),
            ] {
                let mut body = base.clone();
                body[field] = value;
                let (status, error) = send(app(codex), path, body).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{field}");
                assert_eq!(error["error"]["code"], "unsupported_parameter");
            }
            for format in [
                json!({"type":"json_schema","name":"schema","schema":{"type":"object"}}),
                json!({"type":"json_object"}),
                json!({"type":"text","extra":true}),
            ] {
                let mut body = base.clone();
                body["text"] = json!({"format":format});
                let (status, error) = send(app(codex), path, body).await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(error["error"]["code"], "unsupported_parameter");
            }
            for tool in [
                json!({"type":"function","function":{"name":"f","strict":true}}),
                json!({"type":"function","name":"f","strict":true}),
                json!({"type":"function","function":{"name":"f","strict":"false"}}),
                json!({"type":"function","name":"f","strict":"false"}),
            ] {
                let mut body = base.clone();
                body["tools"] = json!([tool]);
                assert_eq!(
                    send(app(codex), path, body).await.0,
                    StatusCode::BAD_REQUEST
                );
            }
            let mut body = base.clone();
            body["tools"] = json!([{"type":"function","function":{"name":"f","strict":false}}]);
            let (status, result) = send(app(codex), path, body).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(result["tools"][0]["function"]["name"], "f");
            if path == "/v1/responses" {
                let mut body = base.clone();
                body["tools"] = json!([{"type":"function","name":"f","strict":false}]);
                let (status, result) = send(app(codex), path, body).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(result["tools"][0]["function"]["name"], "f");
            }
            let mut body = base;
            body["stop"] = Value::Null;
            body["n"] = json!(1);
            body["response_format"] = json!({"type":"text"});
            body["parallel_tool_calls"] = json!(true);
            body["text"] = json!({"format":{"type":"text"}});
            let (status, result) = send(app(codex), path, body).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(result["model"], "m");
        }
    }
}

#[tokio::test]
async fn codex_rejects_media_in_tool_results_without_affecting_text() {
    for input in [
        json!({"model":"m","messages":[{"role":"tool","tool_call_id":"c","content":[{"type":"image_url","image_url":{"url":"https://example.com/a.png"}}]}]}),
        json!({"model":"m","input":[{"role":"tool","tool_call_id":"c","content":[{"type":"input_image","image_url":"https://example.com/a.png"}]}]}),
        json!({"model":"m","input":[{"type":"function_call_output","call_id":"c","output":[{"type":"input_image","image_url":"https://example.com/a.png"}]}]}),
    ] {
        let path = if input.get("messages").is_some() {
            "/v1/chat/completions"
        } else {
            "/v1/responses"
        };
        assert_eq!(
            send(app(true), path, input).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    for (path, input) in [
        (
            "/v1/chat/completions",
            json!({"model":"m","messages":[{"role":"tool","tool_call_id":"c","content":[{"type":"text","text":"ok"}]}]}),
        ),
        (
            "/v1/responses",
            json!({"model":"m","input":[{"type":"function_call_output","call_id":"c","output":"ok"}]}),
        ),
    ] {
        assert_eq!(send(app(true), path, input).await.0, StatusCode::OK);
    }
}

#[tokio::test]
async fn codex_preserves_reasoning_and_user_image_inputs() {
    let body = json!({"model":"m","input":[
        {"type":"reasoning","id":"r","summary":[],"encrypted_content":"opaque"},
        {"type":"message","role":"user","content":[{"type":"input_image","image_url":"https://example.com/a.png"}]}
    ]});
    let (status, result) = send(app(true), "/v1/responses", body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["input"], body["input"]);
    assert_eq!(
        send(app(false), "/v1/responses", body).await.0,
        StatusCode::BAD_REQUEST
    );
}

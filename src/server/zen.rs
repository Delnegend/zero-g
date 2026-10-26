use axum::response::{Json, Response};
use serde_json::Value;

use crate::openai::ChatCompletionRequest;

pub(crate) async fn models_handler() -> Json<Value> {
    Json(crate::zen::models_json())
}

pub(crate) async fn chat_handler(Json(req): Json<ChatCompletionRequest>) -> Response {
    crate::zen::handle_chat(req).await
}

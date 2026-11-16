use serde_json::Value;

use super::downstream::CodexClient;
use crate::provider::codex::auth::{CODEX_CLIENT_VERSION, Credentials};
use crate::provider::common::errors::UpstreamError;

impl CodexClient {
    /// Returns the configured Codex upstream base URL.
    pub(in crate::provider::codex) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Overrides the upstream base URL for integration tests.
    #[cfg(test)]
    pub(in crate::provider::codex) fn set_base_url_for_test(&mut self, url: String) {
        self.base_url = url;
    }
    /// Sends an authenticated streaming request to the Codex Responses endpoint.
    pub(in crate::provider::codex) async fn send_request(
        &self,
        credential_id: Option<&str>,
        creds: &Credentials,
        mut payload: Value,
        session_id: &str,
    ) -> Result<reqwest::Response, UpstreamError> {
        let url = format!("{}/codex/responses", self.base_url);
        let model_slug = payload
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("gpt-5.5");

        let mut builder = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", creds.access_token))
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "omp")
            .header("version", CODEX_CLIENT_VERSION)
            .header("x-codex-routing-hint", format!("model={model_slug}"))
            .header("accept", "text/event-stream")
            .header("content-type", "application/json");

        if let Some(account_id) = &creds.account_id {
            builder = builder.header("chatgpt-account-id", account_id);
        }
        if let Some(residency) = &creds.residency {
            builder = builder.header("x-openai-internal-codex-residency", residency);
        }

        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "prompt_cache_key".to_string(),
                Value::String(session_id.to_owned()),
            );
        }
        builder = builder
            .header("session_id", session_id)
            .header("conversation_id", session_id)
            .header("x-client-request-id", session_id);

        let resp = builder
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Failed to send Codex request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            if let Some(credential_id) = credential_id {
                self.model_cache
                    .invalidate_on_auth_error(credential_id, Some(status))
                    .await;
            }
            let headers = Box::new(resp.headers().clone());
            let body = resp.text().await.unwrap_or_default();
            return Err(UpstreamError {
                status: Some(status),
                message: format!("Codex upstream returned {status}: {body}"),
                headers,
            });
        }
        Ok(resp)
    }
}

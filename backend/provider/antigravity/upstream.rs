//! Upstream generation request transport.
use reqwest::Response;
use serde_json::Value;

use super::AntigravityClient;
use super::auth::{ANTIGRAVITY_USER_AGENT, Credentials};
use super::models::CLAUDE_THINKING_BETA_HEADER;
use crate::provider::common::errors::UpstreamError;

impl AntigravityClient {
    /// Send a streaming generation request, falling back on upstream failures.
    pub(super) async fn stream_request(
        &self,
        credential_id: Option<&str>,
        creds: &Credentials,
        payload: &Value,
        is_claude: bool,
    ) -> Result<Response, UpstreamError> {
        #[cfg(test)]
        let endpoints = self.endpoints();
        #[cfg(not(test))]
        let endpoints = Self::endpoints();
        let mut last_err = UpstreamError::from("No upstream endpoint available".to_string());

        for endpoint in &endpoints {
            let url = format!("{endpoint}/v1internal:streamGenerateContent?alt=sse");
            let mut req_builder = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", creds.access_token))
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .header("User-Agent", ANTIGRAVITY_USER_AGENT);

            if is_claude {
                req_builder = req_builder.header("anthropic-beta", CLAUDE_THINKING_BETA_HEADER);
            }

            match req_builder.json(payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    return Ok(resp);
                }
                Ok(resp) => {
                    let status = resp.status();
                    if let Some(credential_id) = credential_id {
                        self.model_cache
                            .invalidate_on_auth_error(credential_id, Some(status))
                            .await;
                    }
                    let headers = Box::new(resp.headers().clone());
                    let body = resp.text().await.unwrap_or_default();
                    last_err = UpstreamError {
                        status: Some(status),
                        message: format!("Antigravity error ({status}): {body}"),
                        headers,
                    };
                    if status.is_server_error() {
                        continue;
                    }
                    return Err(last_err);
                }
                Err(e) => {
                    last_err = UpstreamError::from(format!("Request failed to {endpoint}: {e}"));
                }
            }
        }

        Err(last_err)
    }
}

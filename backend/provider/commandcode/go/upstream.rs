use axum::body::Bytes;
use axum::http::header;

use super::CLI_VERSION;

/// Send one generate request with the CLI's identifying headers.
pub(super) async fn send(
    root: &str,
    key: &str,
    thread: &str,
    body: Bytes,
) -> Result<reqwest::Response, &'static str> {
    crate::util::HTTP_CLIENT
        .post(format!("{}/alpha/generate", root.trim_end_matches('/')))
        .bearer_auth(key)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, "cli")
        .header("x-command-code-version", CLI_VERSION)
        .header("x-cli-environment", "production")
        .header("x-project-slug", "project")
        .header("x-taste-learning", "false")
        .header("x-session-id", thread)
        .body(body)
        .send()
        .await
        // Reqwest errors can contain URLs; never expose them.
        .map_err(|_| "Failed to contact Command Code")
}

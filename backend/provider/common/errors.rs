use reqwest::StatusCode;

#[derive(Debug)]
pub(crate) struct UpstreamError {
    pub(crate) status: Option<StatusCode>,
    pub(crate) message: String,
}

impl UpstreamError {
    pub(crate) fn retryable(&self) -> bool {
        retryable_status(self.status)
    }
}

pub(crate) fn retryable_status(status: Option<StatusCode>) -> bool {
    matches!(
        status,
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS)
    )
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for UpstreamError {
    fn from(message: String) -> Self {
        Self {
            status: None,
            message,
        }
    }
}

impl From<&str> for UpstreamError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

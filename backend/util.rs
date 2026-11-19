use std::fmt::Write;
use std::sync::LazyLock;
use std::time::Duration;

/// Shared HTTP client for authenticated upstream requests that never follows redirects.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(10)
        .tcp_nodelay(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// Shared HTTP client for public metadata and discovery endpoints that permit redirects.
static HTTP_REDIRECT_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(10)
        .tcp_nodelay(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// Returns the shared no-redirect HTTP client for authenticated upstream requests.
pub(crate) fn http_client() -> &'static reqwest::Client {
    &HTTP_CLIENT
}

/// Returns the shared redirect-allowing HTTP client for public endpoints.
pub(crate) fn http_redirect_client() -> &'static reqwest::Client {
    &HTTP_REDIRECT_CLIENT
}

/// Percent-encodes UTF-8 bytes, leaving RFC 3986 unreserved characters intact.
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// URL encoding coverage.
#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes spaces and reserved characters while preserving unreserved ones.
    #[test]
    fn test_urlencode() {
        assert_eq!(urlencode("hello world"), "hello%20world");
        assert_eq!(urlencode("a/b?c=d&e=f"), "a%2Fb%3Fc%3Dd%26e%3Df");
        assert_eq!(urlencode("safe-text_123.~"), "safe-text_123.~");
    }
}

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

pub(crate) async fn auth_middleware(
    State(expected_token): State<Option<String>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    if req.method() == axum::http::Method::OPTIONS {
        return next.run(req).await;
    }

    if let Some(expected) = expected_token.as_deref()
        && !verify_bearer(req.headers(), expected)
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "Incorrect API key provided.",
                    "type": "invalid_request_error",
                    "param": null,
                    "code": "invalid_api_key"
                }
            })),
        )
            .into_response();
    }

    next.run(req).await
}

pub(crate) fn verify_bearer(headers: &HeaderMap, expected: &str) -> bool {
    let Some(auth_val) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(auth_str) = auth_val.to_str() else {
        return false;
    };
    let Some(token) = auth_str
        .strip_prefix("Bearer ")
        .or_else(|| auth_str.strip_prefix("bearer "))
    else {
        return false;
    };
    constant_time_eq(token.trim().as_bytes(), expected.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn test_verify_bearer_success() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token-123"),
        );
        assert!(verify_bearer(&headers, "secret-token-123"));
    }

    #[test]
    fn test_verify_bearer_case_insensitive_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("bearer secret-token-123"),
        );
        assert!(verify_bearer(&headers, "secret-token-123"));
    }

    #[test]
    fn test_verify_bearer_missing_header() {
        let headers = HeaderMap::new();
        assert!(!verify_bearer(&headers, "secret-token-123"));
    }

    #[test]
    fn test_verify_bearer_wrong_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token"),
        );
        assert!(!verify_bearer(&headers, "secret-token-123"));
    }

    #[test]
    fn test_verify_bearer_non_bearer_format() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Basic secret-token-123"),
        );
        assert!(!verify_bearer(&headers, "secret-token-123"));
    }
}

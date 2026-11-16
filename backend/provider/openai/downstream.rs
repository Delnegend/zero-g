use axum::body::Body;
use axum::response::Response;

/// Forward an upstream response to the caller.
pub(crate) fn forward(response: reqwest::Response) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let mut result = Response::new(Body::from_stream(response.bytes_stream()));
    *result.status_mut() = status;
    for (name, value) in &headers {
        if !matches!(
            name.as_str(),
            "connection"
                | "transfer-encoding"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "upgrade"
        ) {
            result.headers_mut().append(name, value.clone());
        }
    }
    result
}

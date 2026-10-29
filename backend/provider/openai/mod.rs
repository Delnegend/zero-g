/// HTTP routes for OpenAI-compatible providers.
pub(crate) mod routes;

use std::sync::LazyLock;

use crate::provider::registry::Provider;
use crate::server::settings::SettingsStore;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, header};
use axum::response::Response;
use futures_util::StreamExt;
use serde_json::Value;

/// OpenAI-compatible provider backend.
pub(crate) struct Backend;

/// Provider-specific model discovery endpoint.
pub(crate) trait ModelEndpoint {
    /// Request the upstream model list using the supplied credential.
    async fn request(provider: &Provider, key: &str) -> Result<reqwest::Response, &'static str>;
}

impl ModelEndpoint for Backend {
    async fn request(provider: &Provider, key: &str) -> Result<reqwest::Response, &'static str> {
        send(
            &provider.base_url,
            key,
            "models",
            None,
            Method::GET,
            &HeaderMap::new(),
            None,
        )
        .await
    }
}

/// Maximum accepted model discovery response size.
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// Discover models using the provider's available credentials.
pub(crate) async fn discover<E: ModelEndpoint>(
    store: &SettingsStore,
    provider: &Provider,
) -> Result<Vec<Value>, String> {
    let credentials = crate::provider::credentials::list(store, &provider.id).await?;
    let mut response = None;
    for credential in credentials {
        let key = store
            .secret_get(&format!("credential.{}.api_key", credential.id))
            .await
            .map_err(|_| "Failed to load provider credentials")?
            .ok_or("Provider credential is not connected")?;
        let current = E::request(provider, &key).await.map_err(str::to_owned)?;
        let status = current.status();
        response = Some(current);
        if !matches!(status.as_u16(), 401 | 403 | 429) {
            break;
        }
    }
    let response = response.ok_or("Provider has no connected credentials")?;
    if !response.status().is_success() {
        return Err(format!(
            "Provider model discovery returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Failed to read provider model list")?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("Provider model list is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid provider model list")?;
    let models = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Invalid provider model list")?;
    Ok(models
        .iter()
        .filter(|model| {
            model
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
        })
        .cloned()
        .collect())
}
/// Shared client used for OpenAI-compatible upstream requests.
static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> = LazyLock::new(|| {
    reqwest::Client::builder()
        // Never send the upstream credential to a redirect target.
        .redirect(reqwest::redirect::Policy::none())
        .build()
});

/// Send an authenticated OpenAI-compatible upstream request.
pub(crate) async fn send(
    base: &str,
    key: &str,
    path: &str,
    query: Option<&str>,
    method: Method,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Result<reqwest::Response, &'static str> {
    let mut url = format!("{}/{}", base.trim_end_matches('/'), path);
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    let client = CLIENT
        .as_ref()
        .map_err(|_| "Failed to initialize OpenAI-compatible provider")?;
    let mut request = client.request(method, url);
    for name in [header::ACCEPT, header::CONTENT_TYPE] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value);
        }
    }
    if !key.is_empty() {
        request = request.bearer_auth(key);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    // Reqwest errors can contain URLs, which may contain credentials.
    request
        .send()
        .await
        .map_err(|_| "Failed to contact OpenAI-compatible provider")
}

/// Forward an upstream response to the caller.
pub(crate) fn forward(response: reqwest::Response) -> Response {
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let mut result = Response::new(Body::from_stream(response.bytes_stream()));
    *result.status_mut() = status;
    if let Some(value) = content_type {
        result.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    result
}

impl super::common::Provider for Backend {
    const DESCRIPTOR: super::common::ProviderDescriptor = super::common::ProviderDescriptor {
        value: "openai",
        label: "OpenAI-compatible",
        oauth: false,
        api_key: true,
        fixed_id: None,
        fixed_name: None,
        keyless: true,
        default_base_url: None,
        legacy: super::common::LegacyPolicy {
            key: Some(""),
            same_base: false,
        },
    };
    fn router(settings: &SettingsStore) -> axum::Router<crate::server::AppState> {
        routes::router(settings)
    }
    async fn discover(
        state: &crate::server::AppState,
        provider: &Provider,
    ) -> Result<Vec<Value>, String> {
        discover::<Self>(&state.settings, provider).await
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::{Request, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::any;
    use futures_util::StreamExt;
    use std::sync::Arc;
    use tokio::sync::{Mutex, mpsc};

    type StreamReceiver =
        Arc<Mutex<Option<mpsc::UnboundedReceiver<Result<Bytes, std::io::Error>>>>>;

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn forwards_protocol_auth_errors_and_streams() {
        let (seen_tx, mut seen_rx) = mpsc::unbounded_channel();
        let (stream_tx, stream_rx) = mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();
        let stream_rx = Arc::new(Mutex::new(Some(stream_rx)));
        let app = Router::new()
            .route(
                "/{*path}",
                any(
                    move |State(stream_rx): State<StreamReceiver>, req: Request| {
                        let seen_tx = seen_tx.clone();
                        async move {
                            let (parts, body) = req.into_parts();
                            let body = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                            seen_tx
                                .send((parts.uri.to_string(), parts.headers, body))
                                .unwrap();
                            if parts.uri.path().ends_with("/responses") {
                                let stream = stream_rx.lock().await.take().unwrap();
                                (
                                    StatusCode::OK,
                                    [(header::CONTENT_TYPE, "text/event-stream")],
                                    Body::from_stream(
                                        tokio_stream::wrappers::UnboundedReceiverStream::new(
                                            stream,
                                        ),
                                    ),
                                )
                                    .into_response()
                            } else {
                                (
                                    StatusCode::TOO_MANY_REQUESTS,
                                    [(header::CONTENT_TYPE, "application/json")],
                                    "{\"error\":\"limited\"}",
                                )
                                    .into_response()
                            }
                        }
                    },
                ),
            )
            .with_state(stream_rx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer gateway-key".parse().unwrap());
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        let response = send(
            &base,
            "upstream-key",
            "chat/completions",
            Some("x=1"),
            Method::POST,
            &headers,
            Some(Bytes::from_static(b"{\"model\":\"m\"}")),
        )
        .await
        .unwrap();
        let response = forward(response);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
            "{\"error\":\"limited\"}"
        );
        let (uri, sent_headers, body) = seen_rx.recv().await.unwrap();
        assert_eq!(uri, "/v1/chat/completions?x=1");
        assert_eq!(
            sent_headers.get(header::AUTHORIZATION).unwrap(),
            "Bearer upstream-key"
        );
        assert_eq!(body, "{\"model\":\"m\"}");

        let response = send(&base, "", "models", None, Method::GET, &headers, None)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let (uri, sent_headers, body) = seen_rx.recv().await.unwrap();
        assert_eq!(uri, "/v1/models");
        assert!(!sent_headers.contains_key(header::AUTHORIZATION));
        assert!(body.is_empty());

        stream_tx
            .send(Ok(Bytes::from_static(b"data: first\n\n")))
            .unwrap();
        let response = send(
            &base,
            "",
            "responses",
            None,
            Method::POST,
            &headers,
            Some(Bytes::from_static(b"{}")),
        )
        .await
        .unwrap();
        let response = forward(response);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        let (_, sent_headers, _) = seen_rx.recv().await.unwrap();
        assert!(!sent_headers.contains_key(header::AUTHORIZATION));
        let mut stream = response.into_body().into_data_stream();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            "data: first\n\n"
        );
        stream_tx
            .send(Ok(Bytes::from_static(b"data: second\n\n")))
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "data: second\n\n");
        server.abort();
    }
}

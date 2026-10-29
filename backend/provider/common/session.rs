use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::hmac;

/// Opaque upstream affinity identity attached after virtual-key authentication.
#[derive(Clone)]
pub(crate) struct RequestSession {
    /// Session identity scoped to the authenticated virtual key and provider.
    pub(crate) id: String,
}

impl RequestSession {
    /// Selects the first present header in precedence order. A duplicate or invalid
    /// value disables affinity rather than falling back to another header. Values
    /// must contain 1–128 visible ASCII bytes with no whitespace.
    pub(crate) fn new(headers: &HeaderMap, token: &str, provider: &str) -> Self {
        let names = [
            "x-bf-eh-session_id",
            "session_id",
            "session-id",
            "x-session-id",
            "x-opencode-session",
            "conversation_id",
            "conversation-id",
        ];
        let incoming = names
            .iter()
            .find_map(|name| {
                let mut values = headers.get_all(*name).iter();
                values.next().map(|value| {
                    if values.next().is_some() {
                        return None;
                    }
                    let bytes = value.as_bytes();
                    if bytes.is_empty()
                        || bytes.len() > 128
                        || !bytes.iter().all(u8::is_ascii_graphic)
                    {
                        return None;
                    }
                    Some(bytes)
                })
            })
            .flatten();
        let id = incoming.map_or_else(
            || uuid::Uuid::new_v4().to_string(),
            |session| {
                let key = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
                let mut context = hmac::Context::with_key(&key);
                context.update(b"zero-g-session-v1\0");
                context.update(provider.as_bytes());
                context.update(b"\0");
                context.update(session);
                URL_SAFE_NO_PAD.encode(context.sign().as_ref())
            },
        );
        Self { id }
    }
}

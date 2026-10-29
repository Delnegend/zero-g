pub(crate) mod auth;
pub(crate) mod chat;
mod client;
pub(crate) mod login;
mod models;
mod payload;
pub(crate) mod quota;
pub(crate) mod responses;
pub(crate) mod routes;
pub(crate) mod sse;
mod stream;

pub(crate) use auth::{AuthManager, Credentials};
pub(crate) use models::AntigravityModelInfo;
use stream::map_finish_reason;
pub(crate) use stream::{AntigravityEvent, spawn_event_stream};

pub(crate) struct AntigravityClient {
    client: reqwest::Client,
}

impl AntigravityClient {
    pub(crate) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

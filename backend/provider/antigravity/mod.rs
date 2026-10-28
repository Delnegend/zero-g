pub(crate) mod auth;
pub(crate) mod chat;
mod client;
pub(crate) mod login;
mod models;
mod payload;
pub(crate) mod responses;
mod stream;

pub(crate) use auth::{AuthManager, Credentials};
pub(crate) use models::AntigravityModelInfo;
pub(crate) use stream::{AntigravityEvent, map_finish_reason, spawn_event_stream};

pub(crate) struct AntigravityClient {
    pub(crate) client: reqwest::Client,
}

impl AntigravityClient {
    pub(crate) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

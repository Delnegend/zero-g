mod client;
mod models;
mod payload;
mod stream;

pub(crate) use models::MODEL_ALIASES;
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

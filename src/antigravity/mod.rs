mod client;
mod models;
mod payload;

pub(crate) use models::*;

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

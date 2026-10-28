#[derive(Debug, toasty::Model)]
#[table = "request_logs"]
pub(crate) struct RequestLog {
    #[key]
    #[auto]
    pub id: i64,
    #[index]
    pub created_at: i64,
    pub provider: String,
    pub model: String,
    pub path: String,
    pub status: i64,
    pub latency_ms: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub request_body: String,
    pub response_body: String,
}

#[derive(Debug, toasty::Model)]
#[table = "settings"]
pub(crate) struct Setting {
    #[key]
    pub name: String,
    pub value: String,
}

#[derive(Debug, toasty::Model)]
#[table = "secrets"]
pub(crate) struct Secret {
    #[key]
    pub name: String,
    pub value: Vec<u8>,
}

#[derive(Debug, toasty::Model)]
#[table = "virtual_keys"]
pub(crate) struct VirtualKey {
    #[key]
    pub id: String,
    pub name: String,
    #[unique]
    pub key_hash: Vec<u8>,
    pub providers: String,
    pub models: String,
    pub created_at: i64,
}

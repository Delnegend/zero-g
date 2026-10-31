/// Persisted inference request and response metadata.
#[derive(Debug, toasty::Model)]
#[table = "request_logs"]
pub(crate) struct RequestLog {
    /// Auto-generated log identifier.
    #[key]
    #[auto]
    pub id: i64,
    /// Request creation timestamp.
    #[index]
    pub created_at: i64,
    /// Provider serving the request.
    pub provider: String,
    /// Requested model identifier.
    pub model: String,
    /// Request route.
    pub path: String,
    /// HTTP response status.
    pub status: i64,
    /// Request duration in milliseconds.
    pub latency_ms: i64,
    /// Reported input token count, when available.
    pub input_tokens: Option<i64>,
    /// Reported output token count, when available.
    pub output_tokens: Option<i64>,
    /// Stored request body.
    pub request_body: String,
    /// Stored response body.
    pub response_body: String,
    /// Authenticated virtual key identity at request time.
    pub virtual_key_id: Option<String>,
    /// Authenticated virtual key name at request time.
    pub virtual_key_name: Option<String>,
    /// Successfully selected provider credential identity.
    pub credential_id: Option<String>,
    /// Successfully selected provider credential name at request time.
    pub credential_name: Option<String>,
}

/// Named application setting.
#[derive(Debug, toasty::Model)]
#[table = "settings"]
pub(crate) struct Setting {
    /// Setting key.
    #[key]
    pub name: String,
    /// Serialized setting value.
    pub value: String,
}

/// Encrypted secret stored under a named key.
#[derive(Debug, toasty::Model)]
#[table = "secrets"]
pub(crate) struct Secret {
    /// Secret key.
    #[key]
    pub name: String,
    /// Encrypted secret bytes.
    pub value: Vec<u8>,
}

/// Virtual API key and its access restrictions.
#[derive(Debug, toasty::Model)]
#[table = "virtual_keys"]
pub(crate) struct VirtualKey {
    /// Virtual key identifier.
    #[key]
    pub id: String,
    /// Human-readable key name.
    pub name: String,
    /// Unique hash used to authenticate the key.
    #[unique]
    pub key_hash: Vec<u8>,
    /// Serialized provider restrictions.
    pub providers: String,
    /// Serialized model restrictions.
    pub models: String,
    /// Key creation timestamp.
    pub created_at: i64,
}

/// Persisted provider configuration.
#[derive(Debug, toasty::Model)]
#[table = "providers"]
pub(crate) struct ProviderRecord {
    /// Provider identifier.
    #[key]
    pub id: String,
    /// Human-readable provider name.
    pub name: String,
    /// Provider implementation kind.
    pub kind: String,
    /// Upstream base URL.
    pub base_url: String,
    /// Whether the provider has been deleted.
    pub deleted: bool,
}

/// Credential selection metadata for a provider.
#[derive(Debug, toasty::Model)]
#[table = "provider_credentials"]
pub(crate) struct ProviderCredential {
    /// Credential identifier.
    #[key]
    pub id: String,
    /// Identifier of the owning provider.
    #[index]
    pub provider_id: String,
    /// Human-readable credential name.
    pub name: String,
    /// Relative selection weight.
    pub weight: i64,
    /// Whether the credential can be selected.
    pub enabled: bool,
}

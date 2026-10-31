use serde_json::Value;

/// Provider-supplied model details for the dashboard catalog.
#[derive(Default)]
pub(crate) struct CatalogInfo {
    /// Maximum context size, when supplied by the provider.
    pub(crate) context_window: Option<u64>,
    /// Maximum output tokens, when supplied by the provider.
    pub(crate) max_output_tokens: Option<u32>,
    /// Whether reasoning is supported.
    pub(crate) reasoning: Option<bool>,
    /// Whether tool calling is supported.
    pub(crate) tool_call: Option<bool>,
    /// Supported input modalities; absent when the provider did not say.
    pub(crate) input_modalities: Option<Vec<String>>,
    /// Explicitly advertised reasoning efforts.
    pub(crate) reasoning_efforts: Option<Vec<String>>,
    /// Provider-reported prices and their unmodified source record.
    pub(crate) pricing: Option<CatalogPrices>,
}

/// Per-million-token prices taken from a provider's model metadata.
pub(crate) struct CatalogPrices {
    /// Input price.
    pub(crate) input: Option<f64>,
    /// Output price.
    pub(crate) output: Option<f64>,
    /// Cache write price.
    pub(crate) cache_write: Option<f64>,
    /// Cache read price.
    pub(crate) cache_read: Option<f64>,
    /// Identifier identifying the price's source and interpretation.
    pub(crate) source: &'static str,
    /// Original price record, including conditional pricing.
    pub(crate) raw: Value,
}

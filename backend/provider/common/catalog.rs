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

impl CatalogInfo {
    /// Apply discovery facts to a route model, without consulting reference data.
    pub(crate) fn apply_to_route(self, model: &mut crate::openai::Model, claude: bool) {
        // Anthropic's max_input_tokens is input-only, not a combined context limit.
        if !claude {
            model.context_window = model
                .context_window
                .or(self.context_window)
                .filter(|n| *n > 0);
        }
        model.max_output_tokens = model
            .max_output_tokens
            .or(self.max_output_tokens)
            .filter(|n| *n > 0);
        model.reasoning = model.reasoning.or(self.reasoning);
        model.supports_tools = model.supports_tools.or(self.tool_call);
        if model.input_modalities.is_none() {
            model.input_modalities = self.input_modalities.map(|modalities| {
                modalities
                    .into_iter()
                    .filter(|m| {
                        if claude {
                            m == "text" || m == "image"
                        } else {
                            matches!(m.as_str(), "text" | "image" | "audio" | "video" | "pdf")
                        }
                    })
                    .collect()
            });
        }
        if claude {
            // The translator accepts text and images, but not PDF content parts.
            if let Some(modalities) = &mut model.input_modalities {
                modalities.retain(|m| m == "text" || m == "image");
                if !modalities.iter().any(|m| m == "text") {
                    modalities.insert(0, "text".into());
                }
            }
            model.supported_reasoning_efforts = self.reasoning_efforts;
            model.output_modalities = Some(vec!["text".into()]);
        }
        // A flattened provider field must never shadow a normalized field.
        for key in [
            "context_window",
            "max_output_tokens",
            "reasoning",
            "input_modalities",
            "output_modalities",
            "supports_tools",
            "supported_reasoning_efforts",
            "default_reasoning_effort",
        ] {
            model.extra.remove(key);
        }
    }
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

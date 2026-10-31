use crate::openai::Model;
use crate::provider::common::catalog::{CatalogInfo, CatalogPrices};
use serde_json::Value;

/// Normalize only modalities explicitly advertised by `OpenCode` or Models.dev.
fn modalities(value: &Value) -> Option<Vec<String>> {
    let values = value.as_array()?;
    let mut result = Vec::new();
    for value in values {
        let value = value.as_str()?;
        let modality = match value {
            "text" | "image" | "audio" | "video" | "pdf" => Some(value),
            "application/pdf" => Some("pdf"),
            _ if value.starts_with("image/") && value.len() > 6 => Some("image"),
            _ if value.starts_with("audio/") && value.len() > 6 => Some("audio"),
            _ if value.starts_with("video/") && value.len() > 6 => Some("video"),
            _ if value.starts_with("text/") && value.len() > 5 => Some("text"),
            _ => None,
        };
        if let Some(modality) = modality
            && !result.iter().any(|item| item == modality)
        {
            result.push(modality.to_owned());
        }
    }
    if result.is_empty() && !values.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Accept only nonnegative finite prices in dollars per million tokens.
fn price(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}

/// Extract catalog fields without changing discovery or model IDs.
pub(crate) fn catalog_info(model: &Model, kind: &str) -> CatalogInfo {
    let extra = &model.extra;
    let community = (kind == "zen").then(|| extra.get("zen_metadata")).flatten();
    let official = |key: &str, pointer: &str| extra.get(key).and_then(|v| v.pointer(pointer));
    let fallback = |pointer: &str| community.and_then(|v| v.pointer(pointer));
    let context_window = official("limit", "/context")
        .and_then(Value::as_u64)
        .or_else(|| fallback("/limit/context").and_then(Value::as_u64));
    let max_output_tokens = official("limit", "/output")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| {
            fallback("/limit/output")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        });
    let reasoning = official("capabilities", "/reasoning/supported")
        .and_then(Value::as_bool)
        .or_else(|| fallback("/reasoning").and_then(Value::as_bool));
    let tool_call = official("capabilities", "/tools/supported")
        .and_then(Value::as_bool)
        .or_else(|| fallback("/tool_call").and_then(Value::as_bool));
    let input_modalities = official("modalities", "/input")
        .and_then(modalities)
        .or_else(|| fallback("/modalities/input").and_then(modalities));
    let reasoning_efforts = official("capabilities", "/reasoning/effort")
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect()
        });

    // Keep one original price record and its provenance together; never combine
    // Go pricing with Zen or silently attribute community prices to OpenCode.
    let pricing = extra.get("cost").filter(|v| v.is_object()).map_or_else(
        || {
            fallback("/cost")
                .filter(|v| v.is_object())
                .map(|cost| CatalogPrices {
                    input: price(cost.get("input")),
                    output: price(cost.get("output")),
                    cache_read: price(cost.get("cache_read")),
                    cache_write: price(cost.get("cache_write")),
                    source: "models.dev",
                    raw: cost.clone(),
                })
        },
        |cost| {
            Some(CatalogPrices {
                input: price(cost.pointer("/text/input/price")),
                output: price(cost.pointer("/text/output/price")),
                cache_read: price(cost.pointer("/text/input/cache/read")),
                cache_write: price(cost.pointer("/text/input/cache/write")),
                source: if kind == "go" {
                    "opencode-go"
                } else {
                    "opencode-zen"
                },
                raw: cost.clone(),
            })
        },
    );
    CatalogInfo {
        context_window,
        max_output_tokens,
        reasoning,
        tool_call,
        input_modalities,
        reasoning_efforts,
        pricing,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::catalog_info;
    use crate::openai::Model;
    use serde_json::json;

    #[test]
    fn official_records_preserve_prices_limits_and_capabilities() {
        let model = |input| {
            serde_json::from_value::<Model>(json!({
            "id":"same-id", "limit":{"context":1_000_000,"output":131_072},
            "capabilities":{"reasoning":{"supported":false,"effort":["low","high"]},"tools":{"supported":true}},
            "modalities":{"input":["text","image/jpeg","image/png","audio/wav","video/mp4","application/pdf","application/zip"]},
            "cost":{"text":{"input":{"price":input,"cache":{"read":0,"write":0.25}},"output":{"price":4}},"multipliers":[{"factor":2}]}
        })).unwrap()
        };
        let zen = catalog_info(&model(1.0), "zen");
        let go = catalog_info(&model(0.15), "go");
        assert_eq!(zen.context_window, Some(1_000_000));
        assert_eq!(zen.max_output_tokens, Some(131_072));
        assert_eq!(zen.reasoning, Some(false));
        assert_eq!(zen.tool_call, Some(true));
        assert_eq!(zen.reasoning_efforts.unwrap(), ["low", "high"]);
        assert_eq!(
            zen.input_modalities.unwrap(),
            ["text", "image", "audio", "video", "pdf"]
        );
        let zen_price = zen.pricing.unwrap();
        let go_price = go.pricing.unwrap();
        assert_eq!(zen_price.input, Some(1.0));
        assert_eq!(go_price.input, Some(0.15));
        assert_eq!(go_price.output, Some(4.0));
        assert_eq!(go_price.cache_read, Some(0.0));
        assert_eq!(go_price.cache_write, Some(0.25));
        assert_eq!(go_price.source, "opencode-go");
        assert_eq!(go_price.raw["multipliers"][0]["factor"], 2);
    }

    #[test]
    fn community_fallback_is_zen_only_and_unknowns_stay_unknown() {
        let model: Model = serde_json::from_value(json!({
            "id":"example", "limit":{"context":-1,"output":4_294_967_296_u64},
            "capabilities":{"reasoning":{"supported":"yes","effort":["low",1]},"tools":{"supported":false}},
            "modalities":{"input":["application/zip"]},
            "zen_metadata":{"limit":{"context":0,"output":0},"reasoning":false,"tool_call":true,
                "modalities":{"input":["text","image"]},"cost":{"input":0,"output":2,"cache_read":0.1}}
        })).unwrap();
        let zen = catalog_info(&model, "zen");
        assert_eq!(zen.context_window, Some(0));
        assert_eq!(zen.max_output_tokens, Some(0));
        assert_eq!(zen.reasoning, Some(false));
        assert_eq!(zen.tool_call, Some(false));
        assert!(zen.reasoning_efforts.is_none());
        assert_eq!(zen.input_modalities.unwrap(), ["text", "image"]);
        let prices = zen.pricing.unwrap();
        assert_eq!(prices.input, Some(0.0));
        assert_eq!(prices.output, Some(2.0));
        assert_eq!(prices.cache_read, Some(0.1));
        assert_eq!(prices.cache_write, None);
        assert_eq!(prices.source, "models.dev");
        let go = catalog_info(&model, "go");
        assert!(go.context_window.is_none());
        assert!(go.max_output_tokens.is_none());
        assert!(go.reasoning.is_none());
        assert!(go.input_modalities.is_none());
        assert!(go.pricing.is_none());
    }

    #[test]
    fn malformed_and_non_token_costs_are_not_prices() {
        let model: Model = serde_json::from_value(json!({"id":"example", "cost":{
            "text":{"input":{"price":-1,"cache":{"read":"0"}},"output":{"price":null}},
            "request":{"price":5}
        }}))
        .unwrap();
        let prices = catalog_info(&model, "go").pricing.unwrap();
        assert_eq!(prices.input, None);
        assert_eq!(prices.output, None);
        assert_eq!(prices.cache_read, None);
        assert_eq!(prices.cache_write, None);
    }
}

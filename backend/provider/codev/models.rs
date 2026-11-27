use serde_json::{Map, Value};

use crate::openai::Model;
use crate::provider::common::catalog::{CatalogInfo, CatalogPrices};

/// Field under which the gateway's per-model prices are kept on a discovered model.
const INFO_KEY: &str = "codev_pricing";

/// Context windows from the `CoDev` Code client's built-in table.
///
/// The gateway reports no token limits (its `/model_group/info` route is not
/// exposed and `/v1/model/info` carries only prices), so the client ships these.
const CONTEXT_WINDOWS: &[(&str, u64)] = &[
    ("MiniMax/MiniMax-M3", 262_144),
    ("zai-org/GLM-5.3-Flash", 262_144),
];

/// Price fields copied from the gateway's model info, all in cost per token.
const PRICE_FIELDS: [&str; 3] = [
    "input_cost_per_token",
    "output_cost_per_token",
    "cache_read_input_token_cost",
];

/// Picks the prices of the deployment that serves each model name.
///
/// One name can map to several deployments; blocked ones do not serve traffic.
fn prices_by_model(info: &Value) -> Map<String, Value> {
    let mut prices = Map::new();
    for entry in info
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = entry.get("model_name").and_then(Value::as_str) else {
            continue;
        };
        let details = entry.get("model_info");
        if prices.contains_key(name)
            || details
                .and_then(|details| details.get("blocked"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            continue;
        }
        let costs: Map<String, Value> = PRICE_FIELDS
            .iter()
            .filter_map(|field| {
                let value = details
                    .and_then(|details| details.get(*field))
                    .or_else(|| entry.get("litellm_params")?.get(*field))?;
                value
                    .is_number()
                    .then(|| ((*field).to_owned(), value.clone()))
            })
            .collect();
        if !costs.is_empty() {
            prices.insert(name.to_owned(), Value::Object(costs));
        }
    }
    prices
}

/// Attaches gateway prices to discovered models.
pub(super) fn attach_prices(models: &mut [Model], info: &Value) {
    let mut prices = prices_by_model(info);
    for model in models {
        if let Some(costs) = prices.remove(&model.id) {
            model.extra.insert(INFO_KEY.to_owned(), costs);
        }
    }
}

/// Context window, prices, and reasoning support for the dashboard and model list.
pub(crate) fn catalog_info(model: &Model) -> CatalogInfo {
    /// Converts a per-token cost to a per-million-token price.
    fn per_million(costs: &Value, field: &str) -> Option<f64> {
        let per_token = costs.get(field)?.as_f64()?;
        let price = per_token * 1_000_000.0;
        (per_token >= 0.0 && price.is_finite()).then_some(price)
    }
    let pricing = model
        .extra
        .get(INFO_KEY)
        .filter(|costs| costs.is_object())
        .map(|costs| CatalogPrices {
            input: per_million(costs, "input_cost_per_token"),
            output: per_million(costs, "output_cost_per_token"),
            cache_write: None,
            cache_read: per_million(costs, "cache_read_input_token_cost"),
            source: "codev-gateway",
            raw: costs.clone(),
        });
    CatalogInfo {
        context_window: CONTEXT_WINDOWS
            .iter()
            .find(|(id, _)| *id == model.id)
            .map(|(_, window)| *window),
        pricing,
        ..CatalogInfo::default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(id: &str) -> Model {
        serde_json::from_value(json!({"id": id})).unwrap()
    }

    /// Prices come from the serving deployment, and known models get their context window.
    #[test]
    fn prices_skip_blocked_deployments_and_windows_come_from_the_table() {
        let info = json!({"data": [
            {"model_name": "MiniMax/MiniMax-M3",
             "model_info": {"blocked": true, "input_cost_per_token": 1.5e-7}},
            {"model_name": "MiniMax/MiniMax-M3",
             "model_info": {"blocked": false, "input_cost_per_token": 3e-7,
                            "output_cost_per_token": 1.2e-6,
                            "cache_read_input_token_cost": 6e-8}},
            {"model_name": "new/model",
             "litellm_params": {"input_cost_per_token": 1e-7}}
        ]});
        let mut models = vec![
            model("MiniMax/MiniMax-M3"),
            model("new/model"),
            model("bare"),
        ];
        attach_prices(&mut models, &info);

        let minimax = catalog_info(&models[0]);
        assert_eq!(minimax.context_window, Some(262_144));
        let prices = minimax.pricing.unwrap();
        assert!((prices.input.unwrap() - 0.3).abs() < 1e-9);
        assert!((prices.output.unwrap() - 1.2).abs() < 1e-9);
        assert!((prices.cache_read.unwrap() - 0.06).abs() < 1e-9);

        let new = catalog_info(&models[1]);
        assert_eq!(new.context_window, None);
        assert!((new.pricing.unwrap().input.unwrap() - 0.1).abs() < 1e-9);

        let bare = catalog_info(&models[2]);
        assert!(bare.pricing.is_none() && bare.context_window.is_none());
    }
}

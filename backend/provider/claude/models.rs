use crate::{
    openai::Model,
    provider::{claude::auth, registry::Provider},
    server::AppState,
};

/// Extract only metadata explicitly supplied by the Anthropic model list.
pub(crate) fn catalog_info(model: &Model) -> crate::provider::common::catalog::CatalogInfo {
    let capabilities = model.extra.get("capabilities");
    let supported = |name: &str| {
        capabilities
            .and_then(|c| c.get(name))
            .and_then(|c| c.get("supported"))
            .and_then(serde_json::Value::as_bool)
    };
    let image = supported("image_input");
    let pdf = supported("pdf_input");
    let input_modalities = (image.is_some() || pdf.is_some()).then(|| {
        [("image", image), ("pdf", pdf)]
            .into_iter()
            .filter(|(_, enabled)| *enabled == Some(true))
            .map(|(name, _)| name.to_owned())
            .collect()
    });
    let effort = capabilities.and_then(|c| c.get("effort"));
    let levels = ["low", "medium", "high", "max", "xhigh"].map(|name| {
        (
            name,
            effort
                .and_then(|e| e.get(name))
                .and_then(|e| e.get("supported"))
                .and_then(serde_json::Value::as_bool),
        )
    });
    let reasoning_efforts = levels
        .iter()
        .any(|(_, enabled)| enabled.is_some())
        .then(|| {
            levels
                .into_iter()
                .filter(|(_, enabled)| *enabled == Some(true))
                .map(|(name, _)| name.to_owned())
                .collect()
        });
    crate::provider::common::catalog::CatalogInfo {
        context_window: model
            .extra
            .get("max_input_tokens")
            .and_then(serde_json::Value::as_u64),
        max_output_tokens: model
            .extra
            .get("max_tokens")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        reasoning: supported("thinking"),
        input_modalities,
        reasoning_efforts,
        ..Default::default()
    }
}

/// Extract fallback capabilities from an exact Anthropic API datasheet record.
pub(crate) fn catalog_reference(
    value: &serde_json::Value,
) -> crate::provider::common::catalog::CatalogInfo {
    let field = |name: &str| value.get(name);
    let image = field("supports_vision").and_then(serde_json::Value::as_bool);
    let pdf = field("supports_pdf_input").and_then(serde_json::Value::as_bool);
    let chat = field("mode").and_then(serde_json::Value::as_str) == Some("chat");
    let input_modalities = (chat || image.is_some() || pdf.is_some()).then(|| {
        [
            ("text", chat),
            ("image", image == Some(true)),
            ("pdf", pdf == Some(true)),
        ]
        .into_iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(name, _)| name.to_owned())
        .collect()
    });
    crate::provider::common::catalog::CatalogInfo {
        context_window: field("max_input_tokens").and_then(serde_json::Value::as_u64),
        max_output_tokens: field("max_output_tokens")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        reasoning: field("supports_reasoning").and_then(serde_json::Value::as_bool),
        tool_call: field("supports_function_calling").and_then(serde_json::Value::as_bool),
        input_modalities,
        ..Default::default()
    }
}

/// Read the account-authenticated Anthropic catalog, following pagination.
async fn fetch(client: &reqwest::Client, base: &str, token: &str) -> Result<Vec<Model>, String> {
    let mut models = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut request = client
            .get(format!("{}/v1/models", base.trim_end_matches('/')))
            .bearer_auth(token)
            .header("anthropic-version", "2023-06-01");
        if let Some(cursor) = &after {
            request = request.query(&[("after_id", cursor)]);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("Claude model discovery failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Claude model discovery failed: {}",
                response.status()
            ));
        }
        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Invalid Claude model list: {e}"))?;
        let entries = value
            .get("data")
            .and_then(serde_json::Value::as_array)
            .ok_or("Invalid Claude model list")?;
        for entry in entries {
            let id = entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or("Claude model missing id")?;
            let mut model: Model = serde_json::from_value(entry.clone())
                .map_err(|e| format!("Invalid Claude model: {e}"))?;
            model.id = id.to_string();
            model.object = Some("model".into());
            model.owned_by = Some("anthropic".into());
            models.push(model);
        }
        if !value
            .get("has_more")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            break;
        }
        let next = value
            .get("last_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("Claude model list missing pagination cursor")?;
        if after.as_deref() == Some(next) {
            return Err("Claude model list repeated its pagination cursor".into());
        }
        after = Some(next.to_owned());
    }
    Ok(models)
}

/// Discover models using a connected credential from the weighted pool.
pub(super) async fn discover(state: &AppState, provider: &Provider) -> Result<Vec<Model>, String> {
    let credentials = crate::provider::credentials::list(&state.settings, &provider.id).await?;
    let http = super::client::http()?;
    let mut last = "No connected Claude credentials".to_string();
    for credential in credentials {
        match auth::load(&state.settings, &provider.id, &credential.id).await {
            Ok(creds) => match fetch(&http, "https://api.anthropic.com", &creds.access_token).await
            {
                Ok(models) => return Ok(models),
                Err(e) => last = e,
            },
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Authenticated catalog tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::HeaderMap, routing::get};
    use serde_json::json;

    #[test]
    fn catalog_maps_explicit_model_details() {
        let model: Model = serde_json::from_value(json!({
            "id": "claude-example",
            "max_input_tokens": 200_000,
            "max_tokens": 0,
            "capabilities": {
                "image_input": {"supported": true},
                "pdf_input": {"supported": false},
                "thinking": {"supported": false},
                "effort": {
                    "supported": true,
                    "low": {"supported": true},
                    "medium": {"supported": false},
                    "high": {"supported": true},
                    "max": {"supported": false},
                    "xhigh": {"supported": true}
                }
            }
        }))
        .unwrap();
        let info = catalog_info(&model);
        assert_eq!(model.id, "claude-example");
        assert_eq!(info.context_window, Some(200_000));
        assert_eq!(info.max_output_tokens, Some(0));
        assert_eq!(info.reasoning, Some(false));
        assert_eq!(info.input_modalities, Some(vec!["image".into()]));
        assert_eq!(
            info.reasoning_efforts,
            Some(vec!["low".into(), "high".into(), "xhigh".into()])
        );
        assert!(info.tool_call.is_none());
        assert!(info.pricing.is_none());
    }

    #[test]
    fn catalog_leaves_missing_metadata_unknown() {
        let model: Model = serde_json::from_value(json!({"id": "claude-example"})).unwrap();
        let info = catalog_info(&model);
        assert!(info.context_window.is_none());
        assert!(info.max_output_tokens.is_none());
        assert!(info.reasoning.is_none());
        assert!(info.input_modalities.is_none());
        assert!(info.reasoning_efforts.is_none());
        assert!(info.tool_call.is_none());
        assert!(info.pricing.is_none());
    }

    #[test]
    fn catalog_ignores_malformed_metadata_and_preserves_explicit_false() {
        let model: Model = serde_json::from_value(json!({
            "id": "claude-example",
            "max_input_tokens": -1,
            "max_tokens": 4_294_967_296_u64,
            "capabilities": {
                "image_input": {"supported": "true"},
                "pdf_input": {"supported": false},
                "thinking": {"supported": 1},
                "effort": {
                    "low": {"supported": false},
                    "medium": {"supported": "true"},
                    "high": null
                }
            }
        }))
        .unwrap();
        let info = catalog_info(&model);
        assert!(info.context_window.is_none());
        assert!(info.max_output_tokens.is_none());
        assert!(info.reasoning.is_none());
        assert_eq!(info.input_modalities, Some(vec![]));
        assert_eq!(info.reasoning_efforts, Some(vec![]));
    }

    /// Fetch every page with the credential's bearer token.
    #[tokio::test]
    async fn discovery_sends_bearer_and_follows_cursor() {
        let app = Router::new().route("/v1/models", get(|headers: HeaderMap, axum::extract::RawQuery(query): axum::extract::RawQuery| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer test-token");
            assert!(!headers.contains_key("x-api-key"));
            Json(if query.is_none() { json!({"data":[{"id":"first","display_name":"First"}],"has_more":true,"last_id":"first"}) } else { assert_eq!(query.as_deref(), Some("after_id=first")); json!({"data":[{"id":"second"}],"has_more":false}) })
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let models = fetch(
            &reqwest::Client::new(),
            &format!("http://{addr}"),
            "test-token",
        )
        .await
        .unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }
}

use crate::{
    openai::Model,
    provider::{claude::auth, registry::Provider},
    server::AppState,
};

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

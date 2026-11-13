use super::*;
use crate::database::Database;
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use serde_json::json;
use tower::ServiceExt;

fn sample_provider(kind: &str, id: &str) -> CachedProvider {
    CachedProvider {
        id: id.into(),
        name: id.into(),
        kind: kind.into(),
        base_url: String::new(),
        models: Vec::new(),
        error: None,
    }
}

fn sample_data(pricing: Value, parameters: Value) -> super::super::model_setting::ModelData {
    super::super::model_setting::ModelData {
        pricing,
        parameters,
    }
}

/// Reference prices and capabilities apply only to exact Anthropic-owned models.
#[tokio::test]
async fn claude_catalog_uses_exact_reference_data_without_overriding_live_fields() {
    let store = store().await;
    Setting::upsert_by_name("model_pricing_datasheet")
        .value(json!({
            "anthropic/claude-example": {"provider":"anthropic","input_cost_per_token":0.000_001,"output_cost_per_token":0.000_005},
            "claude-other": {"provider":"bedrock","input_cost_per_token":0.000_099}
        }).to_string())
        .exec(&mut (*store.database.db).clone())
        .await.unwrap();
    Setting::upsert_by_name("model_parameters_datasheet")
        .value(json!({
            "anthropic/claude-example": {"provider":"anthropic","mode":"chat","max_input_tokens":200_000,"max_output_tokens":64000,"supports_reasoning":true,"supports_function_calling":true,"supports_vision":true,"supports_pdf_input":true},
            "claude-other": {"provider":"bedrock","max_input_tokens":123}
        }).to_string())
        .exec(&mut (*store.database.db).clone())
        .await.unwrap();
    let mut provider = sample_provider("claude", "claude");
    provider.models = vec![
        serde_json::from_value(json!({"id":"claude-example","max_input_tokens":100_000,"capabilities":{"thinking":{"supported":false}}})).unwrap(),
        serde_json::from_value(json!({"id":"claude-other"})).unwrap(),
    ];
    let result = serde_json::to_value(
        normalize_catalog(
            &store,
            CachedCatalog {
                providers: vec![provider],
            },
        )
        .await,
    )
    .unwrap();
    let models = &result["providers"][0]["models"];
    assert_eq!(models[0]["model"]["context_window"], 100_000);
    assert_eq!(models[0]["model"]["max_output_tokens"], 64000);
    assert_eq!(models[0]["reasoning"], false);
    assert_eq!(models[0]["tool_call"], true);
    assert_eq!(
        models[0]["input_modalities"],
        json!(["text", "image", "pdf"])
    );
    assert_eq!(models[0]["pricing"]["input"], 1.0);
    assert_eq!(models[0]["pricing"]["output"], 5.0);
    assert_eq!(models[0]["pricing"]["source"], "claude-reference");
    assert_eq!(models[1]["pricing"], Value::Null);
    assert_eq!(models[1]["model"].get("context_window"), None);
}

/// Codex uses live input types and exact `OpenAI` API references without mixing providers.
#[tokio::test]
async fn codex_catalog_uses_live_modalities_and_exact_openai_reference_prices() {
    let store = store().await;
    Setting::upsert_by_name("model_pricing_datasheet")
        .value(json!({
            "openai/gpt-5.3-codex": {"provider":"openai","input_cost_per_token":0.000_001_75,"output_cost_per_token":0.000_014,"cache_read_input_token_cost":0.000_000_175},
            "gpt-5.4": {"provider":"openai","input_cost_per_token":0.000_002_5,"output_cost_per_token":0.000_015},
            "gpt-5.5": {"provider":"openai","input_cost_per_token":0.000_005,"supported_modalities":["text","image"]},
            "gpt-impostor": {"provider":"other","input_cost_per_token":0.000_099}
        }).to_string())
        .exec(&mut (*store.database.db).clone()).await.unwrap();
    Setting::upsert_by_name("model_parameters_datasheet")
        .value(json!({
            "openai/gpt-5.3-codex": {"provider":"openai","supported_modalities":["text","image"]},
            "gpt-5.5": {"provider":"openai","mode":"chat"},
            "gpt-5.4": {"provider":"openai","supported_modalities":["text","image"]},
            "gpt-impostor": {"provider":"other","supported_modalities":["audio"]}
        }).to_string())
        .exec(&mut (*store.database.db).clone()).await.unwrap();
    let mut provider = sample_provider("codex", "codex");
    provider.models = vec![
        serde_json::from_value(json!({"id":"gpt-5.3-codex","input_modalities":["text"]})).unwrap(),
        serde_json::from_value(json!({"id":"gpt-5.4"})).unwrap(),
        serde_json::from_value(json!({"id":"gpt-5.5"})).unwrap(),
        serde_json::from_value(json!({"id":"gpt-impostor"})).unwrap(),
    ];
    let result = serde_json::to_value(
        normalize_catalog(
            &store,
            CachedCatalog {
                providers: vec![provider],
            },
        )
        .await,
    )
    .unwrap();
    let models = &result["providers"][0]["models"];
    assert_eq!(models[0]["input_modalities"], json!(["text"]));
    assert_eq!(models[0]["pricing"]["input"], 1.75);
    assert_eq!(models[0]["pricing"]["output"], 14.0);
    assert_eq!(models[0]["pricing"]["cache_read"], 0.175);
    assert_eq!(models[0]["pricing"]["source"], "codex-reference");
    assert_eq!(models[1]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[1]["pricing"]["input"], 2.5);
    assert_eq!(models[2]["input_modalities"], json!(["text", "image"]));
    assert_eq!(models[2]["pricing"]["input"], 5.0);
    assert_eq!(models[3]["input_modalities"], json!([]));
    assert_eq!(models[3]["pricing"], Value::Null);
}

#[test]
fn provider_lookup_precedence_and_owner_restrictions() {
    let provider = sample_provider("openai", "custom");
    let sheet = json!({
        "custom/m": {"provider":"custom", "value":1},
        "openai/m": {"provider":"openai", "value":2},
        "m": {"provider":"custom", "value":3},
        "other/m": {"provider":"custom", "value":4}
    });
    assert_eq!(lookup(&sheet, &provider, "m").unwrap()["value"], 1);
    assert!(lookup(&sheet, &provider, "other/m").is_none());
    let sheet = json!({"custom/m":{"provider":"wrong"}, "openai/m":{"provider":"openai", "value":2}, "m":{"provider":"custom", "value":3}});
    assert_eq!(lookup(&sheet, &provider, "m").unwrap()["value"], 2);
}

#[test]
fn reference_aliases_require_matching_owner_and_base() {
    let sheet = json!({
        "gemini/gemini-x-preview":{"provider":"gemini","base_model":"other"},
        "vertex_ai/gemini-x-preview":{"provider":"vertex_ai","base_model":"gemini-x","value":2},
        "claude-x":{"provider":"anthropic","value":3},
        "anthropic/claude-x":{"provider":"wrong","value":4}
    });
    assert_eq!(reference_price(&sheet, "gemini-x").unwrap()["value"], 2);
    assert_eq!(
        reference_price(&sheet, "claude-x-thinking").unwrap()["value"],
        3
    );
    assert!(reference_price(&sheet, "gemini-y").is_none());
    assert!(
        reference_price(
            &json!({"gemini/gemini-x-preview":{"provider":"wrong","base_model":"gemini-x"}}),
            "gemini-x"
        )
        .is_none()
    );
}

#[test]
fn pricing_selection_units_and_unknown_values() {
    let model: Model = serde_json::from_value(json!({"id":"gemini-x"})).unwrap();
    let agy = sample_provider("agy", "custom");
    let data = sample_data(
        json!({
            "custom/gemini-x":{"provider":"custom","input_cost_per_token":0.000_002,"output_cost_per_token":"invalid"},
            "gemini/gemini-x":{"provider":"gemini","input_cost_per_token":0.000_009}
        }),
        json!({}),
    );
    let result = normalize_model(&agy, model, &data);
    let price = result.pricing.unwrap();
    assert_eq!(price.input, Some(2.0));
    assert_eq!(price.output, None);
    assert_eq!(price.cache_read, None);
    assert_eq!(price.source, "antigravity-reference");
    let zen = sample_provider("zen", "zen");
    let model: Model = serde_json::from_value(json!({
        "id":"m", "reasoning":true, "zen_metadata":{
            "reasoning":false,"tool_call":true,"modalities":{"input":["text", 42, "image"]},
            "cost":{"input":2.5,"output":null,"cache_read":0.1}
        }
    }))
    .unwrap();
    let result = normalize_model(
        &zen,
        model,
        &sample_data(
            json!({"zen/m":{"provider":"zen","input_cost_per_token":1}}),
            json!({}),
        ),
    );
    assert_eq!(result.reasoning, Some(false));
    assert_eq!(result.tool_call, Some(true));
    assert_eq!(result.input_modalities, ["text", "image"]);
    let price = result.pricing.unwrap();
    assert_eq!(price.input, Some(2.5));
    assert_eq!(price.output, None);
    assert_eq!(price.cache_read, Some(0.1));
    assert_eq!(price.source, "models.dev");
    let model: Model =
        serde_json::from_value(json!({"id":"m","zen_metadata":{"cost":[]}})).unwrap();
    assert!(
        normalize_model(&zen, model, &sample_data(json!({}), json!({})))
            .pricing
            .is_none()
    );
    let chatjimmy = sample_provider("chatjimmy", "chatjimmy");
    let model: Model = serde_json::from_value(json!({"id":"llama3.1-8B"})).unwrap();
    let result = normalize_model(&chatjimmy, model, &sample_data(json!({}), json!({})));
    assert_eq!(result.reasoning, Some(false));
    assert_eq!(result.tool_call, Some(false));
    assert_eq!(result.input_modalities, ["text"]);
    assert_eq!(result.model.context_window, Some(6064));
    assert_eq!(result.model.max_output_tokens, Some(2048));
    let price = result.pricing.unwrap();
    assert_eq!(price.input, Some(0.0));
    assert_eq!(price.output, Some(0.0));
    assert_eq!(price.source, "chatjimmy-reference");
}

/// Invalid metadata fields do not hide other capabilities or raw details.
#[test]
fn catalog_preserves_valid_fields_when_zen_metadata_is_malformed() {
    let mut provider = sample_provider("zen", "zen");
    provider.models.push(
        serde_json::from_value(json!({
            "id":"m",
            "zen_metadata":{
                "reasoning":"invalid",
                "tool_call":true,
                "modalities":{"input":["audio", 42, "pdf"]},
                "cost":{"input":"invalid", "output":3.0},
                "extra":"retained"
            }
        }))
        .unwrap(),
    );
    let result = serde_json::to_value(enrich_catalog(
        CachedCatalog {
            providers: vec![provider],
        },
        &sample_data(json!({}), json!({})),
    ))
    .unwrap();
    let model = &result["providers"][0]["models"][0];
    assert_eq!(model["reasoning"], Value::Null);
    assert_eq!(model["tool_call"], true);
    assert_eq!(model["input_modalities"], json!(["audio", "pdf"]));
    assert_eq!(model["pricing"]["input"], Value::Null);
    assert_eq!(model["pricing"]["output"], 3.0);
    assert_eq!(model["provider_metadata"]["extra"], "retained");
    assert_eq!(model["raw_pricing"]["input"], "invalid");
}

#[tokio::test]
async fn normalized_response_uses_current_sheets_without_changing_raw_cache() {
    let store = store().await;
    let mut provider = sample_provider("openai", "custom");
    provider.models.push(serde_json::from_value(json!({"id":"m","name":" ", "display_name":"Model", "description":"details", "custom":42})).unwrap());
    let catalog = CachedCatalog {
        providers: vec![provider],
    };
    save(&store, &catalog).await.unwrap();
    let response = serde_json::to_value(normalize_catalog(&store, catalog).await).unwrap();
    let model = &response["providers"][0]["models"][0];
    assert_eq!(model["model"]["custom"], 42);
    assert_eq!(model["display_name"], "Model");
    assert_eq!(model["description"], "details");
    assert_eq!(model["pricing"], Value::Null);
    assert_eq!(model["parameters"], Value::Null);
    let saved: Value =
        serde_json::from_str(&store.setting_get(CACHE_KEY).await.unwrap().unwrap()).unwrap();
    assert_eq!(saved["providers"][0]["models"][0]["id"], "m");
    assert!(saved["providers"][0]["models"][0].get("model").is_none());
    Setting::upsert_by_name("model_pricing_datasheet")
        .value(r#"{"custom/m":{"provider":"custom","input_cost_per_token":0.000003}}"#)
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
    let cached: CachedCatalog = serde_json::from_value(saved).unwrap();
    let response = serde_json::to_value(normalize_catalog(&store, cached).await).unwrap();
    assert_eq!(
        response["providers"][0]["models"][0]["pricing"]["input"],
        3.0
    );
    assert_eq!(
        response["providers"][0]["models"][0]["pricing"]["source"],
        "datasheet"
    );
}

/// A damaged datasheet is reported without hiding discovered models.
#[tokio::test]
async fn catalog_keeps_models_when_datasheet_is_invalid() {
    let store = store().await;
    Setting::upsert_by_name("model_pricing_datasheet")
        .value("{invalid")
        .exec(&mut (*store.database.db).clone())
        .await
        .unwrap();
    let mut provider = sample_provider("openai", "custom");
    provider
        .models
        .push(serde_json::from_value(json!({"id":"m"})).unwrap());
    let response = normalize_catalog(
        &store,
        CachedCatalog {
            providers: vec![provider],
        },
    )
    .await;
    assert_eq!(
        response.datasheets_error.as_deref(),
        Some("Unable to load cached datasheets")
    );
    assert_eq!(response.providers[0].models[0].model.id, "m");
    assert!(response.providers[0].models[0].pricing.is_none());
}

/// Antigravity's cached capability flags and enabled MIME types are shown in the catalog.
#[tokio::test]
async fn catalog_reports_antigravity_input_modalities() {
    let store = store().await;
    assert_eq!(
        registry_request(&store, "POST", "/api/providers", json!({"kind":"agy"})).await,
        StatusCode::CREATED
    );
    let mut catalog = load(&store).await.unwrap();
    catalog.providers[0].models = [
        json!({"id":"vision","supportsImages":true,"supportedMimeTypes":{"image/png":true,"video/jpeg2000":true,"video/videoframe/jpeg2000":true,"audio/wav":false,"application/pdf":false}}),
        json!({"id":"multimodal","supportsImages":true,"supportsVideo":true,"supportedMimeTypes":{"audio/mpeg":true,"application/pdf":true,"video/mp4":true}}),
        json!({"id":"disabled","supportsImages":false,"supportsVideo":false,"supportedMimeTypes":{"image/png":true,"video/mp4":true,"audio/mpeg":false,"application/pdf":false}}),
        json!({"id":"mime-only","supportedMimeTypes":{"image/webp":true,"video/webm":true}}),
        json!({"id":"plain"}),
    ].into_iter().map(|model| serde_json::from_value(model).unwrap()).collect();
    save(&store, &catalog).await.unwrap();
    let state = AppState {
        providers: crate::provider::Providers::new(),
        database: store.database.clone(),
        dashboard_auth: crate::server::dashboard_auth_config::test_auth().await,
        settings: store,
    };
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/model-catalog")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    let models = document["providers"][0]["models"].as_array().unwrap();
    let inputs = |id: &str| {
        &models
            .iter()
            .find(|model| model["model"]["id"] == id)
            .unwrap()["input_modalities"]
    };
    assert_eq!(inputs("vision"), &json!(["text", "image"]));
    assert_eq!(
        inputs("multimodal"),
        &json!(["text", "image", "audio", "video", "pdf"])
    );
    assert_eq!(inputs("disabled"), &json!(["text"]));
    assert_eq!(inputs("mime-only"), &json!(["text", "image", "video"]));
    assert_eq!(inputs("plain"), &json!(["text"]));
}

/// Create an in-memory settings store.
async fn store() -> SettingsStore {
    let database = Database::open(":memory:").await.unwrap();
    SettingsStore::new(database, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .await
        .unwrap()
}

/// Send a JSON request to the provider registry.
async fn registry_request(
    store: &SettingsStore,
    method: &str,
    path: &str,
    body: Value,
) -> StatusCode {
    registry::router(store.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// Cached models survive reopening and follow registry changes.
#[tokio::test]
async fn cache_survives_store_recreation_and_tracks_registry() {
    let store = store().await;
    let provider = |id| json!({"id":id,"name":id,"kind":"openai","base_url":format!("http://{id}.localhost/v1")});
    assert_eq!(
        registry_request(&store, "POST", "/api/providers", provider("first")).await,
        StatusCode::CREATED
    );
    let mut catalog = load(&store).await.unwrap();
    catalog.providers[0].models =
        vec![serde_json::from_value(json!({"id":"cached", "metadata":42})).unwrap()];
    catalog.providers[0].error = Some("offline".into());
    save(&store, &catalog).await.unwrap();
    let reopened = SettingsStore::new(
        store.database.clone(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .await
    .unwrap();
    let cached = load(&reopened).await.unwrap();
    assert_eq!(cached.providers[0].models[0].extra["metadata"], 42);
    assert_eq!(cached.providers[0].error.as_deref(), Some("offline"));
    assert_eq!(
        registry_request(&store, "POST", "/api/providers", provider("second")).await,
        StatusCode::CREATED
    );
    assert_eq!(
        registry_request(&store, "DELETE", "/api/providers/first", Value::Null).await,
        StatusCode::NO_CONTENT
    );
    let updated = load(&reopened).await.unwrap();
    assert_eq!(updated.providers.len(), 1);
    assert_eq!(updated.providers[0].id, "second");
    assert!(updated.providers[0].models.is_empty());
}

use super::Model;
use serde_json::json;

#[test]
fn model_roundtrip_preserves_provider_metadata() {
    let input = json!({
        "id": "example",
        "name": "Example",
        "provider_metadata": {"nested": [1, true, null]}
    });
    let model: Model = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(serde_json::to_value(model).unwrap(), input);
}

#[test]
fn model_serializes_normalized_capabilities_without_extra_conflicts() {
    let model = Model {
        id: "example".into(),
        input_modalities: Some(vec!["text".into(), "image".into()]),
        output_modalities: Some(vec!["text".into()]),
        supports_tools: Some(false),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(model).unwrap(),
        json!({
            "id": "example",
            "input_modalities": ["text", "image"],
            "output_modalities": ["text"],
            "supports_tools": false
        })
    );
}

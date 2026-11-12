//! Conversion of tool schemas to the Gemini-supported subset.

use serde_json::{Value, json};

/// Rewrites a JSON Schema into the `OpenAPI` subset accepted by Gemini function
/// declarations, collapsing unions and dropping unsupported keywords.
// Google's schema proto accepts only a fixed subset; every other keyword is rejected.
pub(super) fn gemini_schema(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let mut schema = serde_json::Map::new();
    for (key, value) in object {
        match key.as_str() {
            "properties" => {
                if let Some(properties) = value.as_object() {
                    let properties = properties
                        .iter()
                        .map(|(name, value)| (name.clone(), gemini_schema(value)))
                        .collect();
                    schema.insert(key.clone(), Value::Object(properties));
                }
            }
            "items" => {
                let items = match value {
                    Value::Array(items) => items.first().map_or(Value::Null, gemini_schema),
                    _ => gemini_schema(value),
                };
                schema.insert(key.clone(), items);
            }
            "anyOf" | "oneOf" => {
                if let Some(branches) = value.as_array() {
                    let branches = branches.iter().map(gemini_schema).collect();
                    schema.insert("anyOf".to_owned(), Value::Array(branches));
                }
            }
            "const" => {
                if let Some(text) = value.as_str() {
                    schema.insert("enum".to_owned(), json!([text]));
                }
            }
            "enum" => {
                if let Some(values) = value.as_array() {
                    let strings: Vec<_> = values.iter().filter_map(Value::as_str).collect();
                    if !strings.is_empty() && strings.len() == values.len() {
                        schema.insert(key.clone(), json!(strings));
                    }
                }
            }
            "required" => {
                if let Some(values) = value.as_array() {
                    let strings: Vec<_> = values.iter().filter_map(Value::as_str).collect();
                    if !strings.is_empty() {
                        schema.insert(key.clone(), json!(strings));
                    }
                }
            }
            "description" | "title" | "format" | "pattern" | "default" | "example" | "nullable"
            | "minimum" | "maximum" | "minItems" | "maxItems" | "minLength" | "maxLength"
            | "minProperties" | "maxProperties" | "propertyOrdering" => {
                schema.insert(key.clone(), value.clone());
            }
            // `type` is normalized after the loop so unions can add `nullable`.
            // Other JSON Schema keywords have no Gemini schema field: `$ref`,
            // `$defs`, `additionalProperties`, `allOf`, `not`, and constraints.
            _ => {}
        }
    }

    let mut nullable = false;
    let mut kind = match object.get("type") {
        Some(Value::Array(types)) => {
            let mut kind = None;
            for name in types.iter().filter_map(Value::as_str) {
                if name == "null" {
                    nullable = true;
                } else if kind.is_none() {
                    kind = Some(name.to_owned());
                }
            }
            kind
        }
        Some(Value::String(name)) if name == "null" => {
            nullable = true;
            None
        }
        Some(Value::String(name)) => Some(name.clone()),
        _ => None,
    };
    // Gemini rejects a `type` beside union branches, so only infer one when absent.
    if !schema.contains_key("anyOf") {
        if kind.is_none() {
            if object.contains_key("properties") {
                kind = Some("object".to_owned());
            } else if object.contains_key("items") {
                kind = Some("array".to_owned());
            } else {
                kind = Some("string".to_owned());
            }
        }
        if let Some(kind) = kind {
            schema.insert("type".to_owned(), Value::String(kind));
        }
    }
    if nullable {
        schema.insert("nullable".to_owned(), Value::Bool(true));
    }
    Value::Object(schema)
}

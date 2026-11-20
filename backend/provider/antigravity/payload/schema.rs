//! Conversion of tool schemas to provider-compatible subsets.

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

/// Rewrites a tool schema for Claude models served through Antigravity.
///
/// That backend translates Gemini function declarations into Anthropic
/// `custom.input_schema`, which rejects nested multi-branch `anyOf`/`oneOf`
/// unions. Collapse each union to its strongest branch and keep the other
/// alternatives in the description so callers retain the intent.
pub(super) fn claude_schema(value: &Value) -> Value {
    ensure_required(gemini_schema(&flatten_unions(value.clone())))
}

/// Anthropic expects a `required` list wherever an object declares properties.
fn ensure_required(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            for nested in object.values_mut() {
                *nested = ensure_required(std::mem::take(nested));
            }
            if object.contains_key("properties") && !object.contains_key("required") {
                object.insert("required".to_owned(), json!([]));
            }
            Value::Object(object)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(ensure_required).collect()),
        other => other,
    }
}

/// Recursively replaces unions with one representative branch.
fn flatten_unions(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            for nested in object.values_mut() {
                *nested = flatten_unions(std::mem::take(nested));
            }
            let branches = object
                .remove("anyOf")
                .or_else(|| object.remove("oneOf"))
                .and_then(|value| match value {
                    Value::Array(branches) => Some(branches),
                    _ => None,
                })
                .filter(|branches| !branches.is_empty());
            let Some(branches) = branches else {
                return Value::Object(object);
            };

            let mut best_index = 0;
            let mut best_score = -1_i8;
            let mut kinds: Vec<&'static str> = Vec::new();
            for (index, branch) in branches.iter().enumerate() {
                let (score, kind) = branch_rank(branch);
                if !kind.is_empty() && !kinds.contains(&kind) {
                    kinds.push(kind);
                }
                if score > best_score {
                    best_score = score;
                    best_index = index;
                }
            }
            let Some(mut selected) = branches.into_iter().nth(best_index) else {
                return Value::Object(object);
            };
            if let Value::Object(fields) = &mut selected {
                if kinds.len() > 1 {
                    let branch_description = fields
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let parent_description = object
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let combined = [parent_description, branch_description.as_str()]
                        .into_iter()
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let hint = format!("Accepts: {}", kinds.join(" | "));
                    let description = if combined.is_empty() {
                        hint
                    } else {
                        format!("{combined} ({hint})")
                    };
                    fields.insert("description".to_owned(), Value::String(description));
                }
                for (key, value) in object {
                    fields.entry(key).or_insert(value);
                }
            }
            selected
        }
        Value::Array(items) => Value::Array(items.into_iter().map(flatten_unions).collect()),
        other => other,
    }
}

/// Ranks a union branch and names its JSON type for the fallback hint.
fn branch_rank(branch: &Value) -> (i8, &'static str) {
    let declared = branch.get("type").and_then(Value::as_str);
    let kind = match declared {
        Some("object") => "object",
        Some("array") => "array",
        Some("string") => "string",
        Some("number" | "integer") => "number",
        Some("boolean") => "boolean",
        Some("null") => "null",
        _ if branch.get("properties").is_some() => "object",
        _ if branch.get("items").is_some() => "array",
        _ => "",
    };
    let score = match kind {
        "object" => 3,
        "array" => 2,
        "string" | "number" | "boolean" => 1,
        _ => 0,
    };
    (score, kind)
}

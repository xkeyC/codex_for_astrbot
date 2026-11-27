//! A conservative check of a nested tool call's arguments against the tool's
//! JSON Schema, for TypeScript cells: static types cannot see values that came
//! from `any` (a tool result, `JSON.parse`, `load`).
//!
//! It knows `type` (with `integer`, `number`, `null`), `nullable`, `enum`,
//! `const`, `required`, `properties`, `additionalProperties`, `items`,
//! `allOf`, `anyOf` and `oneOf` (valid when any branch is), and ignores every
//! other keyword. Where a keyword it ignores could make the schema accept
//! more (`$ref`, `patternProperties`, `unevaluatedProperties`, ...), it
//! accepts: it never rejects arguments the schema allows.

use serde_json::Map;
use serde_json::Value as JsonValue;

/// Keywords that can widen what an object schema accepts beyond its
/// `properties`; with one of them present, `additionalProperties` is not
/// checked.
const OPEN_OBJECT_KEYWORDS: &[&str] = &[
    "$ref",
    "$dynamicRef",
    "patternProperties",
    "unevaluatedProperties",
    "dependentSchemas",
    "dependencies",
    "if",
    "allOf",
    "anyOf",
    "oneOf",
];

/// The first violation of `schema` by `value`, as `"<path> must be ..."`
/// with `path` starting at `args`; `None` when `value` may be valid.
pub(crate) fn check_arguments(schema: &JsonValue, value: &JsonValue) -> Option<String> {
    let mut path = String::from("args");
    violation(schema, value, &mut path, /*depth*/ 0)
}

const MAX_DEPTH: usize = 64;

fn violation(
    schema: &JsonValue,
    value: &JsonValue,
    path: &mut String,
    depth: usize,
) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    let schema = match schema {
        JsonValue::Bool(true) => return None,
        JsonValue::Bool(false) => return Some(format!("{path} is not allowed")),
        JsonValue::Object(schema) => schema,
        _ => return None,
    };
    // A reference is not resolved: whatever it points to may allow the value.
    if schema.contains_key("$ref") || schema.contains_key("$dynamicRef") {
        return None;
    }
    let nullable = schema.get("nullable") == Some(&JsonValue::Bool(true));
    if nullable && value.is_null() {
        return None;
    }

    if let Some(types) = schema.get("type")
        && let Some(allowed) = type_names(types)
        && !allowed.iter().any(|name| has_type(value, name))
    {
        return Some(format!(
            "{path} must be {}, got {}",
            allowed.join(" | "),
            describe(value)
        ));
    }
    if let Some(JsonValue::Array(options)) = schema.get("enum")
        && !options.iter().any(|option| json_equal(option, value))
    {
        return Some(format!(
            "{path} must be one of {}, got {}",
            options
                .iter()
                .take(10)
                .map(preview)
                .collect::<Vec<_>>()
                .join(", "),
            preview(value)
        ));
    }
    if let Some(constant) = schema.get("const")
        && !json_equal(constant, value)
    {
        return Some(format!(
            "{path} must be {}, got {}",
            preview(constant),
            preview(value)
        ));
    }

    match value {
        JsonValue::Object(object) => {
            if let Some(error) = object_violation(schema, object, path, depth) {
                return Some(error);
            }
        }
        JsonValue::Array(items) => {
            if let Some(item_schema) = schema.get("items")
                && item_schema.is_object()
            {
                for (index, item) in items.iter().enumerate() {
                    let length = path.len();
                    path.push_str(&format!("[{index}]"));
                    let error = violation(item_schema, item, path, depth + 1);
                    path.truncate(length);
                    if error.is_some() {
                        return error;
                    }
                }
            }
        }
        _ => {}
    }

    if let Some(JsonValue::Array(branches)) = schema.get("allOf") {
        for branch in branches {
            if let Some(error) = violation(branch, value, path, depth + 1) {
                return Some(error);
            }
        }
    }
    for keyword in ["anyOf", "oneOf"] {
        if let Some(JsonValue::Array(branches)) = schema.get(keyword)
            && !branches.is_empty()
        {
            let mut errors = Vec::new();
            for branch in branches {
                match violation(branch, value, &mut path.clone(), depth + 1) {
                    Some(error) => errors.push(error),
                    None => break,
                }
            }
            if errors.len() == branches.len() {
                let first = errors.swap_remove(0);
                return Some(if branches.len() == 1 {
                    first
                } else {
                    format!(
                        "{path} matches none of the {} allowed shapes ({first})",
                        branches.len()
                    )
                });
            }
        }
    }
    None
}

fn object_violation(
    schema: &Map<String, JsonValue>,
    object: &Map<String, JsonValue>,
    path: &mut String,
    depth: usize,
) -> Option<String> {
    let properties = schema.get("properties").and_then(JsonValue::as_object);
    if let Some(JsonValue::Array(required)) = schema.get("required") {
        for name in required.iter().filter_map(JsonValue::as_str) {
            if !object.contains_key(name) {
                return Some(format!("{} is required", child_path(path, name)));
            }
        }
    }
    for (name, value) in object {
        let length = path.len();
        push_child(path, name);
        let error = match properties.and_then(|properties| properties.get(name)) {
            Some(property_schema) => violation(property_schema, value, path, depth + 1),
            None => match schema.get("additionalProperties") {
                Some(JsonValue::Bool(false))
                    if !OPEN_OBJECT_KEYWORDS
                        .iter()
                        .any(|keyword| schema.contains_key(*keyword)) =>
                {
                    let known = properties
                        .map(|properties| {
                            properties
                                .keys()
                                .take(20)
                                .map(String::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    Some(format!(
                        "{path} is not an allowed property (allowed: {known})"
                    ))
                }
                Some(additional @ JsonValue::Object(_)) => {
                    violation(additional, value, path, depth + 1)
                }
                _ => None,
            },
        };
        path.truncate(length);
        if error.is_some() {
            return error;
        }
    }
    None
}

fn type_names(types: &JsonValue) -> Option<Vec<&str>> {
    let names = match types {
        JsonValue::String(name) => vec![name.as_str()],
        JsonValue::Array(names) => names.iter().filter_map(JsonValue::as_str).collect(),
        _ => return None,
    };
    // An unknown type name might be something the value is: accept.
    let known = [
        "null", "boolean", "object", "array", "number", "integer", "string",
    ];
    (!names.is_empty() && names.iter().all(|name| known.contains(name))).then_some(names)
}

fn has_type(value: &JsonValue, name: &str) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "number" => value.is_number(),
        "integer" => value
            .as_f64()
            .is_some_and(|number| number.is_finite() && number.fract() == 0.0),
        "string" => value.is_string(),
        _ => true,
    }
}

/// JSON equality with numbers compared by value (`1` equals `1.0`).
fn json_equal(left: &JsonValue, right: &JsonValue) -> bool {
    match (left, right) {
        (JsonValue::Number(left), JsonValue::Number(right)) => {
            left == right
                || left
                    .as_f64()
                    .is_some_and(|left| Some(left) == right.as_f64())
        }
        (JsonValue::Array(left), JsonValue::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_equal(left, right))
        }
        (JsonValue::Object(left), JsonValue::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, left)| right.get(key).is_some_and(|right| json_equal(left, right)))
        }
        _ => left == right,
    }
}

fn describe(value: &JsonValue) -> String {
    let kind = match value {
        JsonValue::Null => return "null".to_string(),
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    };
    format!("{kind} {}", preview(value))
}

fn preview(value: &JsonValue) -> String {
    const MAX: usize = 40;
    let text = value.to_string();
    if text.chars().count() <= MAX {
        return text;
    }
    let mut short = text.chars().take(MAX).collect::<String>();
    short.push('…');
    short
}

fn child_path(path: &str, name: &str) -> String {
    let mut path = path.to_string();
    push_child(&mut path, name);
    path
}

fn push_child(path: &mut String, name: &str) {
    let identifier = name
        .chars()
        .next()
        .is_some_and(|first| first == '_' || first == '$' || first.is_ascii_alphabetic())
        && name
            .chars()
            .all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric());
    if identifier {
        path.push('.');
        path.push_str(name);
    } else {
        path.push('[');
        path.push_str(&serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\"")));
        path.push(']');
    }
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;

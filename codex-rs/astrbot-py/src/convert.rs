use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value as JsonValue;

/// Converts `{"dotted.key": json}` overrides into `-c` style TOML overrides.
pub fn json_overrides_to_toml(
    overrides: &serde_json::Map<String, JsonValue>,
) -> Result<Vec<(String, toml::Value)>> {
    overrides
        .iter()
        .map(|(key, value)| Ok((key.clone(), json_to_toml(value)?)))
        .collect()
}

fn json_to_toml(value: &JsonValue) -> Result<toml::Value> {
    Ok(match value {
        JsonValue::Null => return Err(anyhow!("null is not a valid TOML value")),
        JsonValue::Bool(b) => toml::Value::Boolean(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else {
                toml::Value::Float(n.as_f64().unwrap_or_default())
            }
        }
        JsonValue::String(s) => toml::Value::String(s.clone()),
        JsonValue::Array(items) => {
            toml::Value::Array(items.iter().map(json_to_toml).collect::<Result<_>>()?)
        }
        JsonValue::Object(map) => {
            let mut table = toml::map::Map::new();
            for (k, v) in map {
                if !v.is_null() {
                    table.insert(k.clone(), json_to_toml(v)?);
                }
            }
            toml::Value::Table(table)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_nested_values() {
        let map = json!({"features.shell_tool": false, "model_providers.mock": {"name": "m", "retries": 2}})
            .as_object()
            .cloned()
            .unwrap_or_default();
        let out = json_overrides_to_toml(&map).unwrap_or_default();
        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .any(|(k, v)| k == "features.shell_tool" && *v == toml::Value::Boolean(false))
        );
    }
}

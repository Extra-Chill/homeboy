use serde_json::{Map, Value};

pub(in crate::commands::report) fn string_value(
    finding: &Map<String, Value>,
    key: &str,
) -> Option<String> {
    direct_string_value(finding, key)
        .or_else(|| nested_string_value(finding, "metadata", key))
        .or_else(|| nested_string_value(finding, "raw", key))
}

pub(in crate::commands::report) fn number_value(
    finding: &Map<String, Value>,
    key: &str,
) -> Option<f64> {
    direct_number_value(finding, key)
        .or_else(|| nested_number_value(finding, "metadata", key))
        .or_else(|| nested_number_value(finding, "raw", key))
}

fn direct_string_value(finding: &Map<String, Value>, key: &str) -> Option<String> {
    finding.get(key).and_then(value_to_string)
}

fn direct_number_value(finding: &Map<String, Value>, key: &str) -> Option<f64> {
    finding.get(key).and_then(Value::as_f64)
}

fn nested_string_value(
    finding: &Map<String, Value>,
    object_key: &str,
    value_key: &str,
) -> Option<String> {
    finding
        .get(object_key)
        .and_then(Value::as_object)
        .and_then(|object| object.get(value_key))
        .and_then(value_to_string)
}

fn nested_number_value(
    finding: &Map<String, Value>,
    object_key: &str,
    value_key: &str,
) -> Option<f64> {
    finding
        .get(object_key)
        .and_then(Value::as_object)
        .and_then(|object| object.get(value_key))
        .and_then(Value::as_f64)
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

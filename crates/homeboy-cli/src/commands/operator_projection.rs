//! Shared bounds for command payloads rendered to an operator terminal.

use serde_json::{Map, Value};

pub(crate) const TEXT_BYTES: usize = 512;
pub(crate) const ITEM_LIMIT: usize = 12;

pub(crate) fn text(value: &str) -> String {
    text_with_limit(value, TEXT_BYTES)
}

pub(crate) fn text_with_limit(value: &str, max_bytes: usize) -> String {
    if serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= max_bytes) {
        return value.to_string();
    }
    format!(
        "[omitted {} bytes; sha256={}]",
        value.len(),
        homeboy_engine_primitives::content_hash::sha256_hex(value.as_bytes())
    )
}

pub(crate) fn value(input: &Value) -> Value {
    match input {
        Value::String(input) => Value::String(text(input)),
        Value::Array(values) => Value::Array(values.iter().take(ITEM_LIMIT).map(value).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .take(ITEM_LIMIT)
                .map(|(key, item)| (key.clone(), value(item)))
                .collect::<Map<_, _>>(),
        ),
        input => input.clone(),
    }
}

pub(crate) fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

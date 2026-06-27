//! Shared JSON-path accessors for the compact-summary renderers.
//!
//! The `agent_task`, `bench`, and `runs` compact-summary modules all walk a
//! serialized JSON envelope by dotted segment paths to pull out strings,
//! counts, and array lengths. These small accessors used to be copy-pasted
//! into each summary module verbatim, which the audit flagged as duplicate
//! functions. They live here once so every summary renderer shares a single
//! implementation.

use serde_json::Value;

/// Walk `payload` following `path`, where each segment is either an object key
/// or (when it parses as a `usize`) an array index. Returns the addressed value
/// or `None` if any segment is missing or mistyped.
pub(crate) fn value_at<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = payload;
    for segment in path {
        if let Ok(index) = segment.parse::<usize>() {
            current = current.as_array()?.get(index)?;
        } else {
            current = current.get(*segment)?;
        }
    }
    Some(current)
}

/// Resolve `path` and return it as a string slice, if present and a string.
pub(crate) fn string_value<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(payload, path)?.as_str()
}

/// Resolve `path` and return it as a `u64`, if present and an unsigned integer.
pub(crate) fn u64_value(payload: &Value, path: &[&str]) -> Option<u64> {
    value_at(payload, path)?.as_u64()
}

/// Resolve `path` and return it as a `usize`, if present and a non-negative
/// integer that fits.
pub(crate) fn usize_value(payload: &Value, path: &[&str]) -> Option<usize> {
    value_at(payload, path)?.as_u64()?.try_into().ok()
}

/// Resolve `path` and return the length of the array it addresses, if present.
pub(crate) fn array_len(payload: &Value, path: &[&str]) -> Option<usize> {
    Some(value_at(payload, path)?.as_array()?.len())
}

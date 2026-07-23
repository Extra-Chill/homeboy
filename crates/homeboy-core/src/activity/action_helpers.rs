//! Small leaf helpers shared across the activity report builder and its
//! source-provider submodules: timestamp parsing, metadata extraction, and
//! next-action construction. Extracted from the `activity` module (#9794).

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::ActivityNextAction;

pub(crate) fn parse_ts(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

pub(crate) fn metadata_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

pub(crate) fn action(label: impl Into<String>, command: impl Into<String>) -> ActivityNextAction {
    ActivityNextAction {
        label: label.into(),
        command: command.into(),
    }
}

pub(crate) fn ms_to_rfc3339(ms: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms as i64)
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

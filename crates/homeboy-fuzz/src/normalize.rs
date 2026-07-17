//! Shared normalization helpers for fuzz contract validation.

pub(super) fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub(super) fn normalize_string_vec(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

pub(super) fn trim_or_default(value: &str, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn required_trimmed(field: &str, value: &str) -> std::result::Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} must be non-empty"))
    } else {
        Ok(trimmed.to_string())
    }
}

pub(super) fn require_schema(
    actual: &str,
    expected: &str,
    label: &str,
) -> std::result::Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label} schema must be {expected}"))
    }
}

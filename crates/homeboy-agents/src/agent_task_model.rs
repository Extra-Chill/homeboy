//! Shared model-identifier semantics for agent-task provenance.

/// Return a concrete model identifier suitable for durable provenance.
///
/// Callers own their boundary-specific error field and message so public CLI
/// contracts do not inherit internal domain terminology.
pub fn normalize_concrete_model_identifier(value: &str) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty()
        || value != normalized
        || value.chars().any(char::is_control)
        || matches!(
            normalized.to_ascii_lowercase().as_str(),
            "not recorded"
                | "unknown"
                | "ai-assisted"
                | "ai assisted"
                | "legacy caller did not record a model"
        )
    {
        return None;
    }
    Some(normalized.to_string())
}

#[cfg(test)]
mod tests {
    use super::normalize_concrete_model_identifier;

    #[test]
    fn concrete_model_identifier_normalizes_only_valid_provenance() {
        assert_eq!(
            normalize_concrete_model_identifier("openai/gpt-5.6-terra"),
            Some("openai/gpt-5.6-terra".to_string())
        );
        for value in [
            "",
            " unknown ",
            "not recorded",
            "AI-assisted",
            "legacy caller did not record a model",
            "model\nidentifier",
        ] {
            assert_eq!(
                normalize_concrete_model_identifier(value),
                None,
                "{value:?}"
            );
        }
    }
}

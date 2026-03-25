//! name_segment_clustering — extracted from decompose.rs.

        .collect()
}

/// Generate multi-word prefixes from a name (e.g., "extract_changes_from_diff" → ["extract_changes", "extract"]).
pub(crate) fn name_prefixes(name: &str) -> Vec<String> {
    let parts: Vec<&str> = name.split('_').filter(|s| !s.is_empty()).collect();
    let mut prefixes = Vec::new();

    // 2-word prefix (most specific)
    if parts.len() >= 2 {
        prefixes.push(format!("{}_{}", parts[0], parts[1]).to_lowercase());
    }
    // 1-word prefix
    if !parts.is_empty() && parts[0].len() > 1 {
        prefixes.push(parts[0].to_lowercase());
    }

    prefixes
}

/// Words that are too generic to be useful as cluster names.
pub(crate) fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "get"
            | "set"
            | "new"
            | "is"
            | "has"
            | "the"
            | "for"
            | "from"
            | "into"
            | "with"
            | "to"
            | "in"
            | "of"
            | "fn"
            | "pub"
            | "run"
            | "do"
            | "make"
            | "on"
            | "by"
            | "or"
            | "an"
            | "at"
            | "no"
            | "not"
            | "can"
            | "all"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_segments_default_path() {

        let result = name_segments();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_name_prefixes_default_path() {

        let result = name_prefixes();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_name_prefixes_has_expected_effects() {
        // Expected effects: mutation

        let _ = name_prefixes();
    }

    #[test]
    fn test_is_stop_word_default_path() {

        let _result = is_stop_word();
    }

}

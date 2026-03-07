use std::collections::HashMap;

/// Detect the common naming suffix among a set of class/type names.
///
/// If most names end in `Ability`, returns `Some("Ability")`.
pub(crate) fn detect_naming_suffix(names: &[String]) -> Option<String> {
    if names.len() < 2 {
        return None;
    }

    let mut suffix_counts: HashMap<String, usize> = HashMap::new();

    for name in names {
        if let Some(suffix) = extract_class_suffix(name) {
            *suffix_counts.entry(suffix).or_insert(0) += 1;
        }
    }

    let threshold = (names.len() as f32 * 0.6).ceil() as usize;
    suffix_counts
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .max_by_key(|(_, count)| *count)
        .map(|(suffix, _)| suffix)
}

/// Extract the class-style suffix from a PascalCase name.
///
/// `FlowAbility` → `Ability`
/// `FlowHelpers` → `Helpers`
pub(crate) fn extract_class_suffix(name: &str) -> Option<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut last_upper_start = None;

    for (i, ch) in chars.iter().enumerate() {
        if ch.is_uppercase() && i > 0 {
            last_upper_start = Some(i);
        }
    }

    last_upper_start.map(|i| chars[i..].iter().collect())
}

/// Check if a candidate name matches a detected suffix, with plural tolerance.
pub(crate) fn suffix_matches(candidate: &str, suffix: &str) -> bool {
    if candidate.ends_with(suffix) {
        return true;
    }

    let plural_suffix = pluralize(suffix);
    if candidate.ends_with(&plural_suffix) {
        return true;
    }

    if let Some(singular) = singularize(suffix) {
        if candidate.ends_with(&singular) {
            return true;
        }
    }

    false
}

pub(crate) fn pluralize(word: &str) -> String {
    if word.ends_with('y')
        && !word.ends_with("ey")
        && !word.ends_with("ay")
        && !word.ends_with("oy")
    {
        format!("{}ies", &word[..word.len() - 1])
    } else if word.ends_with('s')
        || word.ends_with('x')
        || word.ends_with("ch")
        || word.ends_with("sh")
    {
        format!("{}es", word)
    } else {
        format!("{}s", word)
    }
}

pub(crate) fn singularize(word: &str) -> Option<String> {
    if word.ends_with("ies") && word.len() > 3 {
        Some(format!("{}y", &word[..word.len() - 3]))
    } else if word.ends_with("ses")
        || word.ends_with("xes")
        || word.ends_with("ches")
        || word.ends_with("shes")
    {
        Some(word[..word.len() - 2].to_string())
    } else if word.ends_with('s') && !word.ends_with("ss") && word.len() > 1 {
        Some(word[..word.len() - 1].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_naming_suffix_majority() {
        let names = vec![
            "CreateFlowAbility".to_string(),
            "UpdateFlowAbility".to_string(),
            "DeleteFlowAbility".to_string(),
            "FlowHelpers".to_string(),
        ];

        assert_eq!(detect_naming_suffix(&names), Some("Ability".to_string()));
    }

    #[test]
    fn extract_class_suffix_pascal_case() {
        assert_eq!(
            extract_class_suffix("CreateFlowAbility"),
            Some("Ability".to_string())
        );
        assert_eq!(
            extract_class_suffix("FlowHelpers"),
            Some("Helpers".to_string())
        );
        assert_eq!(
            extract_class_suffix("BlockSanitizer"),
            Some("Sanitizer".to_string())
        );
    }

    #[test]
    fn suffix_matches_exact() {
        assert!(suffix_matches("CreateFlowAbility", "Ability"));
        assert!(suffix_matches("WebhookTriggerAbility", "Ability"));
        assert!(!suffix_matches("FlowHelpers", "Ability"));
    }

    #[test]
    fn suffix_matches_plural_tolerance() {
        assert!(suffix_matches("GitHubAbilities", "Ability"));
        assert!(suffix_matches("FetchAbilities", "Ability"));
        assert!(suffix_matches("CreateFlowAbility", "Abilities"));
    }

    #[test]
    fn suffix_matches_simple_plural() {
        assert!(suffix_matches("AllTests", "Test"));
        assert!(suffix_matches("SingleTest", "Tests"));
        assert!(suffix_matches("AuthProviders", "Provider"));
    }

    #[test]
    fn suffix_matches_rejects_unrelated() {
        assert!(!suffix_matches("FlowHelpers", "Ability"));
        assert!(!suffix_matches("BlockSanitizer", "Ability"));
        assert!(!suffix_matches("EngineHelpers", "Tool"));
    }
}

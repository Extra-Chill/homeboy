use std::cmp::Ordering;

pub fn normalize_identifier(input: &str) -> String {
    input.trim().to_lowercase()
}

pub fn identifier_eq(a: &str, b: &str) -> bool {
    normalize_identifier(a) == normalize_identifier(b)
}

pub fn normalize_doc_segment(input: &str) -> String {
    input.trim().to_lowercase().replace([' ', '\t'], "-")
}

pub fn cmp_case_insensitive(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_identifier_trims_and_lowercases() {
        assert_eq!(normalize_identifier("  HeLLo  "), "hello");
    }

    #[test]
    fn identifier_eq_is_case_insensitive_and_trims() {
        assert!(identifier_eq("  My-Site ", "my-site"));
    }

    #[test]
    fn normalize_doc_segment_replaces_spaces_and_tabs_with_dashes() {
        assert_eq!(normalize_doc_segment("  My Topic\tName "), "my-topic-name");
    }

    #[test]
    fn cmp_case_insensitive_sorts_without_caring_about_case() {
        let mut values = vec!["b", "A", "c"];
        values.sort_by(|a, b| cmp_case_insensitive(a, b));
        assert_eq!(values, vec!["A", "b", "c"]);
    }
}

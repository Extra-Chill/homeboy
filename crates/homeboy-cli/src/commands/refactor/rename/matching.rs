//! Term matchers used by the rename engine: boundary-aware, literal, and
//! case-insensitive (for discovering the casing a codebase actually uses).

/// Check if a byte is a word boundary character (not alphanumeric, not underscore).
fn is_boundary_char(c: u8) -> bool {
    !c.is_ascii_alphanumeric() && c != b'_'
}

/// Find all occurrences of `term` in `text` at sensible word boundaries.
///
/// Boundary rules:
/// - Left: start of string, non-alphanumeric, underscore, or camelCase/acronym boundary
/// - Right: end of string, non-alphanumeric, underscore, or uppercase letter
///
/// Handles: word boundaries, camelCase joins, snake_case compounds, UPPER_SNAKE,
/// consecutive-uppercase acronym boundaries (WPAgent → WP|Agent).
pub fn find_boundary_matches(text: &str, term: &str) -> Vec<usize> {
    let text_bytes = text.as_bytes();
    let term_bytes = term.as_bytes();
    let term_len = term_bytes.len();
    let text_len = text_bytes.len();
    let mut matches = Vec::new();

    if term_len == 0 || term_len > text_len {
        return matches;
    }

    let mut start = 0;
    while let Some(pos) = text[start..].find(term) {
        let abs = start + pos;
        let end = abs + term_len;

        // Left boundary
        let left_ok = abs == 0
            || is_boundary_char(text_bytes[abs - 1])
            || text_bytes[abs - 1] == b'_'
            // camelCase boundary: lowercase/digit → uppercase
            || (text_bytes[abs].is_ascii_uppercase()
                && (text_bytes[abs - 1].is_ascii_lowercase()
                    || text_bytes[abs - 1].is_ascii_digit()))
            // Consecutive-uppercase boundary: uppercase → uppercase+lowercase
            // e.g., 'P' before 'A' in "WPAgent"
            || (abs >= 2
                && text_bytes[abs].is_ascii_uppercase()
                && text_bytes[abs - 1].is_ascii_uppercase()
                && term_len > 1
                && term_bytes[1].is_ascii_lowercase());

        // Right boundary
        let right_ok = end >= text_len || {
            let next = text_bytes[end];
            is_boundary_char(next) || next.is_ascii_uppercase() || next == b'_'
        };

        if left_ok && right_ok {
            matches.push(abs);
        }

        start = abs + 1;
    }

    matches
}

/// Find all occurrences of `term` in `text` using exact substring matching.
/// No boundary detection — every occurrence is returned.
pub fn find_literal_matches(text: &str, term: &str) -> Vec<usize> {
    let mut matches = Vec::new();
    let term_len = term.len();
    if term_len == 0 {
        return matches;
    }
    let mut start = 0;
    while let Some(pos) = text[start..].find(term) {
        let abs = start + pos;
        matches.push(abs);
        start = abs + 1;
    }
    matches
}

/// Find all occurrences of `term` in `text` using case-insensitive matching,
/// returning the actual text that was found (preserving original casing).
///
/// This is used for variant discovery — when a generated variant like `WpAgent`
/// has 0 matches, this function finds `WPAgent` (the actual casing in the codebase).
pub fn find_case_insensitive_matches(text: &str, term: &str) -> Vec<(usize, String)> {
    let text_lower = text.to_lowercase();
    let term_lower = term.to_lowercase();
    let term_len = term.len();
    let mut matches = Vec::new();

    if term_len == 0 || term_len > text.len() {
        return matches;
    }

    let mut start = 0;
    while let Some(pos) = text_lower[start..].find(&term_lower) {
        let abs = start + pos;
        let actual = &text[abs..abs + term_len];
        matches.push((abs, actual.to_string()));
        start = abs + 1;
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Boundary matching tests ---

    #[test]
    fn boundary_matches_word_boundaries() {
        assert_eq!(find_boundary_matches("pub mod widget;", "widget"), vec![8]);
        assert_eq!(find_boundary_matches("load_widget", "widget"), vec![5]);
        assert_eq!(find_boundary_matches("WIDGET_DIR", "WIDGET"), vec![0]);
    }

    #[test]
    fn boundary_rejects_partial_match() {
        assert!(find_boundary_matches("widgetry", "widget").is_empty());
        assert!(find_boundary_matches("rewidget", "widget").is_empty());
    }

    #[test]
    fn boundary_matches_camel_case() {
        assert_eq!(find_boundary_matches("WidgetManifest", "Widget"), vec![0]);
        assert_eq!(find_boundary_matches("loadWidget", "Widget"), vec![4]);
    }

    #[test]
    fn boundary_matches_consecutive_uppercase() {
        // WPAgent → WP at 0, Agent at 2
        assert_eq!(find_boundary_matches("WPAgent", "Agent"), vec![2]);
        assert_eq!(find_boundary_matches("WPAgent", "WP"), vec![0]);
        assert_eq!(find_boundary_matches("XMLParser", "XML"), vec![0]);
        assert_eq!(find_boundary_matches("XMLParser", "Parser"), vec![3]);
    }

    // --- Literal matching tests ---

    #[test]
    fn literal_matches_exact_substring() {
        assert_eq!(find_literal_matches("the-widget-plugin", "widget"), vec![4]);
        // Literal matches inside words (no boundaries)
        assert_eq!(find_literal_matches("widgetry", "widget"), vec![0]);
    }

    // --- Case-insensitive matching tests ---

    #[test]
    fn case_insensitive_finds_different_casing() {
        let matches = find_case_insensitive_matches("namespace WPAgent;", "wpagent");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, 10); // position
        assert_eq!(matches[0].1, "WPAgent"); // actual casing preserved
    }

    #[test]
    fn case_insensitive_finds_multiple_casings() {
        let text = "WPAgent and wpagent and Wpagent";
        let matches = find_case_insensitive_matches(text, "wpagent");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].1, "WPAgent");
        assert_eq!(matches[1].1, "wpagent");
        assert_eq!(matches[2].1, "Wpagent");
    }
}

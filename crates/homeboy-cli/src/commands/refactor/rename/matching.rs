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

    #[test]
    fn boundary_matches_respect_word_case_and_acronym_boundaries() {
        let cases: &[(&str, &str, &[usize])] = &[
            // word, snake_case and UPPER_SNAKE boundaries
            ("pub mod widget;", "widget", &[8]),
            ("load_widget", "widget", &[5]),
            ("is_widget_linked", "widget", &[3]),
            ("widget_init", "widget", &[0]),
            ("WIDGET_DIR", "WIDGET", &[0]),
            ("THE_WIDGET_CONFIG", "WIDGET", &[4]),
            ("Plugin: WP Agent v1", "WP Agent", &[8]),
            // camelCase joins
            ("WidgetManifest", "Widget", &[0]),
            ("loadWidget", "Widget", &[4]),
            // consecutive-uppercase acronym boundaries
            ("WPAgent", "Agent", &[2]),
            ("WPAgent", "WP", &[0]),
            ("XMLParser", "XML", &[0]),
            ("XMLParser", "Parser", &[3]),
            // partial words never match
            ("widgetry", "widget", &[]),
            ("rewidget", "widget", &[]),
            ("node_modules", "module", &[]),
            ("node_modules", "modules", &[5]),
        ];
        for (text, term, expected) in cases {
            assert_eq!(
                find_boundary_matches(text, term),
                expected.to_vec(),
                "boundary match of {term:?} in {text:?}"
            );
        }
    }

    #[test]
    fn literal_matches_every_exact_substring() {
        let cases: &[(&str, &str, &[usize])] = &[
            ("the-widget-plugin", "widget", &[4]),
            // no boundary filtering
            ("widgetry", "widget", &[0]),
            (
                "sampleplugin-events and sampleplugin-events",
                "sampleplugin-events",
                &[0, 24],
            ),
            ("sample-plugin-events", "sampleplugin-events", &[]),
        ];
        for (text, term, expected) in cases {
            assert_eq!(
                find_literal_matches(text, term),
                expected.to_vec(),
                "literal match of {term:?} in {text:?}"
            );
        }
    }

    #[test]
    fn case_insensitive_matches_preserve_actual_casing() {
        let matches =
            find_case_insensitive_matches("namespace WPAgent; wpagent and Wpagent", "wpagent");
        assert_eq!(
            matches,
            vec![
                (10, "WPAgent".to_string()),
                (19, "wpagent".to_string()),
                (31, "Wpagent".to_string()),
            ]
        );
    }
}

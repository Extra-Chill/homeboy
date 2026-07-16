//! Pure parser/renderer helpers for sectioned GitHub PR comments.

/// Does `body` carry the comment-key marker?
pub(super) fn comment_matches_key(body: &str, comment_key: &str) -> bool {
    let markers = comment_key_markers(comment_key);
    markers.iter().any(|m| body.contains(m.as_str()))
}

/// Parse section blocks out of a comment body.
///
/// Returns an ordered `Vec<(key, body)>` in the order encountered. Keys are
/// trimmed; bodies have leading/trailing newlines stripped. Unpaired or
/// malformed markers are skipped silently (no panic, no error) so the merge
/// loop can always make forward progress even if a comment body was hand-
/// edited.
pub(crate) fn parse_comment_sections(body: &str) -> Vec<(String, String)> {
    // The `regex` crate does not support backreferences, so we capture both
    // start-key and end-key and verify equality post-match.
    let re = regex::Regex::new(
        r"(?s)<!-- homeboy:section-key=([^:]*?):start -->\n?(.*?)\n?<!-- homeboy:section-key=([^:]*?):end -->",
    )
    .expect("section-marker regex is valid");

    let mut out: Vec<(String, String)> = Vec::new();
    for caps in re.captures_iter(body) {
        let start_key = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        let end_key = caps.get(3).map(|m| m.as_str().trim()).unwrap_or("");
        if start_key.is_empty() || start_key != end_key {
            // Unmatched or unnamed — skip.
            continue;
        }
        let inner = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let inner = inner.trim_matches('\n').to_string();
        out.push((start_key.to_string(), inner));
    }
    out
}

/// Extract the header line(s) of a comment — everything between the outer
/// marker and the first section marker, minus the outer marker itself.
///
/// Used to preserve an existing header when merging (so we don't clobber
/// `## Homeboy Results — <component>` that an earlier invocation wrote).
pub(super) fn extract_header(body: &str) -> Option<String> {
    // Find the end of the outer marker line.
    let outer_end = body.find("-->\n")?;
    let after_outer = &body[outer_end + 4..];
    let first_section_idx = after_outer.find("<!-- homeboy:section-key=")?;
    let header = after_outer[..first_section_idx].trim_matches('\n').trim();
    if header.is_empty() {
        None
    } else {
        Some(header.to_string())
    }
}

/// Extract the footer block of a comment — content between
/// `<!-- homeboy:footer:start -->` and `<!-- homeboy:footer:end -->`.
///
/// Used to preserve an existing footer when merging.
pub(super) fn extract_footer(body: &str) -> Option<String> {
    const START: &str = "<!-- homeboy:footer:start -->";
    const END: &str = "<!-- homeboy:footer:end -->";
    let start_idx = body.find(START)?;
    let after_start = &body[start_idx + START.len()..];
    let end_idx = after_start.find(END)?;
    let inner = after_start[..end_idx].trim_matches('\n');
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

/// Merge `(section_key, body)` into `sections`. Replaces any existing entry
/// for `section_key`, preserving the original position; otherwise appends.
pub(crate) fn merge_section(
    mut sections: Vec<(String, String)>,
    section_key: &str,
    body: String,
) -> Vec<(String, String)> {
    for entry in sections.iter_mut() {
        if entry.0 == section_key {
            entry.1 = body;
            return sections;
        }
    }
    sections.push((section_key.to_string(), body));
    sections
}

/// Render a comment body from a set of sections.
///
/// - `comment_key` → outer marker (new format).
/// - `header` → optional line(s) written after the outer marker.
/// - `sections` → section map. Insertion order is preserved only when
///   `explicit_order` is `None`; otherwise explicit-ordered keys come first
///   in the given order, and any remaining keys follow alphabetically.
/// - `footer` → optional block written after the last section, wrapped in
///   dedicated `<!-- homeboy:footer:start|end -->` markers.
/// - Output is always newline-normalized with a trailing newline.
pub(crate) fn render_comment(
    comment_key: &str,
    header: Option<&str>,
    sections: &[(String, String)],
    explicit_order: Option<&[String]>,
    footer: Option<&str>,
) -> String {
    let ordered = order_sections(sections, explicit_order);

    let mut out = String::new();
    out.push_str(&format!("<!-- homeboy:comment-key={} -->\n", comment_key));
    if let Some(h) = header {
        let h = h.trim_matches('\n');
        if !h.is_empty() {
            out.push_str(h);
            out.push('\n');
            out.push('\n');
        }
    }

    let has_footer = footer
        .map(|f| !f.trim_matches('\n').is_empty())
        .unwrap_or(false);

    for (idx, (key, body)) in ordered.iter().enumerate() {
        out.push_str(&format!("<!-- homeboy:section-key={}:start -->\n", key));
        let body_trimmed = body.trim_matches('\n');
        if !body_trimmed.is_empty() {
            out.push_str(body_trimmed);
            out.push('\n');
        }
        out.push_str(&format!("<!-- homeboy:section-key={}:end -->", key));
        // Blank line between sections, and between the last section and a
        // footer block. Trailing newline on the last line of the comment.
        if idx + 1 < ordered.len() || has_footer {
            out.push_str("\n\n");
        } else {
            out.push('\n');
        }
    }

    if let Some(f) = footer {
        let f = f.trim_matches('\n');
        if !f.is_empty() {
            out.push_str("<!-- homeboy:footer:start -->\n");
            out.push_str(f);
            out.push('\n');
            out.push_str("<!-- homeboy:footer:end -->\n");
        }
    }

    out
}

/// Apply the ordering rule: explicit-ordered keys first (in given order),
/// remaining keys alphabetically. Unknown keys in `explicit_order` are
/// silently dropped (nothing to render).
fn order_sections<'a>(
    sections: &'a [(String, String)],
    explicit_order: Option<&[String]>,
) -> Vec<&'a (String, String)> {
    match explicit_order {
        Some(order) => {
            let mut out: Vec<&(String, String)> = Vec::new();
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

            // 1. Keys in explicit order (only if present in sections).
            for key in order {
                if let Some(entry) = sections.iter().find(|(k, _)| k == key) {
                    out.push(entry);
                    seen.insert(entry.0.as_str());
                }
            }

            // 2. Remaining keys, alphabetical.
            let mut leftovers: Vec<&(String, String)> = sections
                .iter()
                .filter(|(k, _)| !seen.contains(k.as_str()))
                .collect();
            leftovers.sort_by(|a, b| a.0.cmp(&b.0));
            out.extend(leftovers);

            out
        }
        None => {
            // Pure alphabetical.
            let mut out: Vec<&(String, String)> = sections.iter().collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        }
    }
}

/// Outer-key marker (start-of-body anchor for the shared comment).
fn comment_key_markers(comment_key: &str) -> [String; 1] {
    [format!("<!-- homeboy:comment-key={} -->", comment_key)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sections_new_markers() {
        let body = "\
<!-- homeboy:comment-key=ci:homeboy -->
## Homeboy Results — `homeboy`

<!-- homeboy:section-key=lint:start -->
:white_check_mark: **lint**
<!-- homeboy:section-key=lint:end -->

<!-- homeboy:section-key=test:start -->
:x: **test**
1 failure
<!-- homeboy:section-key=test:end -->
";
        let sections = parse_comment_sections(body);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "lint");
        assert_eq!(sections[0].1, ":white_check_mark: **lint**");
        assert_eq!(sections[1].0, "test");
        assert!(sections[1].1.contains(":x: **test**"));
        assert!(sections[1].1.contains("1 failure"));
    }

    #[test]
    fn parse_sections_returns_empty_for_unmarkered_body() {
        let body = "Just a regular PR comment.\n\nNo markers here.\n";
        assert!(parse_comment_sections(body).is_empty());
    }

    #[test]
    fn parse_sections_skips_malformed_blocks() {
        // Start without matching end — should be ignored, not panic.
        let body = "\
<!-- homeboy:section-key=lint:start -->
never-ends
";
        assert!(parse_comment_sections(body).is_empty());
    }

    #[test]
    fn render_comment_writes_new_markers() {
        let sections = vec![
            ("lint".to_string(), "lint body".to_string()),
            ("test".to_string(), "test body".to_string()),
        ];
        let out = render_comment("ci:homeboy", Some("## Header"), &sections, None, None);
        assert!(out.starts_with("<!-- homeboy:comment-key=ci:homeboy -->\n"));
        assert!(out.contains("## Header"));
        assert!(out.contains("<!-- homeboy:section-key=lint:start -->"));
        assert!(out.contains("<!-- homeboy:section-key=lint:end -->"));
        assert!(out.contains("<!-- homeboy:section-key=test:start -->"));
    }

    #[test]
    fn render_comment_round_trips_through_parse() {
        let sections = vec![
            ("audit".to_string(), "audit body\nmulti-line".to_string()),
            ("lint".to_string(), "lint body".to_string()),
        ];
        let rendered = render_comment("ci:x", None, &sections, None, None);
        let reparsed = parse_comment_sections(&rendered);

        // Alphabetical default → audit before lint.
        assert_eq!(reparsed.len(), 2);
        assert_eq!(reparsed[0].0, "audit");
        assert_eq!(reparsed[0].1, "audit body\nmulti-line");
        assert_eq!(reparsed[1].0, "lint");
    }

    #[test]
    fn render_comment_alphabetical_by_default() {
        let sections = vec![
            ("test".to_string(), "t".to_string()),
            ("audit".to_string(), "a".to_string()),
            ("lint".to_string(), "l".to_string()),
        ];
        let out = render_comment("k", None, &sections, None, None);
        let audit_pos = out.find("section-key=audit:start").unwrap();
        let lint_pos = out.find("section-key=lint:start").unwrap();
        let test_pos = out.find("section-key=test:start").unwrap();
        assert!(audit_pos < lint_pos);
        assert!(lint_pos < test_pos);
    }

    #[test]
    fn render_comment_honors_explicit_order() {
        let sections = vec![
            ("audit".to_string(), "a".to_string()),
            ("lint".to_string(), "l".to_string()),
            ("test".to_string(), "t".to_string()),
        ];
        let order = vec!["lint".to_string(), "test".to_string(), "audit".to_string()];
        let out = render_comment("k", None, &sections, Some(&order), None);
        let lint_pos = out.find("section-key=lint:start").unwrap();
        let test_pos = out.find("section-key=test:start").unwrap();
        let audit_pos = out.find("section-key=audit:start").unwrap();
        assert!(lint_pos < test_pos);
        assert!(test_pos < audit_pos);
    }

    #[test]
    fn render_comment_unknown_keys_appended_alphabetically() {
        let sections = vec![
            ("zeta".to_string(), "z".to_string()),
            ("alpha".to_string(), "a".to_string()),
            ("lint".to_string(), "l".to_string()),
            ("test".to_string(), "t".to_string()),
        ];
        // Only lint+test in explicit order — zeta and alpha are "unknown".
        let order = vec!["lint".to_string(), "test".to_string()];
        let out = render_comment("k", None, &sections, Some(&order), None);

        let lint_pos = out.find("section-key=lint:start").unwrap();
        let test_pos = out.find("section-key=test:start").unwrap();
        let alpha_pos = out.find("section-key=alpha:start").unwrap();
        let zeta_pos = out.find("section-key=zeta:start").unwrap();

        // Explicit-order keys come first in their listed order.
        assert!(lint_pos < test_pos);
        // Unknown keys appended after, alphabetical among themselves.
        assert!(test_pos < alpha_pos);
        assert!(alpha_pos < zeta_pos);
    }

    #[test]
    fn render_comment_explicit_order_ignores_missing_keys() {
        let sections = vec![("lint".to_string(), "l".to_string())];
        // `test` is in the order but not present in sections — should not appear
        // in output.
        let order = vec!["test".to_string(), "lint".to_string()];
        let out = render_comment("k", None, &sections, Some(&order), None);
        assert!(!out.contains("section-key=test:start"));
        assert!(out.contains("section-key=lint:start"));
    }

    #[test]
    fn merge_section_replaces_existing_preserves_position() {
        let sections = vec![
            ("lint".to_string(), "old lint".to_string()),
            ("test".to_string(), "old test".to_string()),
        ];
        let merged = merge_section(sections, "lint", "new lint".to_string());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].0, "lint");
        assert_eq!(merged[0].1, "new lint");
        assert_eq!(merged[1].0, "test");
        assert_eq!(merged[1].1, "old test");
    }

    #[test]
    fn merge_section_appends_when_absent() {
        let sections = vec![("lint".to_string(), "lint".to_string())];
        let merged = merge_section(sections, "test", "test".to_string());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].0, "test");
        assert_eq!(merged[1].1, "test");
    }

    #[test]
    fn comment_matches_key_recognizes_comment_key_marker() {
        let new_body = "<!-- homeboy:comment-key=ci:x -->\nbody\n";
        let unrelated = "<!-- homeboy:comment-key=ci:y -->\nbody\n";
        let unmarked = "just a comment\n";

        assert!(comment_matches_key(new_body, "ci:x"));
        assert!(!comment_matches_key(unrelated, "ci:x"));
        assert!(!comment_matches_key(unmarked, "ci:x"));
    }

    #[test]
    fn extract_header_reads_between_markers() {
        let body = "\
<!-- homeboy:comment-key=ci:x -->
## Homeboy Results — `homeboy`

<!-- homeboy:section-key=lint:start -->
body
<!-- homeboy:section-key=lint:end -->
";
        assert_eq!(
            extract_header(body),
            Some("## Homeboy Results — `homeboy`".to_string())
        );
    }

    #[test]
    fn extract_header_empty_when_no_header_text() {
        let body = "\
<!-- homeboy:comment-key=ci:x -->
<!-- homeboy:section-key=lint:start -->
body
<!-- homeboy:section-key=lint:end -->
";
        assert_eq!(extract_header(body), None);
    }

    #[test]
    fn render_comment_writes_footer_block_after_last_section() {
        let sections = vec![("lint".to_string(), "lint body".to_string())];
        let out = render_comment(
            "ci:x",
            Some("## Header"),
            &sections,
            None,
            Some("tooling versions block"),
        );
        // Footer markers present, one blank line before start marker, trailing
        // newline after end marker.
        assert!(out.contains(
            "<!-- homeboy:footer:start -->\ntooling versions block\n<!-- homeboy:footer:end -->\n"
        ));
        // Footer appears after the last section's :end marker.
        let last_section_end = out.find("<!-- homeboy:section-key=lint:end -->").unwrap();
        let footer_start = out.find("<!-- homeboy:footer:start -->").unwrap();
        assert!(last_section_end < footer_start);
        // Exactly one blank line (=\n\n) between section end and footer start.
        let between = &out[last_section_end..footer_start];
        assert!(between.ends_with("\n\n"));
    }

    #[test]
    fn render_comment_without_footer_omits_footer_markers() {
        let sections = vec![("lint".to_string(), "lint body".to_string())];
        let out = render_comment("ci:x", None, &sections, None, None);
        assert!(!out.contains("homeboy:footer:start"));
        assert!(!out.contains("homeboy:footer:end"));
    }

    #[test]
    fn render_comment_empty_footer_string_omits_footer_markers() {
        // Passing Some("") or Some("\n") should behave like None — no footer.
        let sections = vec![("lint".to_string(), "lint body".to_string())];
        for probe in ["", "\n", "\n\n"] {
            let out = render_comment("ci:x", None, &sections, None, Some(probe));
            assert!(
                !out.contains("homeboy:footer:start"),
                "footer markers should be omitted for empty footer '{:?}'",
                probe
            );
        }
    }

    #[test]
    fn extract_footer_reads_block_between_markers() {
        let body = "\
<!-- homeboy:comment-key=ci:x -->
## Header

<!-- homeboy:section-key=lint:start -->
lint body
<!-- homeboy:section-key=lint:end -->

<!-- homeboy:footer:start -->
tooling block line 1
tooling block line 2
<!-- homeboy:footer:end -->
";
        assert_eq!(
            extract_footer(body),
            Some("tooling block line 1\ntooling block line 2".to_string())
        );
    }

    #[test]
    fn extract_footer_returns_none_when_markers_absent() {
        let body = "\
<!-- homeboy:comment-key=ci:x -->
<!-- homeboy:section-key=lint:start -->
body
<!-- homeboy:section-key=lint:end -->
";
        assert_eq!(extract_footer(body), None);
    }

    #[test]
    fn extract_footer_empty_inner_returns_none() {
        let body = "\
<!-- homeboy:footer:start -->
<!-- homeboy:footer:end -->
";
        assert_eq!(extract_footer(body), None);
    }

    #[test]
    fn render_comment_round_trips_footer_through_parse() {
        // Rendering with a footer, then extract_footer on the output, should
        // return the same footer content.
        let sections = vec![("lint".to_string(), "body".to_string())];
        let footer = "- Homeboy CLI: `1.2.3`\n- Action: `repo@v1`";
        let rendered = render_comment("ci:x", None, &sections, None, Some(footer));
        assert_eq!(extract_footer(&rendered), Some(footer.to_string()));
        // And sections should still round-trip.
        let parsed = parse_comment_sections(&rendered);
        assert_eq!(parsed, vec![("lint".to_string(), "body".to_string())]);
    }

    #[test]
    fn render_comment_footer_with_multiple_sections_alphabetical() {
        // Footer sits after the last section regardless of section ordering.
        let sections = vec![
            ("lint".to_string(), "l".to_string()),
            ("audit".to_string(), "a".to_string()),
            ("test".to_string(), "t".to_string()),
        ];
        let out = render_comment("ci:x", None, &sections, None, Some("FTR"));
        // Alphabetical order: audit, lint, test; footer last.
        let audit_pos = out.find("section-key=audit:end").unwrap();
        let lint_pos = out.find("section-key=lint:end").unwrap();
        let test_pos = out.find("section-key=test:end").unwrap();
        let footer_pos = out.find("homeboy:footer:start").unwrap();
        assert!(audit_pos < lint_pos);
        assert!(lint_pos < test_pos);
        assert!(test_pos < footer_pos);
    }

    #[test]
    fn render_comment_footer_with_explicit_section_order() {
        let sections = vec![
            ("audit".to_string(), "a".to_string()),
            ("lint".to_string(), "l".to_string()),
        ];
        let order = vec!["lint".to_string(), "audit".to_string()];
        let out = render_comment("ci:x", None, &sections, Some(&order), Some("FTR"));
        let lint_pos = out.find("section-key=lint:end").unwrap();
        let audit_pos = out.find("section-key=audit:end").unwrap();
        let footer_pos = out.find("homeboy:footer:start").unwrap();
        assert!(lint_pos < audit_pos);
        assert!(audit_pos < footer_pos);
    }

    #[test]
    fn render_comment_footer_content_trimmed_of_surrounding_newlines() {
        // A footer passed with leading/trailing newlines should render cleanly
        // (no double blank lines).
        let sections = vec![("lint".to_string(), "body".to_string())];
        let out = render_comment("ci:x", None, &sections, None, Some("\n\nFTR\n\n"));
        assert!(out.contains("<!-- homeboy:footer:start -->\nFTR\n<!-- homeboy:footer:end -->\n"));
        assert!(!out.contains("start -->\n\nFTR"));
        assert!(!out.contains("FTR\n\n<!-- homeboy:footer:end"));
    }

    #[test]
    fn parse_sections_ignores_footer_block() {
        // A comment with both sections and a footer should parse only the
        // sections — the footer is not a section.
        let body = "\
<!-- homeboy:comment-key=ci:x -->
<!-- homeboy:section-key=lint:start -->
lint body
<!-- homeboy:section-key=lint:end -->

<!-- homeboy:footer:start -->
tooling
<!-- homeboy:footer:end -->
";
        let sections = parse_comment_sections(body);
        assert_eq!(
            sections,
            vec![("lint".to_string(), "lint body".to_string())]
        );
    }
}

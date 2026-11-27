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

    super::neutralize_mentions(&out)
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

    fn sections(keys: &[&str]) -> Vec<(String, String)> {
        keys.iter()
            .map(|key| (key.to_string(), format!("{key} body")))
            .collect()
    }

    fn rendered_order(out: &str) -> Vec<String> {
        parse_comment_sections(out)
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    }

    #[test]
    fn merged_comment_protects_all_prose_including_preserved_sections() {
        let sections = vec![("old".into(), "@stranger `@code` a@b.com".into())];
        let out = render_comment("ci", Some("@header"), &sections, None, Some("@footer"));
        assert!(out.contains("@\u{200b}stranger `@code` a@b.com"));
        assert!(out.contains("@\u{200b}header"));
        assert!(out.contains("@\u{200b}footer"));
    }

    #[test]
    fn parse_sections_reads_only_matched_section_blocks() {
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

<!-- homeboy:footer:start -->
tooling
<!-- homeboy:footer:end -->
";
        assert_eq!(
            parse_comment_sections(body),
            vec![
                (
                    "lint".to_string(),
                    ":white_check_mark: **lint**".to_string()
                ),
                ("test".to_string(), ":x: **test**\n1 failure".to_string()),
            ]
        );
        for body in [
            "Just a regular PR comment.\n\nNo markers here.\n",
            "<!-- homeboy:section-key=lint:start -->\nnever-ends\n",
        ] {
            assert!(parse_comment_sections(body).is_empty(), "{body}");
        }
    }

    #[test]
    fn render_comment_round_trips_header_sections_and_footer() {
        let sections = vec![
            ("lint".to_string(), "lint body".to_string()),
            ("audit".to_string(), "audit body\nmulti-line".to_string()),
        ];
        let footer = "- Homeboy CLI: `1.2.3`\n- Action: `repo@v1`";
        let rendered = render_comment(
            "ci:homeboy",
            Some("## Homeboy Results — `homeboy`"),
            &sections,
            None,
            Some(footer),
        );

        assert!(rendered.starts_with("<!-- homeboy:comment-key=ci:homeboy -->\n"));
        assert!(comment_matches_key(&rendered, "ci:homeboy"));
        assert!(!comment_matches_key(&rendered, "ci:other"));
        assert_eq!(
            extract_header(&rendered).as_deref(),
            Some("## Homeboy Results — `homeboy`")
        );
        assert_eq!(
            parse_comment_sections(&rendered),
            vec![
                ("audit".to_string(), "audit body\nmulti-line".to_string()),
                ("lint".to_string(), "lint body".to_string()),
            ]
        );
        assert_eq!(extract_footer(&rendered).as_deref(), Some(footer));
    }

    #[test]
    fn render_comment_orders_explicit_keys_first_then_alphabetically() {
        for (keys, order, expected) in [
            (
                vec!["test", "audit", "lint"],
                None,
                vec!["audit", "lint", "test"],
            ),
            (
                vec!["audit", "lint", "test"],
                Some(vec!["lint", "test", "audit"]),
                vec!["lint", "test", "audit"],
            ),
            (
                vec!["zeta", "alpha", "lint", "test"],
                Some(vec!["lint", "test"]),
                vec!["lint", "test", "alpha", "zeta"],
            ),
            (vec!["lint"], Some(vec!["test", "lint"]), vec!["lint"]),
        ] {
            let order: Option<Vec<String>> =
                order.map(|keys| keys.into_iter().map(String::from).collect());
            let out = render_comment("k", None, &sections(&keys), order.as_deref(), Some("FTR"));
            assert_eq!(rendered_order(&out), expected, "{keys:?} / {order:?}");
            assert!(out.rfind("section-key=").unwrap() < out.find("homeboy:footer:start").unwrap());
        }
    }

    #[test]
    fn merge_section_replaces_in_place_or_appends() {
        let merged = merge_section(sections(&["lint", "test"]), "lint", "new lint".to_string());
        assert_eq!(
            merged,
            vec![
                ("lint".to_string(), "new lint".to_string()),
                ("test".to_string(), "test body".to_string()),
            ]
        );
        let merged = merge_section(sections(&["lint"]), "test", "test".to_string());
        assert_eq!(merged[1], ("test".to_string(), "test".to_string()));
    }

    #[test]
    fn render_comment_writes_a_trimmed_footer_one_blank_line_after_the_last_section() {
        let out = render_comment(
            "ci:x",
            None,
            &sections(&["lint"]),
            None,
            Some("\n\nFTR\n\n"),
        );
        assert!(out.ends_with(
            "<!-- homeboy:section-key=lint:end -->\n\n<!-- homeboy:footer:start -->\nFTR\n<!-- homeboy:footer:end -->\n"
        ));
    }

    #[test]
    fn empty_header_and_footer_are_omitted_and_extract_as_none() {
        for footer in [None, Some(""), Some("\n"), Some("\n\n")] {
            let out = render_comment("ci:x", Some(""), &sections(&["lint"]), None, footer);
            assert!(!out.contains("homeboy:footer:start"), "{footer:?}");
            assert_eq!(extract_header(&out), None);
            assert_eq!(extract_footer(&out), None);
        }
        let empty_footer = "<!-- homeboy:footer:start -->\n<!-- homeboy:footer:end -->\n";
        assert_eq!(extract_footer(empty_footer), None);
    }
}

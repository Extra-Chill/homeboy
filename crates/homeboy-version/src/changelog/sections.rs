mod normalize_heading_label;
mod types;
mod unreleased;

pub(crate) use normalize_heading_label::extract_last_release_snapshot;
pub use normalize_heading_label::get_latest_finalized_version;
use normalize_heading_label::{is_matching_next_section_heading, validate_section_content};
use std::collections::HashSet;
use types::SectionContentStatus;
pub use unreleased::count_unreleased_entries;
pub(crate) use unreleased::get_unreleased_entries;

use chrono::Local;

use homeboy_core::engine::validation;
use homeboy_core::error::{Error, Result};

use super::settings::*;

pub(crate) fn finalize_next_section(
    changelog_content: &str,
    next_section_aliases: &[String],
    new_version: &str,
    allow_empty: bool,
) -> Result<(String, bool)> {
    if new_version.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "newVersion",
            "New version label cannot be empty",
            None,
            None,
        ));
    }

    let lines: Vec<&str> = changelog_content.lines().collect();
    let start = validation::require_with_hints(
        find_next_section_start(&lines, next_section_aliases),
        "changelog",
        &format!(
            "No unreleased changelog section found (looked for: {})",
            next_section_aliases
                .iter()
                .map(|a| format!("\"## {}\"", a))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        vec![
            "Homeboy generates entries from conventional-prefixed commits (feat:/fix:/...) at release time."
                .to_string(),
            "If the section is missing entirely, add a `## Unreleased` heading manually to your changelog."
                .to_string(),
        ],
    )?;

    let end = find_section_end(&lines, start);
    let body_lines = &lines[start + 1..end];
    let content_status = validate_section_content(body_lines);

    if content_status != SectionContentStatus::Valid {
        if allow_empty {
            return Ok((changelog_content.to_string(), false));
        }

        let message = match content_status {
            SectionContentStatus::SubsectionsOnly => {
                "Changelog has subsection headers but no bullet items"
            }
            SectionContentStatus::Empty => "Changelog has no items",
            _ => unreachable!(),
        };

        return Err(Error::validation_invalid_argument(
            "changelog",
            message,
            None,
            None,
        )
        .with_hint("Commit all changes before running version bump — homeboy generates changelog entries from conventional-prefixed commits (feat:/fix:/...) at release time."));
    }

    let mut out_lines: Vec<String> = Vec::new();

    // Copy everything before ## Unreleased.
    for line in &lines[..start] {
        out_lines.push((*line).to_string());
    }

    // Replace old ## Unreleased with ## [new_version] - date (Keep a Changelog format).
    if out_lines.last().is_some_and(|l| !l.trim().is_empty()) {
        out_lines.push(String::new());
    }
    let today = Local::now().format("%Y-%m-%d");
    out_lines.push(format!("## [{}] - {}", new_version.trim(), today));
    out_lines.push(String::new());

    // Copy everything after the old heading (body + rest of file).
    // Skip leading blank lines so the new version section starts cleanly.
    let mut started = false;
    for line in &lines[start + 1..] {
        if !started {
            if line.trim().is_empty() {
                continue;
            }
            started = true;
        }
        out_lines.push((*line).to_string());
    }

    // Ensure a blank line between the finalized section and the next heading.
    for idx in 0..out_lines.len().saturating_sub(1) {
        let is_bullet = out_lines[idx].trim_start().starts_with("- ");
        let next_is_heading = out_lines[idx + 1].trim_start().starts_with("## ");

        if is_bullet && next_is_heading {
            out_lines.insert(idx + 1, String::new());
            break;
        }
    }

    let mut out = out_lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }

    Ok((out, true))
}

/// Generate changelog entries from grouped commit data and finalize into a versioned section
/// in a single pass. No `## Unreleased` section is written to disk — entries are built in
/// memory and written directly as `## [version] - date`.
///
/// `entries_by_type` maps changelog type names (e.g. "added", "fixed") to lists of messages.
pub(crate) fn finalize_with_generated_entries(
    changelog_content: &str,
    aliases: &[String],
    entries_by_type: &std::collections::HashMap<&str, Vec<String>>,
    new_version: &str,
) -> Result<(String, bool)> {
    if entries_by_type.is_empty() || entries_by_type.values().all(|msgs| msgs.is_empty()) {
        return Ok((changelog_content.to_string(), false));
    }

    // Build entries in memory: ensure next section exists, add typed entries
    let (mut content, _) = ensure_next_section(changelog_content, aliases)?;
    let mut covered_entries = get_unreleased_entries(changelog_content, aliases);

    for (entry_type, messages) in entries_by_type {
        for message in messages {
            let trimmed = message.trim();
            if !trimmed.is_empty() {
                if generated_entry_is_covered(&covered_entries, trimmed) {
                    continue;
                }
                let (new_content, _) =
                    append_item_to_subsection(&content, aliases, trimmed, entry_type)?;
                content = new_content;
                covered_entries.push(trimmed.to_string());
            }
        }
    }

    // Finalize directly into versioned section — no intermediate disk write
    finalize_next_section(&content, aliases, new_version, false)
}

/// Add generated entries to an already finalized `## [version]` section.
///
/// Used when a release picks up commits after its changelog was finalized (the
/// release commit replayed onto an advanced remote). The section keeps its
/// heading and date; entries already covered by the section are skipped.
pub fn append_to_finalized_section(
    changelog_content: &str,
    aliases: &[String],
    version: &str,
    entries_by_type: &std::collections::HashMap<&str, Vec<String>>,
) -> Result<(String, bool)> {
    if entries_by_type.values().all(|msgs| msgs.is_empty()) {
        return Ok((changelog_content.to_string(), false));
    }

    let lines: Vec<&str> = changelog_content.lines().collect();
    if find_next_section_start(&lines, aliases).is_some() {
        return Err(Error::validation_invalid_argument(
            "changelog",
            "Changelog has an unreleased section; refusing to amend a finalized one",
            None,
            None,
        ));
    }
    let is_version_heading = |line: &str| {
        let trimmed = line.trim();
        trimmed.starts_with("## ")
            && normalize_heading_label::extract_version_from_heading(
                trimmed.trim_start_matches("## ").trim(),
            )
            .as_deref()
                == Some(version.trim())
    };
    let start = validation::require_with_hints(
        lines.iter().position(|line| is_version_heading(line)),
        "changelog",
        &format!("No finalized changelog section found for {}", version),
        Vec::new(),
    )?;

    // Reopen the section under the next-section label, reuse the generated-entry
    // path (typed subsections, dedupe), then restore the original heading.
    let label = aliases.first().map(String::as_str).unwrap_or("Unreleased");
    let original_heading = lines[start].to_string();
    let mut reopened: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    reopened[start] = format!("## {}", label);
    let (finalized, _) =
        finalize_with_generated_entries(&reopened.join("\n"), aliases, entries_by_type, version)?;

    let mut restored = false;
    let mut out_lines: Vec<String> = Vec::new();
    for line in finalized.lines() {
        if !restored && is_version_heading(line) {
            out_lines.push(original_heading.clone());
            restored = true;
        } else {
            out_lines.push(line.to_string());
        }
    }
    let mut out = out_lines.join("\n");
    if changelog_content.ends_with('\n') || !changelog_content.contains('\n') {
        out.push('\n');
    }
    let changed = out.trim_end() != changelog_content.trim_end();
    Ok((
        if changed {
            out
        } else {
            changelog_content.to_string()
        },
        changed,
    ))
}

fn generated_entry_is_covered(existing_entries: &[String], generated: &str) -> bool {
    let generated_key = normalized_changelog_entry_key(generated);
    let generated_tokens = changelog_semantic_tokens(generated);
    let generated_token_set: HashSet<&str> = generated_tokens.iter().map(String::as_str).collect();

    existing_entries.iter().any(|existing| {
        let existing_key = normalized_changelog_entry_key(existing);
        if existing_key == generated_key {
            return true;
        }

        let existing_tokens = changelog_semantic_tokens(existing);
        if generated_tokens.len() < 2 || existing_tokens.len() < generated_tokens.len() {
            return false;
        }

        if has_conflicting_action(existing) && !has_conflicting_action(generated) {
            return false;
        }

        let existing_token_set: HashSet<&str> =
            existing_tokens.iter().map(String::as_str).collect();
        generated_token_set.is_subset(&existing_token_set)
    })
}

fn normalized_changelog_entry_key(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_space = true;

    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            normalized.push(character);
            previous_was_space = false;
        } else if !previous_was_space {
            normalized.push(' ');
            previous_was_space = true;
        }
    }

    normalized.trim().to_string()
}

fn changelog_semantic_tokens(value: &str) -> Vec<String> {
    let mut tokens: Vec<String> = normalized_changelog_entry_key(value)
        .split_whitespace()
        .filter(|token| !CHANGELOG_FILLER_TOKENS.contains(token))
        .map(normalized_changelog_token)
        .collect();

    while tokens.len() > 1
        && tokens
            .first()
            .is_some_and(|token| CHANGELOG_WEAK_LEADING_ACTIONS.contains(&token.as_str()))
    {
        tokens.remove(0);
    }

    tokens
}

fn normalized_changelog_token(token: &str) -> String {
    if token.len() > 3 && token.ends_with('s') && !token.ends_with("ss") {
        token[..token.len() - 1].to_string()
    } else {
        token.to_string()
    }
}

fn has_conflicting_action(value: &str) -> bool {
    normalized_changelog_entry_key(value)
        .split_whitespace()
        .any(|token| CHANGELOG_CONFLICTING_ACTIONS.contains(&token))
}

const CHANGELOG_FILLER_TOKENS: &[&str] = &[
    "a", "an", "and", "for", "in", "of", "or", "support", "the", "to", "with",
];

const CHANGELOG_WEAK_LEADING_ACTIONS: &[&str] = &[
    "add",
    "added",
    "adds",
    "enable",
    "enabled",
    "enables",
    "implement",
    "implemented",
    "implements",
    "introduce",
    "introduced",
    "introduces",
];

const CHANGELOG_CONFLICTING_ACTIONS: &[&str] = &[
    "deprecate",
    "deprecated",
    "deprecates",
    "disable",
    "disabled",
    "disables",
    "drop",
    "dropped",
    "drops",
    "remove",
    "removed",
    "removes",
    "replace",
    "replaced",
    "replaces",
];

pub(super) fn find_next_section_start(lines: &[&str], aliases: &[String]) -> Option<usize> {
    lines
        .iter()
        .position(|line| is_matching_next_section_heading(line, aliases))
}

pub(super) fn find_section_end(lines: &[&str], start: usize) -> usize {
    let mut index = start + 1;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        // Match only H2 headers (## ), not H3 subsections (###)
        if trimmed.starts_with("## ") || trimmed == "##" {
            break;
        }
        index += 1;
    }
    index
}

pub(super) fn ensure_next_section(content: &str, aliases: &[String]) -> Result<(String, bool)> {
    let lines: Vec<&str> = content.lines().collect();
    if find_next_section_start(&lines, aliases).is_some() {
        return Ok((content.to_string(), false));
    }

    let default_label = aliases.first().map(|s| s.as_str()).unwrap_or("Unreleased");

    // Insert location: after initial "# ..." title block + optional intro paragraph,
    // but before the first version section (## <semver>).
    let mut insert_at = 0usize;

    // Keep a leading title block together.
    while insert_at < lines.len() {
        let line = lines[insert_at];
        if insert_at == 0 && line.trim().starts_with('#') {
            insert_at += 1;
            continue;
        }

        if line.trim().starts_with("##") {
            break;
        }

        insert_at += 1;
    }

    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx == insert_at {
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            if !out.ends_with("\n\n") && !out.is_empty() {
                out.push('\n');
            }
            out.push_str("## ");
            out.push_str(default_label);
            out.push_str("\n\n");
        }
        out.push_str(line);
        out.push('\n');
    }

    if insert_at >= lines.len() {
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str("## ");
        out.push_str(default_label);
        out.push('\n');
    }

    Ok((out, true))
}

pub(super) fn append_item_to_subsection(
    content: &str,
    aliases: &[String],
    message: &str,
    entry_type: &str,
) -> Result<(String, bool)> {
    let lines: Vec<&str> = content.lines().collect();
    let start = find_next_section_start(&lines, aliases).ok_or_else(|| {
        Error::internal_unexpected("Next changelog section not found (unexpected)".to_string())
    })?;

    let section_end = find_section_end(&lines, start);
    let bullet = format!("- {}", message);
    let target_header = subsection_header_from_type(entry_type);

    // Check for duplicates across entire next section
    for line in &lines[start + 1..section_end] {
        if line.trim() == bullet {
            return Ok((content.to_string(), false));
        }
    }

    // Find target subsection or determine where to insert a new one
    let mut target_subsection_idx: Option<usize> = None;
    let mut target_subsection_end: Option<usize> = None;
    let mut insert_new_subsection_at: Option<usize> = None;
    let mut found_any_subsection = false;

    // Map of subsection positions for canonical ordering
    let mut subsection_positions: Vec<(usize, &str)> = Vec::new();

    for (i, line) in lines.iter().enumerate().take(section_end).skip(start + 1) {
        let trimmed = line.trim();
        for header in KEEP_A_CHANGELOG_SUBSECTIONS {
            if trimmed.starts_with(header) {
                found_any_subsection = true;
                subsection_positions.push((i, *header));
                if trimmed.starts_with(&target_header) {
                    target_subsection_idx = Some(i);
                }
                break;
            }
        }
    }

    // If target subsection exists, find its end
    if let Some(target_idx) = target_subsection_idx {
        // Find the next subsection or section end
        target_subsection_end = Some(section_end);
        for (i, line) in lines
            .iter()
            .enumerate()
            .take(section_end)
            .skip(target_idx + 1)
        {
            let trimmed = line.trim();
            if KEEP_A_CHANGELOG_SUBSECTIONS
                .iter()
                .any(|h| trimmed.starts_with(h))
            {
                target_subsection_end = Some(i);
                break;
            }
        }
    } else if found_any_subsection {
        // Need to create subsection in canonical order
        let target_order = KEEP_A_CHANGELOG_SUBSECTIONS
            .iter()
            .position(|h| h.starts_with(&target_header))
            .unwrap_or(0);

        // Find where to insert based on canonical order
        for (pos, header) in &subsection_positions {
            let header_order = KEEP_A_CHANGELOG_SUBSECTIONS
                .iter()
                .position(|h| header.starts_with(h))
                .unwrap_or(0);
            if header_order > target_order {
                insert_new_subsection_at = Some(*pos);
                break;
            }
        }
        // If all existing subsections come before, insert at section end
        if insert_new_subsection_at.is_none() {
            insert_new_subsection_at = Some(section_end);
        }
    }

    let mut out = String::new();

    if let Some(target_idx) = target_subsection_idx {
        // Target subsection exists - insert bullet at the end of its content
        let subsection_end = target_subsection_end.unwrap_or(section_end);
        let mut insert_after = target_idx;

        // Find the last bullet in this subsection
        for (rel_i, line) in lines[target_idx + 1..subsection_end].iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('-') || trimmed.starts_with('*') {
                insert_after = target_idx + 1 + rel_i;
            }
        }

        for (idx, line) in lines.iter().enumerate() {
            out.push_str(line);
            out.push('\n');
            if idx == insert_after {
                out.push_str(&bullet);
                out.push('\n');
            }
        }
    } else if let Some(insert_at) = insert_new_subsection_at {
        // Need to create new subsection
        for (idx, line) in lines.iter().enumerate() {
            if idx == insert_at {
                // Ensure blank line before new subsection
                if !out.ends_with("\n\n") && !out.is_empty() {
                    out.push('\n');
                }
                push_subsection_item(&mut out, &target_header, &bullet);
                out.push('\n');
            }
            out.push_str(line);
            out.push('\n');
        }
        // Handle insertion at end of section
        if insert_at >= lines.len() {
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            push_subsection_item(&mut out, &target_header, &bullet);
        }
    } else {
        // No subsections exist yet - create the first one after section header
        for (idx, line) in lines.iter().enumerate() {
            out.push_str(line);
            out.push('\n');
            if idx == start {
                out.push('\n');
                out.push_str(&target_header);
                out.push('\n');
                out.push_str(&bullet);
                out.push('\n');
            }
        }
    }

    Ok((out, true))
}

fn push_subsection_item(out: &mut String, target_header: &str, bullet: &str) {
    out.push_str(target_header);
    out.push('\n');
    out.push_str(bullet);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn aliases(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    /// Assert every fragment appears in `out`, in the given order.
    fn assert_in_order(out: &str, fragments: &[&str]) {
        let mut cursor = 0;
        for fragment in fragments {
            let found = out[cursor..]
                .find(fragment)
                .unwrap_or_else(|| panic!("missing {fragment:?} after byte {cursor} in:\n{out}"));
            cursor += found + fragment.len();
        }
    }

    #[test]
    fn finalize_next_section_renames_next_heading_to_dated_version() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            (
                "# Changelog\n\n## Unreleased\n\n- First\n- Second\n\n## 0.1.0\n\n- Old\n",
                &["Unreleased", "[Unreleased]"],
                &["## [0.2.0] - ", "- First\n- Second", "## 0.1.0", "- Old"],
            ),
            (
                "# Changelog\n\n## Unreleased\n\n### Added\n\n- Feature\n\n### Fixed\n\n- Bug\n\n## 0.1.0\n",
                &["Unreleased"],
                &["## [0.2.0] - ", "### Added", "- Feature", "### Fixed", "- Bug", "## 0.1.0"],
            ),
            (
                "# Changelog\n\n## [Next]\n\n- Item\n\n## 0.1.0\n",
                &["Unreleased", "[Unreleased]", "Next", "[Next]"],
                &["## [0.2.0] - ", "- Item", "## 0.1.0"],
            ),
        ];

        for (content, aliases_in, expected) in cases {
            let (out, changed) =
                finalize_next_section(content, &aliases(aliases_in), "0.2.0", false).unwrap();
            assert!(changed);
            assert!(
                !out.contains("Unreleased") && !out.contains("[Next]"),
                "{out}"
            );
            assert_in_order(&out, expected);
        }
    }

    #[test]
    fn finalize_next_section_rejects_sections_without_bullets() {
        let cases = [
            (
                "# Changelog\n\n## Unreleased\n\n\n## 0.1.0\n\n- Old\n",
                "no items",
            ),
            (
                "# Changelog\n\n## Unreleased\n\n### Added\n\n### Changed\n\n## 0.1.0\n",
                "subsection",
            ),
        ];

        for (content, problem) in cases {
            let err = finalize_next_section(content, &aliases(&["Unreleased"]), "0.2.0", false)
                .unwrap_err();
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            let detail = err
                .details
                .get("problem")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert!(detail.contains(problem), "{content}: {detail}");

            let (unchanged, changed) =
                finalize_next_section(content, &aliases(&["Unreleased"]), "0.2.0", true).unwrap();
            assert!(!changed);
            assert_eq!(unchanged, content);
        }
    }

    #[test]
    fn get_latest_finalized_version_reads_plain_bracketed_and_dated_headings() {
        for (content, expected) in [
            (
                "# Changelog\n\n## Unreleased\n\n## 0.2.16\n\n- Item\n\n## 0.2.15\n",
                Some("0.2.16"),
            ),
            (
                "# Changelog\n\n## Unreleased\n\n## [1.0.0]\n\n## 0.2.16\n",
                Some("1.0.0"),
            ),
            (
                "# Changelog\n\n## Unreleased\n\n## [1.0.0] - 2025-01-14\n\n## 0.2.16\n",
                Some("1.0.0"),
            ),
            ("# Changelog\n\n## Unreleased\n\n- Item\n", None),
        ] {
            assert_eq!(
                get_latest_finalized_version(content).as_deref(),
                expected,
                "{content}"
            );
        }
    }

    #[test]
    fn append_item_to_subsection_places_items_in_canonical_typed_subsections() {
        // (content, message, type, changed, expected fragments in order)
        let cases: &[(&str, &str, &str, bool, &[&str])] = &[
            // Appends after existing bullets in the matching subsection.
            (
                "# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Existing fix\n\n## 0.1.0\n",
                "New bug fix",
                "fixed",
                true,
                &["### Fixed", "- Existing fix\n- New bug fix", "## 0.1.0"],
            ),
            // Creates a missing subsection after earlier canonical ones.
            (
                "# Changelog\n\n## Unreleased\n\n### Added\n\n- Feature\n\n## 0.1.0\n",
                "Bug fix",
                "fixed",
                true,
                &["### Added", "- Feature", "### Fixed\n- Bug fix", "## 0.1.0"],
            ),
            // Creates a missing subsection before later canonical ones.
            (
                "# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Bug fix\n\n## 0.1.0\n",
                "New feature",
                "added",
                true,
                &[
                    "### Added\n- New feature",
                    "### Fixed",
                    "- Bug fix",
                    "## 0.1.0",
                ],
            ),
            // Creates the first subsection in an empty section.
            (
                "# Changelog\n\n## Unreleased\n\n## 0.1.0\n",
                "New feature",
                "added",
                true,
                &["## Unreleased", "### Added\n- New feature", "## 0.1.0"],
            ),
            // Dedupes an existing bullet.
            (
                "# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Bug fix\n\n## 0.1.0\n",
                "Bug fix",
                "fixed",
                false,
                &["### Fixed", "- Bug fix", "## 0.1.0"],
            ),
        ];

        for (content, message, entry_type, expect_changed, expected) in cases {
            let (out, changed) =
                append_item_to_subsection(content, &aliases(&["Unreleased"]), message, entry_type)
                    .unwrap();
            assert_eq!(changed, *expect_changed, "{content}");
            assert_eq!(out.matches(&format!("- {message}\n")).count(), 1, "{out}");
            assert_in_order(&out, expected);
        }
    }

    #[test]
    fn append_to_finalized_section_adds_entries_and_keeps_heading() {
        let content = "# Changelog\n\n## [1.24.4] - 2026-10-02\n\n### Fixed\n\n- preserve mapping plan shell type\n\n## [1.24.3] - 2026-10-01\n\n### Fixed\n\n- older fix\n";
        let aliases = aliases(&["Unreleased", "[Unreleased]"]);
        let entries = HashMap::from([
            (
                "added",
                vec!["hand off validated ICO branding through native media".to_string()],
            ),
            (
                "fixed",
                vec!["preserve mapping plan shell type".to_string()],
            ),
        ]);

        let (out, changed) =
            append_to_finalized_section(content, &aliases, "1.24.4", &entries).unwrap();

        assert!(changed);
        assert!(out.contains("## [1.24.4] - 2026-10-02\n"));
        assert!(!out.contains("Unreleased"));
        let section_end = out.find("## [1.24.3]").unwrap();
        let section = &out[..section_end];
        assert!(section.contains("### Added"), "{out}");
        assert!(
            section.contains("- hand off validated ICO branding through native media"),
            "{out}"
        );
        assert_eq!(
            section
                .matches("- preserve mapping plan shell type")
                .count(),
            1
        );
        assert!(out[section_end..].contains("- older fix"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn append_to_finalized_section_skips_covered_entries_and_requires_the_section() {
        let content = "# Changelog\n\n## [0.2.0] - 2026-10-02\n\n### Fixed\n\n- existing fix\n";
        let aliases = aliases(&["Unreleased"]);

        let covered = HashMap::from([("fixed", vec!["existing fix".to_string()])]);
        let (out, changed) =
            append_to_finalized_section(content, &aliases, "0.2.0", &covered).unwrap();
        assert!(!changed);
        assert_eq!(out, content);

        let late = HashMap::from([("fixed", vec!["late fix".to_string()])]);
        assert!(append_to_finalized_section(content, &aliases, "0.3.0", &late).is_err());
    }

    #[test]
    fn finalize_with_generated_entries_dedupes_against_existing_unreleased_prose() {
        // (content, kept entry, dropped entry)
        let cases: &[(&str, &str, Option<&str>)] = &[
            // Semantic match: hand-written prose wins over the generated entry.
            (
                "# Changelog\n\n## Unreleased\n\n### Added\n\n- Codex CLI runtime support\n\n## 0.1.0\n",
                "- Codex CLI runtime support",
                Some("- add Codex runtime support"),
            ),
            // Normalized exact match (case, punctuation).
            (
                "# Changelog\n\n## Unreleased\n\n### Added\n\n- Add Codex runtime support.\n\n## 0.1.0\n",
                "- Add Codex runtime support.",
                Some("- add Codex runtime support"),
            ),
            // Nothing to dedupe against: the generated entry populates the section.
            (
                "# Changelog\n\n## Unreleased\n\n## 0.1.0\n",
                "### Added\n- add Codex runtime support",
                None,
            ),
        ];
        let entries = HashMap::from([("added", vec!["add Codex runtime support".to_string()])]);

        for (content, kept, dropped) in cases {
            let (out, changed) = finalize_with_generated_entries(
                content,
                &aliases(&["Unreleased"]),
                &entries,
                "0.2.0",
            )
            .unwrap();
            assert!(changed);
            assert_in_order(&out, &["## [0.2.0] - ", kept, "## 0.1.0"]);
            if let Some(dropped) = dropped {
                assert!(!out.contains(dropped), "{out}");
            }
        }
    }

    #[test]
    fn unreleased_entries_count_and_extract_dash_and_asterisk_bullets_only() {
        let aliases = aliases(&["Unreleased"]);
        let cases: &[(&str, &[&str])] = &[
            (
                "# Changelog\n\n## Unreleased\n\n- Item one\n- Item two\n- Item three\n\n## 0.1.0\n- Old\n",
                &["Item one", "Item two", "Item three"],
            ),
            (
                "# Changelog\n\n## Unreleased\n\n### Added\n\n- Feature one\n\n### Fixed\n\n- Dash item\n* Asterisk item\n\n## 0.1.0\n",
                &["Feature one", "Dash item", "Asterisk item"],
            ),
            ("# Changelog\n\n## Unreleased\n\n## 0.1.0\n", &[]),
            ("# Changelog\n\n## 0.1.0\n- Initial release\n", &[]),
            (
                "# Changelog\n\n## Unreleased\n\n### Added\n\n### Changed\n\n## 0.1.0\n",
                &[],
            ),
        ];

        for (content, expected) in cases {
            assert_eq!(
                count_unreleased_entries(content, &aliases),
                expected.len(),
                "{content}"
            );
            assert_eq!(
                get_unreleased_entries(content, &aliases),
                *expected,
                "{content}"
            );
        }
    }
}

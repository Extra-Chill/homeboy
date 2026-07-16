mod normalize_heading_label;
mod types;
mod unreleased;

pub use normalize_heading_label::{extract_last_release_snapshot, get_latest_finalized_version};
use normalize_heading_label::{is_matching_next_section_heading, validate_section_content};
use std::collections::HashSet;
use types::SectionContentStatus;
pub use unreleased::{count_unreleased_entries, get_unreleased_entries};

use chrono::Local;

use crate::engine::validation;
use crate::error::{Error, Result};

use super::settings::*;

pub fn finalize_next_section(
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
pub fn finalize_with_generated_entries(
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

    #[test]
    fn finalize_moves_body_to_new_version_and_omits_empty_next_section() {
        let content = "# Changelog\n\n## Unreleased\n\n- First\n- Second\n\n## 0.1.0\n\n- Old\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let (out, changed) = finalize_next_section(content, &aliases, "0.2.0", false).unwrap();
        assert!(changed);
        assert!(!out.contains("## Unreleased\n\n## [0.2.0]"));
        // Check for Keep a Changelog format: ## [X.Y.Z] - YYYY-MM-DD
        assert!(out.contains("## [0.2.0] - "));
        assert!(out.contains("- First\n- Second"));
        assert!(out.contains("## 0.1.0"));
    }

    #[test]
    fn finalize_errors_on_empty_next_section_by_default() {
        let content = "# Changelog\n\n## Unreleased\n\n\n## 0.1.0\n\n- Old\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let err = finalize_next_section(content, &aliases, "0.2.0", false).unwrap_err();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("Invalid"));
    }

    #[test]
    fn get_latest_finalized_version_finds_first_semver() {
        let content = "# Changelog\n\n## Unreleased\n\n## 0.2.16\n\n- Item\n\n## 0.2.15\n";
        assert_eq!(
            get_latest_finalized_version(content),
            Some("0.2.16".to_string())
        );
    }

    #[test]
    fn get_latest_finalized_version_parses_bracketed_format() {
        let content = "# Changelog\n\n## Unreleased\n\n## [1.0.0]\n\n## 0.2.16\n";
        // [1.0.0] is now parsed as 1.0.0 (Keep a Changelog format)
        assert_eq!(
            get_latest_finalized_version(content),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn get_latest_finalized_version_parses_dated_format() {
        let content = "# Changelog\n\n## Unreleased\n\n## [1.0.0] - 2025-01-14\n\n## 0.2.16\n";
        // Full Keep a Changelog format with date
        assert_eq!(
            get_latest_finalized_version(content),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn get_latest_finalized_version_returns_none_when_no_versions() {
        let content = "# Changelog\n\n## Unreleased\n\n- Item\n";
        assert_eq!(get_latest_finalized_version(content), None);
    }

    // === Keep a Changelog Subsection Tests ===

    #[test]
    fn validate_section_content_with_direct_bullets() {
        let lines = vec!["- Item one", "- Item two"];
        assert_eq!(
            validate_section_content(&lines),
            SectionContentStatus::Valid
        );
    }

    #[test]
    fn validate_section_content_with_subsection_bullets() {
        let lines = vec![
            "### Added",
            "",
            "- New feature",
            "",
            "### Fixed",
            "",
            "- Bug fix",
        ];
        assert_eq!(
            validate_section_content(&lines),
            SectionContentStatus::Valid
        );
    }

    #[test]
    fn validate_section_content_subsections_only() {
        let lines = vec!["### Added", "", "### Changed", ""];
        assert_eq!(
            validate_section_content(&lines),
            SectionContentStatus::SubsectionsOnly
        );
    }

    #[test]
    fn validate_section_content_empty() {
        let lines = vec!["", ""];
        assert_eq!(
            validate_section_content(&lines),
            SectionContentStatus::Empty
        );
    }

    #[test]
    fn finalize_preserves_subsection_structure() {
        let content =
            "# Changelog\n\n## Unreleased\n\n### Added\n\n- Feature\n\n### Fixed\n\n- Bug\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) = finalize_next_section(content, &aliases, "0.2.0", false).unwrap();

        assert!(changed);
        assert!(out.contains("## [0.2.0]"));
        assert!(out.contains("### Added"));
        assert!(out.contains("### Fixed"));
        assert!(out.contains("- Feature"));
        assert!(out.contains("- Bug"));
    }

    #[test]
    fn finalize_errors_on_empty_subsections() {
        let content = "# Changelog\n\n## Unreleased\n\n### Added\n\n### Changed\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let result = finalize_next_section(content, &aliases, "0.2.0", false);

        assert!(result.is_err());
        let err = result.unwrap_err();
        // Error details contain "problem" field with the specific message
        let problem = err
            .details
            .get("problem")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            problem.contains("subsection"),
            "Error should mention subsection headers: {}",
            problem
        );
    }

    #[test]
    fn find_next_section_matches_next_alias() {
        let lines: Vec<&str> = "# Changelog\n\n## [Next]\n\n- Item\n\n## 0.1.0\n"
            .lines()
            .collect();
        let aliases = vec![
            "Unreleased".to_string(),
            "[Unreleased]".to_string(),
            "Next".to_string(),
            "[Next]".to_string(),
        ];

        let start = find_next_section_start(&lines, &aliases);
        assert_eq!(start, Some(2));
    }

    // === Typed Subsection Tests (--type flag) ===

    #[test]
    fn append_item_to_subsection_adds_to_existing() {
        let content = "# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Existing fix\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) =
            append_item_to_subsection(content, &aliases, "New bug fix", "fixed").unwrap();

        assert!(changed);
        assert!(out.contains("- Existing fix"));
        assert!(out.contains("- New bug fix"));
        // New item should be after existing
        assert!(out.contains("- Existing fix\n- New bug fix"));
    }

    #[test]
    fn append_item_to_subsection_creates_new_subsection() {
        let content = "# Changelog\n\n## Unreleased\n\n### Added\n\n- Feature\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) =
            append_item_to_subsection(content, &aliases, "Bug fix", "fixed").unwrap();

        assert!(changed);
        assert!(out.contains("### Fixed"));
        assert!(out.contains("- Bug fix"));
        // Should preserve existing subsection
        assert!(out.contains("### Added"));
        assert!(out.contains("- Feature"));
    }

    #[test]
    fn append_item_to_subsection_creates_first_subsection() {
        let content = "# Changelog\n\n## Unreleased\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) =
            append_item_to_subsection(content, &aliases, "New feature", "added").unwrap();

        assert!(changed);
        assert!(out.contains("### Added"));
        assert!(out.contains("- New feature"));
    }

    #[test]
    fn append_item_to_subsection_maintains_canonical_order() {
        // Fixed comes after Added in canonical order
        let content = "# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Bug fix\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) =
            append_item_to_subsection(content, &aliases, "New feature", "added").unwrap();

        assert!(changed);
        assert!(out.contains("### Added"));
        assert!(out.contains("- New feature"));
        // Added should appear before Fixed (canonical order)
        let added_pos = out.find("### Added").unwrap();
        let fixed_pos = out.find("### Fixed").unwrap();
        assert!(
            added_pos < fixed_pos,
            "Added should come before Fixed in canonical order"
        );
    }

    #[test]
    fn append_item_to_subsection_dedupes_existing() {
        let content = "# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Bug fix\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) =
            append_item_to_subsection(content, &aliases, "Bug fix", "fixed").unwrap();

        assert!(!changed);
        assert_eq!(out.matches("- Bug fix").count(), 1);
    }

    #[test]
    fn finalize_generated_entries_prefers_existing_unreleased_prose() {
        let content =
            "# Changelog\n\n## Unreleased\n\n### Added\n\n- Codex CLI runtime support\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let entries = std::collections::HashMap::from([(
            "added",
            vec!["add Codex runtime support".to_string()],
        )]);

        let (out, changed) =
            finalize_with_generated_entries(content, &aliases, &entries, "0.2.0").unwrap();

        assert!(changed);
        assert!(out.contains("## [0.2.0]"));
        assert!(out.contains("- Codex CLI runtime support"));
        assert!(!out.contains("- add Codex runtime support"));
    }

    #[test]
    fn finalize_generated_entries_dedupes_normalized_exact_existing_entries() {
        let content = "# Changelog\n\n## Unreleased\n\n### Added\n\n- Add Codex runtime support.\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let entries = std::collections::HashMap::from([(
            "added",
            vec!["add codex runtime support".to_string()],
        )]);

        let (out, changed) =
            finalize_with_generated_entries(content, &aliases, &entries, "0.2.0").unwrap();

        assert!(changed);
        assert_eq!(out.matches("Codex runtime support").count(), 1);
    }

    #[test]
    fn finalize_generated_entries_still_populates_empty_unreleased_section() {
        let content = "# Changelog\n\n## Unreleased\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let entries = std::collections::HashMap::from([(
            "added",
            vec!["add Codex runtime support".to_string()],
        )]);

        let (out, changed) =
            finalize_with_generated_entries(content, &aliases, &entries, "0.2.0").unwrap();

        assert!(changed);
        assert!(out.contains("## [0.2.0]"));
        assert!(out.contains("### Added"));
        assert!(out.contains("- add Codex runtime support"));
    }

    // === count_unreleased_entries Tests ===

    #[test]
    fn count_unreleased_entries_with_direct_bullets() {
        let content =
            "# Changelog\n\n## Unreleased\n\n- Item one\n- Item two\n- Item three\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        assert_eq!(count_unreleased_entries(content, &aliases), 3);
    }

    #[test]
    fn count_unreleased_entries_with_subsection_bullets() {
        let content = "# Changelog\n\n## Unreleased\n\n### Added\n\n- Feature one\n- Feature two\n\n### Fixed\n\n- Bug fix\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        assert_eq!(count_unreleased_entries(content, &aliases), 3);
    }

    #[test]
    fn count_unreleased_entries_empty_section() {
        let content = "# Changelog\n\n## Unreleased\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        assert_eq!(count_unreleased_entries(content, &aliases), 0);
    }

    #[test]
    fn count_unreleased_entries_no_section() {
        let content = "# Changelog\n\n## 0.1.0\n- Initial release\n";
        let aliases = vec!["Unreleased".to_string()];
        assert_eq!(count_unreleased_entries(content, &aliases), 0);
    }

    #[test]
    fn count_unreleased_entries_subsections_only_no_bullets() {
        let content = "# Changelog\n\n## Unreleased\n\n### Added\n\n### Changed\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        assert_eq!(count_unreleased_entries(content, &aliases), 0);
    }

    #[test]
    fn count_unreleased_entries_with_asterisk_bullets() {
        let content = "# Changelog\n\n## Unreleased\n\n* Item one\n* Item two\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        assert_eq!(count_unreleased_entries(content, &aliases), 2);
    }

    #[test]
    fn count_unreleased_entries_mixed_bullets() {
        let content = "# Changelog\n\n## Unreleased\n\n- Dash item\n* Asterisk item\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        assert_eq!(count_unreleased_entries(content, &aliases), 2);
    }

    #[test]
    fn get_unreleased_entries_extracts_bullet_text() {
        let content = "# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Dash item\n* Asterisk item\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        assert_eq!(
            get_unreleased_entries(content, &aliases),
            vec!["Dash item".to_string(), "Asterisk item".to_string()]
        );
    }
}

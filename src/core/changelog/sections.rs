use chrono::Local;

use crate::error::{Error, Result};
use crate::utils::{parser, validation};

use super::settings::*;

#[derive(Debug, PartialEq)]
enum SectionContentStatus {
    Valid,           // Has bullet items (direct or under subsections)
    SubsectionsOnly, // Has ### headers but no bullets
    Empty,           // Nothing meaningful
}

fn validate_section_content(body_lines: &[&str]) -> SectionContentStatus {
    let mut has_subsection_headers = false;
    let mut has_bullets = false;

    for line in body_lines {
        let trimmed = line.trim();

        // Stop at next ## heading
        if trimmed.starts_with("## ") {
            break;
        }

        // Detect subsection headers
        if KEEP_A_CHANGELOG_SUBSECTIONS
            .iter()
            .any(|h| trimmed.starts_with(h))
        {
            has_subsection_headers = true;
        }

        // Detect bullet items (- or *)
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            has_bullets = true;
        }
    }

    if has_bullets {
        SectionContentStatus::Valid
    } else if has_subsection_headers {
        SectionContentStatus::SubsectionsOnly
    } else {
        SectionContentStatus::Empty
    }
}

pub fn check_next_section_content(
    changelog_content: &str,
    next_section_aliases: &[String],
) -> Result<Option<String>> {
    let lines: Vec<&str> = changelog_content.lines().collect();
    let start = match find_next_section_start(&lines, next_section_aliases) {
        Some(idx) => idx,
        None => return Ok(None),
    };

    let end = find_section_end(&lines, start);
    let body_lines = &lines[start + 1..end];
    let content_status = validate_section_content(body_lines);

    match content_status {
        SectionContentStatus::Valid => Ok(None),
        SectionContentStatus::SubsectionsOnly => Ok(Some(String::from("subsection_headers_only"))),
        SectionContentStatus::Empty => Ok(Some(String::from("empty"))),
    }
}

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
        "No changelog items found (cannot finalize)",
        vec![
            "Commit all changes before running version bump (changelog auto-generates from commits).".to_string(),
            "Or add entries manually: `homeboy changelog add <componentId> -m \"...\"`".to_string(),
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
        .with_hint("Commit all changes before running version bump (changelog auto-generates from commits).")
        .with_hint("Or add entries manually: `homeboy changelog add <componentId> -m \"...\"`"));
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

fn normalize_heading_label(label: &str) -> String {
    label.trim().trim_matches(['[', ']']).trim().to_string()
}

fn is_matching_next_section_heading(line: &str, aliases: &[String]) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with("##") {
        return false;
    }

    let raw_label = trimmed.trim_start_matches('#').trim();
    let normalized = normalize_heading_label(raw_label);

    aliases
        .iter()
        .any(|a| normalize_heading_label(a) == normalized)
}

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

/// Count bullet items in the unreleased section.
/// Returns 0 if no unreleased section exists or section is empty.
pub fn count_unreleased_entries(content: &str, aliases: &[String]) -> usize {
    let lines: Vec<&str> = content.lines().collect();
    let start = match find_next_section_start(&lines, aliases) {
        Some(idx) => idx,
        None => return 0,
    };

    let end = find_section_end(&lines, start);

    lines[start + 1..end]
        .iter()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("- ") || trimmed.starts_with("* ")
        })
        .count()
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

/// Extract semver from changelog heading formats:
/// - "0.1.0" -> Some("0.1.0")
/// - "[0.1.0]" -> Some("0.1.0")
/// - "0.1.0 - 2025-01-14" -> Some("0.1.0")
/// - "[0.1.0] - 2025-01-14" -> Some("0.1.0")
/// - "Unreleased" -> None
fn extract_version_from_heading(label: &str) -> Option<String> {
    parser::extract_first(label, r"\[?(\d+\.\d+\.\d+)\]?")
}

/// Get the latest finalized version from the changelog (first ## heading that contains a semver).
/// Supports Keep a Changelog format: `## [X.Y.Z] - YYYY-MM-DD`
/// Returns None if no version section is found.
pub fn get_latest_finalized_version(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            let label = trimmed.trim_start_matches("## ").trim();
            if let Some(version) = extract_version_from_heading(label) {
                return Some(version);
            }
        }
    }
    None
}

pub fn add_next_section_item(
    changelog_content: &str,
    next_section_aliases: &[String],
    message: &str,
) -> Result<(String, bool)> {
    let trimmed_message = message.trim();
    if trimmed_message.is_empty() {
        return Err(Error::validation_invalid_argument(
            "message",
            "Changelog message cannot be empty",
            None,
            None,
        ));
    }

    let (with_section, section_changed) =
        ensure_next_section(changelog_content, next_section_aliases)?;
    let (with_item, item_changed) =
        append_item_to_next_section(&with_section, next_section_aliases, trimmed_message)?;

    Ok((with_item, section_changed || item_changed))
}

pub fn add_next_section_items(
    changelog_content: &str,
    next_section_aliases: &[String],
    messages: &[String],
) -> Result<(String, bool, usize)> {
    if messages.is_empty() {
        return Err(Error::validation_invalid_argument(
            "messages",
            "Changelog messages cannot be empty",
            None,
            None,
        ));
    }

    let (mut content, mut changed) = ensure_next_section(changelog_content, next_section_aliases)?;
    let mut items_added = 0;

    for message in messages {
        let trimmed_message = message.trim();
        if trimmed_message.is_empty() {
            return Err(Error::validation_invalid_argument(
                "messages",
                "Changelog messages cannot include empty values",
                None,
                None,
            ));
        }

        let (next, item_changed) =
            append_item_to_next_section(&content, next_section_aliases, trimmed_message)?;
        if item_changed {
            items_added += 1;
            changed = true;
        }
        content = next;
    }

    Ok((content, changed, items_added))
}

fn append_item_to_next_section(
    content: &str,
    aliases: &[String],
    message: &str,
) -> Result<(String, bool)> {
    let lines: Vec<&str> = content.lines().collect();
    let start = find_next_section_start(&lines, aliases).ok_or_else(|| {
        Error::internal_unexpected("Next changelog section not found (unexpected)".to_string())
    })?;

    let section_end = find_section_end(&lines, start);
    let bullet = format!("- {}", message);

    // Check for duplicates
    for line in &lines[start + 1..section_end] {
        if line.trim() == bullet {
            return Ok((content.to_string(), false));
        }
    }

    // Detect if section uses Keep a Changelog subsections
    let has_subsections = lines[start + 1..section_end].iter().any(|l| {
        KEEP_A_CHANGELOG_SUBSECTIONS
            .iter()
            .any(|h| l.trim().starts_with(h))
    });

    // Find where to insert
    let mut insert_after = start;
    let mut has_bullets = false;
    let mut first_subsection_idx: Option<usize> = None;

    for (i, line) in lines.iter().enumerate().take(section_end).skip(start + 1) {
        let trimmed = line.trim();

        // Track first subsection header (for fallback insertion point)
        if first_subsection_idx.is_none()
            && KEEP_A_CHANGELOG_SUBSECTIONS
                .iter()
                .any(|h| trimmed.starts_with(h))
        {
            first_subsection_idx = Some(i);
        }

        if trimmed.starts_with('-') || trimmed.starts_with('*') {
            insert_after = i;
            has_bullets = true;
        } else if !has_bullets && !has_subsections && trimmed.is_empty() {
            // No bullets yet and no subsections, insert after blank line following header
            insert_after = i;
        }
    }

    // If subsections exist but no bullets found, insert after the first subsection header
    if has_subsections && !has_bullets {
        if let Some(idx) = first_subsection_idx {
            insert_after = idx;
        }
    }

    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        // Skip trailing blank lines after bullets (we'll add one at the end)
        // Only skip if there are actual bullets and we're in the simple (non-subsection) case
        if has_bullets
            && !has_subsections
            && idx > insert_after
            && idx < section_end
            && lines[idx].trim().is_empty()
        {
            continue;
        }

        out.push_str(line);
        out.push('\n');

        // Insert new bullet at the insertion point
        if idx == insert_after {
            out.push_str(&bullet);
            out.push('\n');
            // Add blank line after bullets (before next section) only in simple case
            if !has_subsections && section_end < lines.len() {
                out.push('\n');
            }
        }
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
                out.push_str(&target_header);
                out.push('\n');
                out.push_str(&bullet);
                out.push_str("\n\n");
            }
            out.push_str(line);
            out.push('\n');
        }
        // Handle insertion at end of section
        if insert_at >= lines.len() {
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(&target_header);
            out.push('\n');
            out.push_str(&bullet);
            out.push('\n');
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_next_section_items_appends_multiple_in_order() {
        let content = "# Changelog\n\n## Unreleased\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let messages = vec!["First".to_string(), "Second".to_string()];

        let (out, changed, items_added) =
            add_next_section_items(content, &aliases, &messages).unwrap();
        assert!(changed);
        assert_eq!(items_added, 2);
        assert!(out.contains("## Unreleased\n\n- First\n- Second\n\n## 0.1.0"));
    }

    #[test]
    fn add_next_section_items_dedupes_exact_bullets() {
        let content = "# Changelog\n\n## Unreleased\n\n- First\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string(), "[Unreleased]".to_string()];
        let messages = vec!["First".to_string(), "Second".to_string()];

        let (out, changed, items_added) =
            add_next_section_items(content, &aliases, &messages).unwrap();
        assert!(changed);
        assert_eq!(items_added, 1);
        assert!(out.contains("- First"));
        assert!(out.contains("- Second"));
    }

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
    fn append_item_works_with_subsection_structure() {
        let content = "# Changelog\n\n## Unreleased\n\n### Added\n\n- Existing\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) = append_item_to_next_section(content, &aliases, "New item").unwrap();

        assert!(changed);
        assert!(out.contains("- New item"));
        // Item should be inserted after "- Existing"
        assert!(out.contains("- Existing\n- New item"));
    }

    #[test]
    fn append_item_to_empty_subsection() {
        let content = "# Changelog\n\n## Unreleased\n\n### Added\n\n### Fixed\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) = append_item_to_next_section(content, &aliases, "New item").unwrap();

        assert!(changed);
        assert!(out.contains("- New item"));
        // Item should be inserted after the first subsection header
        assert!(out.contains("### Added\n- New item"));
    }

    #[test]
    fn append_item_preserves_multiple_subsections() {
        let content =
            "# Changelog\n\n## Unreleased\n\n### Added\n\n- Feature 1\n\n### Fixed\n\n- Bug 1\n\n## 0.1.0\n";
        let aliases = vec!["Unreleased".to_string()];
        let (out, changed) = append_item_to_next_section(content, &aliases, "New item").unwrap();

        assert!(changed);
        assert!(out.contains("- New item"));
        // Should preserve subsection structure
        assert!(out.contains("### Added"));
        assert!(out.contains("### Fixed"));
        assert!(out.contains("- Feature 1"));
        assert!(out.contains("- Bug 1"));
    }

    // === Typed Subsection Tests (--type flag) ===

    #[test]
    fn validate_entry_type_accepts_valid_types() {
        assert!(validate_entry_type("added").is_ok());
        assert!(validate_entry_type("Added").is_ok());
        assert!(validate_entry_type("FIXED").is_ok());
        assert!(validate_entry_type("changed").is_ok());
        assert!(validate_entry_type("deprecated").is_ok());
        assert!(validate_entry_type("removed").is_ok());
        assert!(validate_entry_type("security").is_ok());
        assert!(validate_entry_type("refactored").is_ok());
        assert!(validate_entry_type("Refactored").is_ok());
        // "refactor" is accepted as an alias for "refactored"
        assert!(validate_entry_type("refactor").is_ok());
        assert!(validate_entry_type("Refactor").is_ok());
        assert_eq!(validate_entry_type("refactor").unwrap(), "refactored");
    }

    #[test]
    fn validate_entry_type_rejects_invalid_types() {
        assert!(validate_entry_type("invalid").is_err());
        assert!(validate_entry_type("feature").is_err());
        assert!(validate_entry_type("bugfix").is_err());
    }

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

    // === count_unreleased_entries Tests ===

    #[test]
    fn count_unreleased_entries_with_direct_bullets() {
        let content = "# Changelog\n\n## Unreleased\n\n- Item one\n- Item two\n- Item three\n\n## 0.1.0\n";
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
}

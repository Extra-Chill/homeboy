//! normalize_heading_label — extracted from sections.rs.

use super::super::settings::KEEP_A_CHANGELOG_SUBSECTIONS;
use crate::core::release::changelog::io::FinalizedReleaseSnapshot;
use crate::core::release::changelog::sections::types::SectionContentStatus;
use crate::engine::text;

pub(crate) fn validate_section_content(body_lines: &[&str]) -> SectionContentStatus {
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

pub(crate) fn normalize_heading_label(label: &str) -> String {
    label.trim().trim_matches(['[', ']']).trim().to_string()
}

pub(crate) fn is_matching_next_section_heading(line: &str, aliases: &[String]) -> bool {
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

/// Extract semver from changelog heading formats:
/// - "0.1.0" -> Some("0.1.0")
/// - "[0.1.0]" -> Some("0.1.0")
/// - "0.1.0 - 2025-01-14" -> Some("0.1.0")
/// - "[0.1.0] - 2025-01-14" -> Some("0.1.0")
/// - "Unreleased" -> None
pub(crate) fn extract_version_from_heading(label: &str) -> Option<String> {
    text::extract_first(label, r"\[?(\d+\.\d+\.\d+)\]?")
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

pub(crate) fn extract_date_from_heading(label: &str) -> Option<String> {
    text::extract_first(label, r"(\d{4}-\d{2}-\d{2})")
}

pub(crate) fn extract_first_bullet(lines: &[&str], start: usize) -> Option<String> {
    for line in &lines[start..] {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = trimmed.strip_prefix("* ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

pub fn extract_last_release_snapshot(content: &str) -> Option<FinalizedReleaseSnapshot> {
    let lines: Vec<&str> = content.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("## ") {
            continue;
        }

        let label = trimmed.trim_start_matches("## ").trim();
        let normalized = normalize_heading_label(label);
        if normalized.eq_ignore_ascii_case("unreleased") || normalized.eq_ignore_ascii_case("next")
        {
            continue;
        }

        let Some(tag) = extract_version_from_heading(label) else {
            continue;
        };

        return Some(FinalizedReleaseSnapshot {
            tag: format!("v{}", tag),
            date: extract_date_from_heading(label),
            summary: extract_first_bullet(&lines, index + 1),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_section_content_default_path() {

        let _result = validate_section_content();
    }

    #[test]
    fn test_normalize_heading_label_default_path() {

        let _result = normalize_heading_label();
    }

    #[test]
    fn test_is_matching_next_section_heading_trimmed_starts_with() {

        let result = is_matching_next_section_heading();
        assert!(!result, "expected false when: !trimmed.starts_with(\"##\")");
    }

    #[test]
    fn test_extract_version_from_heading_default_path() {

        let _result = extract_version_from_heading();
    }

    #[test]
    fn test_get_latest_finalized_version_trimmed_starts_with() {
        let content = "";
        let result = get_latest_finalized_version(&content);
        assert!(result.is_some(), "expected Some for: trimmed.starts_with(\"## \")");
    }

    #[test]
    fn test_get_latest_finalized_version_let_some_version_extract_version_from_heading_label() {
        let content = "";
        let result = get_latest_finalized_version(&content);
        assert!(result.is_some(), "expected Some for: let Some(version) = extract_version_from_heading(label)");
    }

    #[test]
    fn test_get_latest_finalized_version_let_some_version_extract_version_from_heading_label_2() {
        let content = "";
        let result = get_latest_finalized_version(&content);
        assert!(result.is_none(), "expected None for: let Some(version) = extract_version_from_heading(label)");
    }

    #[test]
    fn test_extract_date_from_heading_default_path() {

        let _result = extract_date_from_heading();
    }

    #[test]
    fn test_extract_first_bullet_trimmed_starts_with() {

        let result = extract_first_bullet();
        assert!(result.is_some(), "expected Some for: trimmed.starts_with(\"## \")");
    }

    #[test]
    fn test_extract_first_bullet_let_some_rest_trimmed_strip_prefix() {

        let result = extract_first_bullet();
        assert!(result.is_some(), "expected Some for: let Some(rest) = trimmed.strip_prefix(\"- \")");
    }

    #[test]
    fn test_extract_first_bullet_let_some_rest_trimmed_strip_prefix_2() {

        let result = extract_first_bullet();
        assert!(result.is_some(), "expected Some for: let Some(rest) = trimmed.strip_prefix(\"- \")");
    }

    #[test]
    fn test_extract_first_bullet_let_some_rest_trimmed_strip_prefix_3() {

        let result = extract_first_bullet();
        assert!(result.is_some(), "expected Some for: let Some(rest) = trimmed.strip_prefix(\"* \")");
    }

    #[test]
    fn test_extract_first_bullet_let_some_rest_trimmed_strip_prefix_4() {

        let result = extract_first_bullet();
        assert!(result.is_none(), "expected None for: let Some(rest) = trimmed.strip_prefix(\"* \")");
    }

    #[test]
    fn test_extract_last_release_snapshot_normalized_eq_ignore_ascii_case_unreleased_normalized_eq_ign() {
        let content = "";
        let result = extract_last_release_snapshot(&content);
        let inner = result.expect("expected Some for: normalized.eq_ignore_ascii_case(\"unreleased\") || normalized.eq_ignore_ascii_case(\"next\")");
        // Branch returns Some(tag)
        assert_eq!(inner.tag, String::new());
        assert_eq!(inner.date, None);
        assert_eq!(inner.summary, None);
    }

    #[test]
    fn test_extract_last_release_snapshot_none() {
        let content = "";
        let result = extract_last_release_snapshot(&content);
        assert!(result.is_none(), "expected None for: None");
    }

}

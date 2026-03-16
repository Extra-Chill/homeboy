//! normalize_heading_label — extracted from sections.rs.

use crate::engine::text;
use crate::core::release::changelog::io::FinalizedReleaseSnapshot;
use crate::core::release::changelog::sections::types::SectionContentStatus;
use super::super::settings::KEEP_A_CHANGELOG_SUBSECTIONS;


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

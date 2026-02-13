use crate::component::{self, Component};
use crate::project;

pub(super) const DEFAULT_NEXT_SECTION_LABEL: &str = "Unreleased";

pub(super) const KEEP_A_CHANGELOG_SUBSECTIONS: &[&str] = &[
    "### Added",
    "### Changed",
    "### Deprecated",
    "### Removed",
    "### Fixed",
    "### Security",
];

pub(super) const VALID_ENTRY_TYPES: &[&str] = &[
    "added",
    "changed",
    "deprecated",
    "removed",
    "fixed",
    "security",
    "refactored",
];

#[derive(Debug, Clone)]
pub struct EffectiveChangelogSettings {
    pub next_section_label: String,
    pub next_section_aliases: Vec<String>,
}

pub fn resolve_effective_settings(component: Option<&Component>) -> EffectiveChangelogSettings {
    let project_settings = component
        .and_then(|c| component::projects_using(&c.id).ok())
        .and_then(|projects| {
            if projects.len() == 1 {
                project::load(&projects[0]).ok()
            } else {
                None
            }
        });

    let next_section_label = component
        .and_then(|c| c.changelog_next_section_label.clone())
        .or_else(|| {
            project_settings
                .as_ref()
                .and_then(|p| p.changelog_next_section_label.clone())
        })
        .unwrap_or_else(|| DEFAULT_NEXT_SECTION_LABEL.to_string());

    let mut next_section_aliases = component
        .and_then(|c| c.changelog_next_section_aliases.clone())
        .or_else(|| project_settings.and_then(|p| p.changelog_next_section_aliases.clone()))
        .unwrap_or_default();

    if next_section_aliases.is_empty() {
        next_section_aliases.extend([
            next_section_label.clone(),
            format!("[{}]", next_section_label),
        ]);
    } else {
        let label_alias = next_section_label.trim();
        let bracketed_alias = format!("[{}]", label_alias);

        let mut has_label = false;
        let mut has_bracketed = false;

        for alias in &next_section_aliases {
            let trimmed_alias = alias.trim();
            if trimmed_alias == label_alias {
                has_label = true;
            }
            if trimmed_alias == bracketed_alias {
                has_bracketed = true;
            }
        }

        if !has_label {
            next_section_aliases.push(next_section_label.clone());
        }
        if !has_bracketed {
            next_section_aliases.push(format!("[{}]", next_section_label));
        }
    }

    EffectiveChangelogSettings {
        next_section_label,
        next_section_aliases,
    }
}

pub(super) fn validate_entry_type(entry_type: &str) -> crate::error::Result<String> {
    use crate::error::Error;
    let normalized = entry_type.to_lowercase();
    // Accept "refactor" as alias for "refactored"
    let normalized = if normalized == "refactor" {
        "refactored".to_string()
    } else {
        normalized
    };
    if VALID_ENTRY_TYPES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(Error::validation_invalid_argument(
            "type",
            format!(
                "Invalid changelog entry type '{}'. Valid types: Added, Changed, Deprecated, Removed, Fixed, Security, Refactored",
                entry_type
            ),
            None,
            Some(vec![
                "Use --type added for new features".to_string(),
                "Use --type fixed for bug fixes".to_string(),
                "Use --type changed for modifications".to_string(),
                "Use --type refactored for code restructuring".to_string(),
            ]),
        ))
    }
}

pub(super) fn subsection_header_from_type(entry_type: &str) -> String {
    let capitalized = entry_type
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default()
        + &entry_type[1..];
    format!("### {}", capitalized)
}

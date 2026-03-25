use crate::component::{self, Component};
use crate::project;

pub(super) const DEFAULT_NEXT_SECTION_LABEL: &str = "Unreleased";
pub(super) const DEFAULT_NEXT_SECTION_ALIASES: &[&str] = &["Unreleased", "Next"];

pub(super) const KEEP_A_CHANGELOG_SUBSECTIONS: &[&str] = &[
    "### Added",
    "### Changed",
    "### Deprecated",
    "### Removed",
    "### Fixed",
    "### Security",
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
        for alias in DEFAULT_NEXT_SECTION_ALIASES {
            next_section_aliases.push((*alias).to_string());
        }
    }

    let mut ensure_alias = |alias: &str| {
        if !next_section_aliases
            .iter()
            .any(|existing| existing.trim().eq_ignore_ascii_case(alias.trim()))
        {
            next_section_aliases.push(alias.to_string());
        }
    };

    for alias in DEFAULT_NEXT_SECTION_ALIASES {
        ensure_alias(alias);
        ensure_alias(&format!("[{}]", alias));
    }

    ensure_alias(&next_section_label);
    ensure_alias(&format!("[{}]", next_section_label));

    EffectiveChangelogSettings {
        next_section_label,
        next_section_aliases,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_include_unreleased_and_next_aliases() {
        let settings = resolve_effective_settings(None);
        let aliases = settings.next_section_aliases;

        assert!(aliases.iter().any(|a| a == "Unreleased"));
        assert!(aliases.iter().any(|a| a == "[Unreleased]"));
        assert!(aliases.iter().any(|a| a == "Next"));
        assert!(aliases.iter().any(|a| a == "[Next]"));
    }

    #[test]
    fn test_resolve_effective_settings_else() {
        let component = None;
        let _result = resolve_effective_settings(component);
    }

    #[test]
    fn test_resolve_effective_settings_has_expected_effects() {
        // Expected effects: mutation
        let component = None;
        let _ = resolve_effective_settings(component);
    }

    #[test]
    fn test_subsection_header_from_type_default_path() {

        let _result = subsection_header_from_type();
    }

}

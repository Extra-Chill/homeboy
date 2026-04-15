use std::path::{Path, PathBuf};

use crate::component::{self, Component};
use crate::engine::local_files;
use crate::engine::validation;
use crate::error::Result;
use crate::paths::resolve_path;

use super::sections::*;
use super::settings::*;

/// Common changelog file locations to check when the configured path doesn't exist.
/// Ordered by convention preference.
const CHANGELOG_CANDIDATES: &[&str] = &[
    "CHANGELOG.md",
    "docs/CHANGELOG.md",
    "changelog.md",
    "docs/changelog.md",
    "doc/CHANGELOG.md",
    "CHANGES.md",
];

pub fn resolve_changelog_path(component: &Component) -> Result<PathBuf> {
    // Validate local_path is absolute and exists before any file operations
    component::validate_local_path(component)?;

    // Require explicit configuration - no auto-detection
    let target = validation::require_with_hints(
        component.changelog_target.as_ref(),
        "component.changelog_target",
        "No changelog configured for this component. To add one, run:",
        vec![
            format!(
                "Create new changelog:\n  homeboy changelog init {} --configure",
                component.id
            ),
            format!(
                "Use existing file:\n  homeboy component set {} --changelog-target \"CHANGELOG.md\"",
                component.id
            ),
        ],
    )?;

    let configured_path = resolve_path(&component.local_path, target);

    // If the configured path exists, use it directly
    if configured_path.exists() {
        return Ok(configured_path);
    }

    // Configured path doesn't exist — try common fallback locations
    let local_path = Path::new(&component.local_path);
    for candidate in CHANGELOG_CANDIDATES {
        let candidate_path = local_path.join(candidate);
        if candidate_path.exists() && candidate_path != configured_path {
            log_status!(
                "changelog",
                "Configured changelog_target '{}' not found, using discovered '{}'. Fix with:\n  homeboy component set {} --changelog-target \"{}\"",
                target,
                candidate,
                component.id,
                candidate
            );
            return Ok(candidate_path);
        }
    }

    // Nothing found — return the configured path (will fail downstream with a
    // clear "file not found" error from the caller that tries to read it)
    Ok(configured_path)
}

#[derive(Debug, Clone)]
pub struct ChangelogSnapshotData {
    pub path: String,
    pub label: String,
    pub items: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FinalizedReleaseSnapshot {
    pub tag: String,
    pub date: Option<String>,
    pub summary: Option<String>,
}

pub fn read_component_snapshots(
    component: &Component,
) -> Result<(
    Option<FinalizedReleaseSnapshot>,
    Option<ChangelogSnapshotData>,
)> {
    let changelog_path = resolve_changelog_path(component)?;
    let content = local_files::read_file(&changelog_path, "read changelog")?;
    let settings = resolve_effective_settings(Some(component));

    let last_release = extract_last_release_snapshot(&content);
    let unreleased = Some(ChangelogSnapshotData {
        path: changelog_path.to_string_lossy().to_string(),
        label: settings.next_section_label,
        items: get_unreleased_entries(&content, &settings.next_section_aliases),
    });

    Ok((last_release, unreleased))
}

use std::path::PathBuf;

use crate::component::{self, Component};
use crate::engine::local_files;
use crate::engine::validation;
use crate::error::Result;
use crate::paths::resolve_path;

use super::sections::*;
use super::settings::*;

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

    resolve_target_path(&component.local_path, target)
}

fn resolve_target_path(local_path: &str, file: &str) -> Result<PathBuf> {
    Ok(resolve_path(local_path, file))
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

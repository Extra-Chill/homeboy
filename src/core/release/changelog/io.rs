use std::path::PathBuf;

use crate::component::{self, Component};
use crate::engine::validation;
use crate::error::Result;
use crate::local_files;
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

pub fn read_and_add_next_section_items(
    component: &Component,
    settings: &EffectiveChangelogSettings,
    messages: &[String],
) -> Result<(PathBuf, bool, usize)> {
    let path = resolve_changelog_path(component)?;
    let content = local_files::read_file(&path, "read changelog")?;

    let (new_content, changed, items_added) =
        add_next_section_items(&content, &settings.next_section_aliases, messages)?;

    if changed {
        local_files::write_file(&path, &new_content, "write changelog")?;
    }

    Ok((path, changed, items_added))
}

pub fn read_and_add_next_section_items_typed(
    component: &Component,
    settings: &EffectiveChangelogSettings,
    messages: &[String],
    entry_type: &str,
) -> Result<(PathBuf, bool, usize)> {
    let path = resolve_changelog_path(component)?;
    let content = local_files::read_file(&path, "read changelog")?;

    let (with_section, _) = ensure_next_section(&content, &settings.next_section_aliases)?;
    let mut current_content = with_section;
    let mut items_added = 0;
    let mut changed = false;

    for message in messages {
        let trimmed_message = message.trim();
        if trimmed_message.is_empty() {
            return Err(crate::error::Error::validation_invalid_argument(
                "messages",
                "Changelog messages cannot include empty values",
                None,
                None,
            ));
        }

        let (new_content, item_changed) = append_item_to_subsection(
            &current_content,
            &settings.next_section_aliases,
            trimmed_message,
            entry_type,
        )?;
        if item_changed {
            items_added += 1;
            changed = true;
        }
        current_content = new_content;
    }

    if changed {
        local_files::write_file(&path, &current_content, "write changelog")?;
    }

    Ok((path, changed, items_added))
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
) -> Result<(Option<FinalizedReleaseSnapshot>, Option<ChangelogSnapshotData>)> {
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

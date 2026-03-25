//! helpers — extracted from operations.rs.

use crate::component;
use crate::error::{Error, Result};
use crate::release::changelog;
use super::super::{execute_git, resolve_target};
use serde::{Deserialize, Serialize};
use std::process::Command;
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use super::detect_baseline_with_version;
use super::ChangesOutput;
use super::BaselineSource;
use super::ChangelogInfo;
use super::BaselineInfo;
use super::super::changes::*;
use super::super::commits::*;


pub(crate) fn get_component_path(component_id: &str) -> Result<String> {
    let comp = component::resolve_effective(Some(component_id), None, None)?;
    Ok(comp.local_path)
}

pub fn execute_git_for_release(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    execute_git(path, args)
}

pub(crate) fn resolve_changelog_info(
    component: &crate::component::Component,
    commits: &[CommitInfo],
) -> Option<ChangelogInfo> {
    let changelog_path = changelog::resolve_changelog_path(component).ok()?;
    let content = std::fs::read_to_string(&changelog_path).ok()?;
    let settings = changelog::resolve_effective_settings(Some(component));
    let unreleased_entries =
        changelog::count_unreleased_entries(&content, &settings.next_section_aliases);

    let hint = if unreleased_entries == 0 && !commits.is_empty() {
        Some(format!(
            "Run `homeboy changelog add {}` before bumping version",
            component.id
        ))
    } else {
        None
    };

    Some(ChangelogInfo {
        unreleased_entries,
        path: Some(changelog_path.to_string_lossy().to_string()),
        hint,
    })
}

/// Get all changes for a component.
pub fn changes(
    component_id: Option<&str>,
    since_tag: Option<&str>,
    include_diff: bool,
) -> Result<ChangesOutput> {
    let id = component_id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "componentId",
            "Missing componentId",
            None,
            Some(vec![
                "Provide a component ID: homeboy changes <component-id>".to_string(),
                "List available components: homeboy component list".to_string(),
            ]),
        )
    })?;
    let path = get_component_path(id)?;

    // Load component for version checking and changelog info
    let component = crate::component::resolve_effective(Some(id), None, None).ok();

    // Determine baseline with version alignment awareness
    let baseline = match since_tag {
        Some(t) => {
            // Explicit tag override - use as-is
            BaselineInfo {
                latest_tag: Some(t.to_string()),
                source: Some(BaselineSource::Tag),
                reference: Some(t.to_string()),
                warning: None,
            }
        }
        None => {
            // Use component version for alignment checking
            let current_version = component
                .as_ref()
                .and_then(crate::release::version::get_component_version);
            detect_baseline_with_version(&path, current_version.as_deref())?
        }
    };

    let commits = match baseline.source {
        Some(BaselineSource::LastNCommits) => get_last_n_commits(&path, DEFAULT_COMMIT_LIMIT)?,
        _ => get_commits_since_tag(&path, baseline.reference.as_deref())?,
    };

    // Resolve changelog info if component has changelog configured
    let changelog_info = component
        .as_ref()
        .and_then(|c| resolve_changelog_info(c, &commits));

    let uncommitted = get_uncommitted_changes(&path)?;
    let uncommitted_diff = if uncommitted.has_changes {
        Some(get_diff(&path)?)
    } else {
        None
    };
    let diff = if include_diff {
        baseline
            .reference
            .as_ref()
            .map(|r| get_range_diff(&path, r))
            .transpose()?
    } else {
        None
    };

    Ok(ChangesOutput {
        component_id: id.to_string(),
        path,
        success: true,
        latest_tag: baseline.latest_tag,
        baseline_source: baseline.source,
        baseline_ref: baseline.reference,
        commits,
        uncommitted,
        uncommitted_diff,
        diff,
        warning: baseline.warning,
        error: None,
        changelog: changelog_info,
    })
}

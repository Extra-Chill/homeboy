//! resolve — extracted from report.rs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use crate::component::{self, Component};
use crate::extension::{
    extension_ready_status, is_extension_compatible, is_extension_linked, load_all_extensions,
};
use crate::project::{self, Project};
use crate::{changelog, git, is_zero, is_zero_u32, version, Result};
use serde::Serialize;
use crate::server::{self, Server};
use super::super::{build_component_info, path_is_parent_of, ComponentGap, ContextOutput};
use super::ReleaseSnapshot;
use super::GitSnapshot;
use super::VersionSnapshot;
use super::ChangelogSnapshot;
use super::ComponentWithState;


pub(crate) fn collect_focused_components(
    show_all: bool,
    relevant_ids: &HashSet<String>,
    all_components: Vec<Component>,
    all_projects: &[Project],
) -> Vec<Component> {
    if show_all {
        return all_components;
    }

    let mut by_id: HashMap<String, Component> = all_components
        .into_iter()
        .filter(|c| relevant_ids.contains(&c.id))
        .map(|component| (component.id.clone(), component))
        .collect();

    for project in all_projects {
        for attachment in &project.components {
            if !relevant_ids.contains(&attachment.id) || by_id.contains_key(&attachment.id) {
                continue;
            }

            if let Some(mut component) =
                component::discover_from_portable(Path::new(&attachment.local_path))
            {
                component.id = attachment.id.clone();
                by_id.insert(component.id.clone(), component);
            }
        }
    }

    by_id.into_values().collect()
}

pub(crate) fn resolve_version_snapshot(components: &[ComponentWithState]) -> Option<VersionSnapshot> {
    let wrapper = components.first()?;
    let component = &wrapper.component;
    let snapshot = version::read_component_snapshot(component).ok()?;
    Some(VersionSnapshot {
        component_id: snapshot.component_id,
        version: snapshot.version,
        targets: snapshot.targets,
    })
}

pub(crate) fn resolve_git_snapshot(
    git_root: Option<&String>,
    current_version: Option<&str>,
) -> Option<GitSnapshot> {
    let root = git_root?;
    let snapshot = git::build_repo_baseline_snapshot(root, current_version).ok()?;
    Some(GitSnapshot {
        branch: snapshot.branch,
        clean: snapshot.clean,
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        commits_since_version: snapshot.commits_since_version,
        baseline_ref: snapshot.baseline_ref,
        baseline_warning: snapshot.baseline_warning,
    })
}

pub(crate) fn resolve_changelog_snapshots(
    components: &[ComponentWithState],
) -> (Option<ReleaseSnapshot>, Option<ChangelogSnapshot>) {
    let wrapper = match components.first() {
        Some(c) => c,
        None => return (None, None),
    };
    let component = &wrapper.component;

    let (last_release, changelog_snapshot) = match changelog::read_component_snapshots(component) {
        Ok((last_release, changelog_snapshot)) => (last_release, changelog_snapshot),
        Err(_) => return (None, None),
    };

    (
        last_release.map(|snapshot| ReleaseSnapshot {
            tag: snapshot.tag,
            date: snapshot.date,
            summary: snapshot.summary,
        }),
        changelog_snapshot.map(|snapshot| ChangelogSnapshot {
            path: snapshot.path,
            label: snapshot.label,
            items: if snapshot.items.is_empty() {
                None
            } else {
                Some(snapshot.items)
            },
        }),
    )
}

pub(crate) fn resolve_agent_context_files(git_root: Option<&String>) -> Vec<String> {
    let root = match git_root {
        Some(r) => r,
        None => return Vec::new(),
    };

    let path = PathBuf::from(root);
    git::list_tracked_markdown_files(&path).unwrap_or_default()
}

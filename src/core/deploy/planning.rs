use std::collections::HashMap;
use std::path::Path;

use crate::component::{self, Component};
use crate::error::{Error, Result};
use crate::extension;
use crate::git;
use crate::project::{self, Project};
use crate::server::SshClient;
use crate::version;

use super::types::{
    ComponentStatus, DeployConfig, ReleaseState, ReleaseStateBuckets, ReleaseStateStatus,
};
use super::version_overrides::fetch_remote_versions;

pub(super) fn calculate_directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total_size = 0;

    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();

            if entry_path.is_dir() {
                total_size += calculate_directory_size(&entry_path)?;
            } else {
                total_size += entry.metadata()?.len();
            }
        }
    } else {
        total_size = path.metadata()?.len();
    }

    Ok(total_size)
}

/// Format bytes into human-readable format.
pub(super) fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", size as u64, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Plan which components to deploy based on config flags.
pub(super) fn plan_components(
    config: &DeployConfig,
    all_components: &[Component],
    skipped_component_ids: &[String],
    base_path: &str,
    client: &SshClient,
) -> Result<Vec<Component>> {
    if !config.component_ids.is_empty() {
        let selected: Vec<Component> = all_components
            .iter()
            .filter(|c| config.component_ids.contains(&c.id))
            .cloned()
            .collect();

        let missing: Vec<String> = config
            .component_ids
            .iter()
            .filter(|id| !selected.iter().any(|c| &c.id == *id))
            .cloned()
            .collect();

        if !missing.is_empty() {
            let non_deployable: Vec<String> = missing
                .iter()
                .filter(|id| skipped_component_ids.contains(*id))
                .cloned()
                .collect();

            let unknown: Vec<String> = missing
                .iter()
                .filter(|id| !non_deployable.contains(*id))
                .cloned()
                .collect();

            let mut details = Vec::new();
            if !unknown.is_empty() {
                details.extend(unknown);
            }
            if !non_deployable.is_empty() {
                details.push(format!(
                    "Non-deployable components (no artifact/deploy strategy): {}",
                    non_deployable.join(", ")
                ));
            }

            return Err(Error::validation_invalid_argument(
                "componentIds",
                "Invalid component selection",
                None,
                Some(details),
            ));
        }

        if selected.is_empty() {
            return Err(Error::validation_invalid_argument(
                "componentIds",
                "No components selected",
                None,
                None,
            ));
        }

        return Ok(selected);
    }

    if config.check {
        return Ok(all_components.to_vec());
    }

    if config.all {
        return Ok(all_components.to_vec());
    }

    if config.outdated {
        let remote_versions = fetch_remote_versions(all_components, base_path, client);

        let selected: Vec<Component> = all_components
            .iter()
            .filter(|c| {
                let Some(local_version) = version::get_component_version(c) else {
                    return true;
                };

                let Some(remote_version) = remote_versions.get(&c.id) else {
                    return true;
                };

                local_version != *remote_version
            })
            .cloned()
            .collect();

        if selected.is_empty() {
            return Err(Error::validation_invalid_argument(
                "outdated",
                "No outdated components found",
                None,
                None,
            ));
        }

        return Ok(selected);
    }

    Err(Error::validation_missing_argument(vec![
        "component IDs, --all, --outdated, or --check".to_string(),
    ]))
}

/// Calculate component status based on local and remote versions.
pub(super) fn calculate_component_status(
    component: &Component,
    remote_versions: &HashMap<String, String>,
) -> ComponentStatus {
    let local_version = version::get_component_version(component);
    let remote_version = remote_versions.get(&component.id);

    match (local_version, remote_version) {
        (None, None) => ComponentStatus::Unknown,
        (None, Some(_)) => ComponentStatus::NeedsUpdate,
        (Some(_), None) => ComponentStatus::NeedsUpdate,
        (Some(local), Some(remote)) => {
            if local == *remote {
                ComponentStatus::UpToDate
            } else {
                ComponentStatus::NeedsUpdate
            }
        }
    }
}

/// Calculate release state for a component.
/// Returns commit count since last version tag and uncommitted changes status.
pub fn calculate_release_state(component: &Component) -> Option<ReleaseState> {
    let path = &component.local_path;

    let current_version = version::read_component_version(component)
        .ok()
        .map(|info| info.version);

    let baseline = git::detect_baseline_with_version(path, current_version.as_deref()).ok()?;

    let commits = git::get_commits_since_tag(path, baseline.reference.as_deref())
        .ok()
        .unwrap_or_default();

    // Categorize commits into code vs docs-only
    let counts = git::categorize_commits(path, &commits);

    let uncommitted = git::get_uncommitted_changes(path)
        .ok()
        .map(|u| u.has_changes)
        .unwrap_or(false);

    Some(ReleaseState {
        commits_since_version: counts.total,
        code_commits: counts.code,
        docs_only_commits: counts.docs_only,
        has_uncommitted_changes: uncommitted,
        baseline_ref: baseline.reference,
        baseline_warning: baseline.warning,
    })
}

pub fn classify_release_state(state: Option<&ReleaseState>) -> ReleaseStateStatus {
    state
        .map(ReleaseState::status)
        .unwrap_or(ReleaseStateStatus::Unknown)
}

pub fn bucket_release_states<'a, I>(components: I) -> ReleaseStateBuckets
where
    I: IntoIterator<Item = (&'a str, Option<&'a ReleaseState>)>,
{
    let mut buckets = ReleaseStateBuckets::default();

    for (component_id, state) in components {
        match classify_release_state(state) {
            ReleaseStateStatus::Uncommitted => {
                buckets.has_uncommitted.push(component_id.to_string())
            }
            ReleaseStateStatus::NeedsBump => buckets.needs_bump.push(component_id.to_string()),
            ReleaseStateStatus::DocsOnly => buckets.docs_only.push(component_id.to_string()),
            ReleaseStateStatus::Clean => buckets.ready_to_deploy.push(component_id.to_string()),
            ReleaseStateStatus::Unknown => buckets.unknown.push(component_id.to_string()),
        }
    }

    buckets
}

/// Result of loading project components, including skipped (non-deployable) component IDs.
pub(super) struct LoadedComponents {
    pub deployable: Vec<Component>,
    pub skipped: Vec<String>,
}

/// Load effective project components, resolve artifact paths via extension patterns,
/// and filter non-deployable.
///
/// Validates that any extensions declared in the component's `extensions` field are installed.
/// Returns an actionable error with install instructions when extensions are missing,
/// rather than silently skipping the component.
///
/// Returns both the deployable components and the IDs of skipped (non-deployable) ones,
/// so callers can produce accurate error messages.
pub(super) fn load_project_components(
    project: &Project,
    requested_ids: &[String],
) -> Result<LoadedComponents> {
    let mut deployable = Vec::new();
    let mut skipped = Vec::new();

    for attachment in &project.components {
        // When specific components are requested, skip extension validation for
        // unrelated components — a missing extension on an unrequested component
        // should not block deploying the ones you asked for.
        let is_requested = requested_ids.is_empty() || requested_ids.contains(&attachment.id);

        let mut loaded = project::resolve_project_component(project, &attachment.id)?;

        // Validate required extensions are installed before attempting artifact resolution.
        // Without this check, missing extensions cause resolve_artifact() to silently
        // return None, and the component gets skipped with a vague "no artifact" message.
        if is_requested {
            extension::validate_required_extensions(&loaded)?;
        } else if extension::validate_required_extensions(&loaded).is_err() {
            log_status!(
                "deploy",
                "Skipping '{}': missing required extension (not requested for deploy)",
                loaded.id
            );
            skipped.push(loaded.id.clone());
            continue;
        }

        // Resolve effective artifact (component value OR extension pattern)
        let effective_artifact = component::resolve_artifact(&loaded);

        // Git-deploy components don't need a build artifact
        let is_git_deploy = loaded.deploy_strategy.as_deref() == Some("git");

        match effective_artifact {
            Some(artifact) if !is_git_deploy => {
                let resolved_artifact =
                    crate::paths::resolve_path_string(&loaded.local_path, &artifact);
                loaded.build_artifact = Some(resolved_artifact);
                deployable.push(loaded);
            }
            _ if is_git_deploy => {
                // Git-deploy components are deployable without an artifact
                deployable.push(loaded);
            }
            Some(_) | None => {
                // Skip - component is intentionally non-deployable
                log_status!(
                    "deploy",
                    "Skipping '{}': no artifact configured (non-deployable component)",
                    loaded.id
                );
                skipped.push(loaded.id.clone());
                continue;
            }
        }
    }

    Ok(LoadedComponents {
        deployable,
        skipped,
    })
}

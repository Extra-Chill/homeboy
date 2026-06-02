use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::component::{self, TargetSpec};
use crate::core::{Error, Result};

use super::{
    sync_workspace, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

const LAB_EXTRA_WORKSPACES_ENV: &str = "HOMEBOY_LAB_EXTRA_WORKSPACES";
const LAB_EXTRA_WORKSPACES_JSON_ENV: &str = "HOMEBOY_LAB_EXTRA_WORKSPACES_JSON";
pub(super) const LAB_WORKSPACE_MAPPING_SCHEMA: &str = "homeboy/workspace-map/v1";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct LabWorkspaceMappingEntry {
    role: String,
    local_path: String,
    remote_path: String,
    sync_mode: String,
    snapshot_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExtraLabWorkspace {
    role: String,
    path: PathBuf,
}

pub(super) fn sync_extra_lab_workspaces(
    runner_id: &str,
    primary_local_path: &str,
    extra_workspaces: Vec<ExtraLabWorkspace>,
    workspace_mapping: &mut Vec<LabWorkspaceMappingEntry>,
) -> Result<Vec<LabWorkspaceMappingEntry>> {
    let primary = canonical_existing_dir(primary_local_path, "path")?;
    let mut seen = HashSet::from([primary]);
    let mut synced_entries = Vec::new();

    for extra in extra_workspaces {
        let local_path = canonical_existing_dir(&extra.path.display().to_string(), "workspace")?;
        if !seen.insert(local_path.clone()) {
            continue;
        }
        let synced = sync_workspace(
            runner_id,
            RunnerWorkspaceSyncOptions {
                path: local_path.display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                changed_since_base: None,
            },
        )?
        .0;
        let entry = workspace_mapping_entry(&extra.role, &synced);
        workspace_mapping.push(entry.clone());
        synced_entries.push(entry);
    }

    Ok(synced_entries)
}

pub(super) fn workspace_mapping_entry(
    role: impl Into<String>,
    synced: &RunnerWorkspaceSyncOutput,
) -> LabWorkspaceMappingEntry {
    LabWorkspaceMappingEntry {
        role: role.into(),
        local_path: synced.local_path.clone(),
        remote_path: synced.remote_path.clone(),
        sync_mode: synced.sync_mode.label().to_string(),
        snapshot_identity: synced.snapshot_identity.clone(),
    }
}

pub(super) fn lab_workspace_mapping_metadata(
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> serde_json::Value {
    let local_to_remote = workspace_mapping
        .iter()
        .map(|entry| {
            (
                entry.local_path.clone(),
                serde_json::Value::String(entry.remote_path.clone()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::json!({
        "schema": LAB_WORKSPACE_MAPPING_SCHEMA,
        "workspaces": workspace_mapping,
        "local_to_remote": local_to_remote,
    })
}

pub(super) fn lab_extra_workspaces(source_path: &Path) -> Result<Vec<ExtraLabWorkspace>> {
    let mut workspaces = accepted_extra_lab_workspaces()?;
    workspaces.extend(discovered_validation_dependency_workspaces(source_path)?);
    Ok(workspaces)
}

fn accepted_extra_lab_workspaces() -> Result<Vec<ExtraLabWorkspace>> {
    let mut paths = Vec::new();
    if let Ok(raw) = std::env::var(LAB_EXTRA_WORKSPACES_JSON_ENV) {
        if !raw.trim().is_empty() {
            let parsed: Vec<String> = serde_json::from_str(&raw).map_err(|err| {
                Error::validation_invalid_argument(
                    LAB_EXTRA_WORKSPACES_JSON_ENV,
                    format!("{LAB_EXTRA_WORKSPACES_JSON_ENV} must be a JSON array of paths: {err}"),
                    Some(raw.clone()),
                    None,
                )
            })?;
            paths.extend(parsed);
        }
    }
    if let Ok(raw) = std::env::var(LAB_EXTRA_WORKSPACES_ENV) {
        paths.extend(
            raw.lines()
                .flat_map(|line| line.split(','))
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_string),
        );
    }

    paths
        .into_iter()
        .map(|path| {
            Ok(ExtraLabWorkspace {
                role: "extra".to_string(),
                path: canonical_existing_dir(&path, "extra_workspace")?,
            })
        })
        .collect()
}

fn discovered_validation_dependency_workspaces(
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let source_path_string = source_path.display().to_string();
    let Ok(target) = component::resolve_target(TargetSpec {
        component_id: None,
        path_override: Some(&source_path_string),
        project: None,
        capability: None,
        allow_synthetic: true,
        accept_bare_directory: true,
    }) else {
        return Ok(Vec::new());
    };
    let Some(extensions) = target.component.extensions.as_ref() else {
        return Ok(Vec::new());
    };

    let mut workspaces = Vec::new();
    for config in extensions.values() {
        let Some(dependencies) = config.settings.get("validation_dependencies") else {
            continue;
        };
        let Some(dependencies) = dependencies.as_array() else {
            continue;
        };
        for dependency in dependencies.iter().filter_map(|value| value.as_str()) {
            let path = resolve_dependency_workspace_path(dependency)?;
            workspaces.push(ExtraLabWorkspace {
                role: "dependency".to_string(),
                path,
            });
        }
    }

    Ok(workspaces)
}

fn resolve_dependency_workspace_path(dependency: &str) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(dependency).to_string();
    if Path::new(&expanded).is_dir() {
        return canonical_existing_dir(&expanded, "validation_dependencies");
    }

    let component = component::resolve_effective(Some(dependency), None, None).map_err(|err| {
        Error::validation_invalid_argument(
            "validation_dependencies",
            format!(
                "Lab offload cannot resolve validation dependency `{dependency}` to a local checkout: {}",
                err.message
            ),
            Some(dependency.to_string()),
            Some(vec![
                "Register the dependency component locally, or pass an explicit checkout path via HOMEBOY_LAB_EXTRA_WORKSPACES_JSON.".to_string(),
            ]),
        )
    })?;
    canonical_existing_dir(&component.local_path, "validation_dependencies")
}

fn canonical_existing_dir(path: &str, field: &str) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(path).to_string();
    let path = Path::new(&expanded);
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            field,
            format!("Lab offload workspace path must be an existing directory: {expanded}"),
            Some(expanded),
            None,
        ));
    }
    path.canonicalize().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("canonicalize Lab workspace path".to_string()),
        )
    })
}

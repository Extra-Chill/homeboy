use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::component::{self, TargetSpec};
use crate::core::{Error, Result};

use super::{
    sync_workspace, RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

const LAB_EXTRA_WORKSPACES_ENV: &str = concat!("HOME", "BOY_LAB_EXTRA_WORKSPACES");
const LAB_EXTRA_WORKSPACES_JSON_ENV: &str = concat!("HOME", "BOY_LAB_EXTRA_WORKSPACES_JSON");
pub(super) const LAB_WORKSPACE_MAPPING_SCHEMA: &str = concat!("home", "boy/workspace-map/v1");

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct LabWorkspaceMappingEntry {
    role: String,
    local_path: String,
    remote_path: String,
    sync_mode: String,
    snapshot_identity: String,
}

impl LabWorkspaceMappingEntry {
    pub(super) fn local_path(&self) -> &str {
        &self.local_path
    }

    pub(super) fn remote_path(&self) -> &str {
        &self.remote_path
    }
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

pub(super) fn workspace_mapping_entry_for_git_dependency(
    role: impl Into<String>,
    dependency: &RunnerGitDependencyMaterializationOutput,
) -> LabWorkspaceMappingEntry {
    LabWorkspaceMappingEntry {
        role: role.into(),
        local_path: dependency.local_path.clone(),
        remote_path: dependency.remote_path.clone(),
        sync_mode: "git".to_string(),
        snapshot_identity: dependency.head.clone(),
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

/// Discover controller-local directories referenced by a `--provider-config` in
/// the offloaded args (runtime component paths, provider plugin paths, mount
/// sources, workspace root) so they are synced to the runner and remappable.
///
/// Directories under `source_path` are skipped because the primary workspace
/// sync already covers them. Non-existent or non-directory paths are ignored;
/// they will simply not be remapped.
pub(super) fn provider_config_extra_workspaces(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let Some(spec) = provider_config_spec(args) else {
        return Ok(Vec::new());
    };
    let raw = match crate::core::config::read_json_spec_to_string(&spec) {
        Ok(raw) => raw,
        Err(_) => return Ok(Vec::new()),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(Vec::new()),
    };

    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());

    let mut seen = std::collections::BTreeSet::new();
    let mut workspaces = Vec::new();
    for candidate in provider_config_candidate_paths(&value) {
        let expanded = shellexpand::tilde(&candidate).to_string();
        let path = Path::new(&expanded);
        if !path.is_dir() {
            continue;
        }
        let canon = match path.canonicalize() {
            Ok(canon) => canon,
            Err(_) => continue,
        };
        // The primary workspace (and paths inside it) are already synced.
        if canon == source_canon || canon.starts_with(&source_canon) {
            continue;
        }
        if !seen.insert(canon.clone()) {
            continue;
        }
        workspaces.push(ExtraLabWorkspace {
            role: "provider_config".to_string(),
            path: canon,
        });
    }
    Ok(workspaces)
}

fn provider_config_spec(args: &[String]) -> Option<String> {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--provider-config" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--provider-config=") {
            return Some(value.to_string());
        }
    }
    None
}

fn provider_config_candidate_paths(value: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(root) = value.get("workspace_root").and_then(|v| v.as_str()) {
        paths.push(root.to_string());
    }
    if let Some(mounts) = value.get("mounts").and_then(|v| v.as_array()) {
        for mount in mounts {
            if let Some(src) = mount.get("source").and_then(|v| v.as_str()) {
                paths.push(src.to_string());
            }
        }
    }
    if let Some(components) = value
        .get("runtime_component_paths")
        .and_then(|v| v.as_object())
    {
        for component in components.values() {
            if let Some(path) = component.as_str() {
                paths.push(path.to_string());
            }
        }
    }
    if let Some(plugins) = value
        .get("provider_plugin_paths")
        .and_then(|v| v.as_array())
    {
        for plugin in plugins {
            if let Some(path) = plugin.as_str() {
                paths.push(path.to_string());
            }
        }
    }
    // Runtime overlay sources (e.g. a bundled helper library build that
    // supplies provider APIs) are controller-local directories the
    // sandbox mounts; sync them so the overlay resolves on the runner.
    if let Some(overlays) = value.get("runtime_overlays").and_then(|v| v.as_array()) {
        for overlay in overlays {
            if let Some(src) = overlay.get("source").and_then(|v| v.as_str()) {
                paths.push(src.to_string());
            }
        }
    }
    for key in [
        "agents_api",
        "agents_api_path",
        "homeboy_extensions",
        "homeboy_extensions_path",
    ] {
        if let Some(path) = value.get(key).and_then(|v| v.as_str()) {
            paths.push(path.to_string());
        }
    }
    paths
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
                "Runner workspace sync cannot resolve validation dependency `{dependency}` to a local checkout: {}",
                err.message
            ),
            Some(dependency.to_string()),
            Some(vec![
                format!("Register the dependency component locally, or pass an explicit checkout path via {LAB_EXTRA_WORKSPACES_JSON_ENV}."),
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
            format!("Runner workspace path must be an existing directory: {expanded}"),
            Some(expanded),
            None,
        ));
    }
    path.canonicalize().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("canonicalize runner workspace path".to_string()),
        )
    })
}

#[cfg(test)]
mod provider_config_candidate_paths_tests {
    use super::provider_config_candidate_paths;

    #[test]
    fn extracts_all_local_path_sources_including_runtime_overlays() {
        let value = serde_json::json!({
            "workspace_root": "/local/data-machine@cook",
            "mounts": [{ "source": "/local/data-machine@cook", "target": "/workspace/data-machine" }],
            "runtime_component_paths": {
                "agent_runtime": "/local/data-machine",
                "agent_runtime_tools": "/local/data-machine-code"
            },
            "provider_plugin_paths": ["/local/ai-provider-for-claude-code"],
            "runtime_overlays": [
                { "kind": "bundled-library", "library": "client-library", "source": "/local/client-library@custom-provider-auth", "target": "/workspace/vendor/client-library" }
            ],
            "agents_api": "/local/agents-api",
            "model": "claude-opus-4-8"
        });

        let paths = provider_config_candidate_paths(&value);

        for expected in [
            "/local/data-machine@cook",
            "/local/data-machine",
            "/local/data-machine-code",
            "/local/ai-provider-for-claude-code",
            "/local/client-library@custom-provider-auth",
            "/local/agents-api",
        ] {
            assert!(
                paths.iter().any(|p| p == expected),
                "missing candidate path: {expected}"
            );
        }
        // Non-path scalars are not collected.
        assert!(!paths.iter().any(|p| p == "claude-opus-4-8"));
    }

    #[test]
    fn empty_config_yields_no_candidates() {
        let value = serde_json::json!({ "model": "x" });
        assert!(provider_config_candidate_paths(&value).is_empty());
    }
}

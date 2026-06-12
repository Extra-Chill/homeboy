use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::component::{self, TargetSpec};
use crate::core::{Error, Result};

use super::{
    exec, sync_workspace, workspace::git_output, RunnerExecOptions,
    RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
    RunnerWorkspaceSyncOutput,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dependency_freshness: Option<serde_json::Value>,
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
    snapshot_includes: Vec<String>,
    bootstrap_node_dependencies: bool,
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
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: extra.snapshot_includes.clone(),
            },
        )?
        .0;
        let entry = workspace_mapping_entry(&extra.role, &synced);
        if extra.bootstrap_node_dependencies {
            bootstrap_source_cli_node_dependencies(runner_id, &synced.remote_path)?;
        }
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
        dependency_freshness: None,
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
        sync_mode: dependency.sync_mode.label().to_string(),
        snapshot_identity: dependency.head.clone(),
        dependency_freshness: Some(serde_json::json!({
            "local_path": dependency.local_path.as_str(),
            "remote": dependency.remote_url.as_str(),
            "before_sha": dependency.before_sha.as_deref(),
            "after_sha": dependency.after_sha.as_deref(),
            "upstream_sha": dependency.upstream_sha.as_deref(),
            "upstream": dependency.upstream.as_deref(),
            "status": dependency.status.as_str(),
            "pinned_ref": dependency.pinned_ref.as_deref(),
            "used_pinned_ref": dependency.used_pinned_ref,
        })),
    }
}

pub(super) fn workspace_mapping_entries_for_git_dependency(
    role: impl Into<String>,
    dependency: &RunnerGitDependencyMaterializationOutput,
) -> Vec<LabWorkspaceMappingEntry> {
    let role = role.into();
    let mut entries = vec![workspace_mapping_entry_for_git_dependency(
        role.clone(),
        dependency,
    )];
    if let Some(subpath) = dependency
        .required_subpath
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        entries.push(LabWorkspaceMappingEntry {
            role,
            local_path: Path::new(&dependency.local_path)
                .join(subpath)
                .display()
                .to_string(),
            remote_path: Path::new(&dependency.remote_path)
                .join(subpath)
                .display()
                .to_string(),
            sync_mode: dependency.sync_mode.label().to_string(),
            snapshot_identity: dependency.head.clone(),
            dependency_freshness: None,
        });
    }
    entries
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
/// sync already covers them. Existing files are mapped to their containing git
/// checkout when available, falling back to their parent directory.
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

    let mut seen = BTreeSet::new();
    let mut workspaces: Vec<ExtraLabWorkspace> = Vec::new();
    for candidate in provider_config_candidate_paths(&value) {
        add_candidate_extra_workspace(
            &candidate,
            "provider_config",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }
    Ok(workspaces)
}

/// Discover a controller-local `agent-task run-plan --plan @file` checkout so
/// Lab offload can remap the plan path instead of asking the runner to read a
/// controller-only filesystem path.
pub(super) fn agent_task_plan_extra_workspaces(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let Some(spec) = agent_task_plan_spec(args) else {
        return Ok(Vec::new());
    };
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();

    if let Some(path) = spec.strip_prefix('@') {
        let expanded = shellexpand::tilde(path).to_string();
        let path = Path::new(&expanded);
        if path.is_file() {
            let workspace_path = containing_checkout_or_parent(path)?;
            if let Ok(canon) = workspace_path.canonicalize() {
                if canon != source_canon && !canon.starts_with(&source_canon) {
                    seen.insert(canon.clone());
                    workspaces.push(ExtraLabWorkspace {
                        role: "agent_task_plan".to_string(),
                        path: canon,
                        snapshot_includes: Vec::new(),
                        bootstrap_node_dependencies: false,
                    });
                }
            }
        }
    }

    let raw = match crate::core::config::read_json_spec_to_string(&spec) {
        Ok(raw) => raw,
        Err(_) => return Ok(workspaces),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(workspaces),
    };
    for candidate in provider_config_candidate_paths(&value) {
        add_candidate_extra_workspace(
            &candidate,
            "agent_task_plan_config",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }

    Ok(workspaces)
}

pub(super) fn rig_component_path_env_extra_workspaces(
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    rig_component_path_env_extra_workspaces_from_entries(source_path, std::env::vars())
}

fn rig_component_path_env_extra_workspaces_from_entries(
    source_path: &Path,
    entries: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<ExtraLabWorkspace>> {
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();

    for (name, value) in entries
        .into_iter()
        .filter(|(name, value)| is_rig_component_path_env_name(name) && !value.trim().is_empty())
    {
        let expanded = shellexpand::tilde(&value).to_string();
        let path = Path::new(&expanded);
        if !path.exists() {
            return Err(Error::validation_invalid_argument(
                name.clone(),
                format!(
                    "Lab offload cannot forward `{name}` because its controller-side path does not exist"
                ),
                Some(value.clone()),
                Some(vec![
                    format!("Controller-side value: {value}"),
                    "Set the variable to an existing checkout/component path before offload, unset it to use the rig default, or run with --force-hot to keep the check local.".to_string(),
                ]),
            ));
        }
        add_candidate_extra_workspace(
            &value,
            "rig_component_path_env",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }

    Ok(workspaces)
}

pub(super) fn is_rig_component_path_env_name(name: &str) -> bool {
    name.starts_with("HOMEBOY_") && name.ends_with("_COMPONENT_PATH")
}

pub(super) fn preflight_provider_config_source_cli_dependencies(
    args: &[String],
    snapshot_excludes: &[String],
) -> Result<()> {
    if !snapshot_excludes
        .iter()
        .any(|exclude| exclude == "node_modules" || exclude == "node_modules/**")
    {
        return Ok(());
    }

    let Some(spec) = provider_config_spec(args) else {
        return Ok(());
    };
    let raw = match crate::core::config::read_json_spec_to_string(&spec) {
        Ok(raw) => raw,
        Err(_) => return Ok(()),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };

    for file in provider_config_source_cli_files(&value) {
        let content = match fs::read_to_string(&file) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let imports = bare_module_imports(&content);
        if let Some(package) = imports.iter().next() {
            return Err(Error::validation_invalid_argument(
                "provider_config",
                format!(
                    "Lab offload cannot preflight source-built CLI `{}` because it imports package `{}` while node_modules is excluded from the synced snapshot",
                    file.display(),
                    package
                ),
                Some(file.display().to_string()),
                Some(vec![
                    format!(
                        "Materialize `{}` on the runner before execution, bundle it into the CLI artifact, or adjust runner snapshot policy to include the dependency path.",
                        package
                    ),
                    "Use runner policy snapshot_includes for generated CLI outputs that must travel with the snapshot.".to_string(),
                ]),
            ));
        }
    }

    Ok(())
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

fn agent_task_plan_spec(args: &[String]) -> Option<String> {
    let run_plan_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 1)
    })?;

    let mut iter = args.iter().skip(run_plan_index + 1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--plan" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--plan=") {
            return Some(value.to_string());
        }
    }
    None
}

fn subcommand_index(args: &[String], subcommand: &str) -> Option<usize> {
    args.iter().position(|arg| arg == subcommand)
}

fn provider_config_candidate_paths(value: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_provider_config_candidate_paths(value, &mut paths);
    paths
}

fn add_candidate_extra_workspace(
    candidate: &str,
    role: &str,
    source_canon: &Path,
    seen: &mut BTreeSet<PathBuf>,
    workspaces: &mut Vec<ExtraLabWorkspace>,
) -> Result<()> {
    let expanded = shellexpand::tilde(candidate).to_string();
    let path = Path::new(&expanded);
    let (workspace_path, snapshot_includes, bootstrap_node_dependencies) = if path.is_dir() {
        (path.to_path_buf(), Vec::new(), false)
    } else if path.is_file() {
        let workspace_path = containing_checkout_or_parent(path)?;
        let snapshot_includes = provider_config_file_snapshot_includes(&workspace_path, path);
        (
            workspace_path,
            snapshot_includes,
            is_node_cli_file(path) && source_cli_workspace_has_package_lock(path),
        )
    } else {
        return Ok(());
    };
    let canon = match workspace_path.canonicalize() {
        Ok(canon) => canon,
        Err(_) => return Ok(()),
    };
    // The primary workspace (and paths inside it) are already synced.
    if canon == source_canon || canon.starts_with(source_canon) {
        return Ok(());
    }
    if !seen.insert(canon.clone()) {
        if let Some(existing) = workspaces
            .iter_mut()
            .find(|workspace| workspace.path == canon)
        {
            for include in snapshot_includes {
                if !existing.snapshot_includes.contains(&include) {
                    existing.snapshot_includes.push(include);
                }
            }
            existing.bootstrap_node_dependencies |= bootstrap_node_dependencies;
        }
        return Ok(());
    }
    workspaces.push(ExtraLabWorkspace {
        role: role.to_string(),
        path: canon,
        snapshot_includes,
        bootstrap_node_dependencies,
    });
    Ok(())
}

fn collect_provider_config_candidate_paths(value: &serde_json::Value, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if is_controller_path_like(text) {
                paths.push(text.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_provider_config_candidate_paths(item, paths);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_provider_config_candidate_paths(item, paths);
            }
        }
        _ => {}
    }
}

fn is_controller_path_like(value: &str) -> bool {
    value.starts_with('/') || value.starts_with("~/")
}

fn provider_config_source_cli_files(value: &serde_json::Value) -> Vec<PathBuf> {
    provider_config_candidate_paths(value)
        .into_iter()
        .map(|candidate| shellexpand::tilde(&candidate).to_string())
        .map(PathBuf::from)
        .filter(|path| path.is_file() && is_node_cli_file(path))
        .collect()
}

fn is_node_cli_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("js" | "mjs" | "cjs")
    )
}

fn containing_checkout_or_parent(path: &Path) -> Result<PathBuf> {
    let dir = path.parent().unwrap_or(path);
    if let Ok(root) = git_output(dir, &["rev-parse", "--show-toplevel"]) {
        return Ok(PathBuf::from(root));
    }
    canonical_existing_dir(&dir.display().to_string(), "provider_config")
}

fn bare_module_imports(content: &str) -> BTreeSet<String> {
    let mut imports = BTreeSet::new();
    for marker in [
        "from '",
        "from \"",
        "require('",
        "require(\"",
        "import('",
        "import(\"",
    ] {
        collect_imports_after_marker(content, marker, &mut imports);
    }
    imports
        .into_iter()
        .filter(|specifier| {
            !specifier.starts_with('.')
                && !specifier.starts_with('/')
                && !is_builtin_module_specifier(specifier)
        })
        .collect()
}

fn is_builtin_module_specifier(specifier: &str) -> bool {
    let specifier = specifier.strip_prefix("node:").unwrap_or(specifier);
    matches!(
        specifier,
        "assert"
            | "buffer"
            | "child_process"
            | "crypto"
            | "events"
            | "fs"
            | "http"
            | "https"
            | "module"
            | "os"
            | "path"
            | "process"
            | "stream"
            | "url"
            | "util"
    )
}

fn collect_imports_after_marker(content: &str, marker: &str, imports: &mut BTreeSet<String>) {
    let quote = marker.chars().last().unwrap_or('\'');
    let mut rest = content;
    while let Some(index) = rest.find(marker) {
        let after = &rest[index + marker.len()..];
        if let Some(end) = after.find(quote) {
            imports.insert(after[..end].to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
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
                snapshot_includes: Vec::new(),
                bootstrap_node_dependencies: false,
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
                snapshot_includes: Vec::new(),
                bootstrap_node_dependencies: false,
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

fn provider_config_file_snapshot_includes(workspace_path: &Path, file_path: &Path) -> Vec<String> {
    let workspace_path = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let file_path = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    let Ok(relative) = file_path.strip_prefix(&workspace_path) else {
        return Vec::new();
    };
    let Some(parent) = relative.parent() else {
        return Vec::new();
    };
    let parent = parent.display().to_string();
    if parent.is_empty() {
        return Vec::new();
    }
    vec![parent.clone(), format!("{parent}/**")]
}

fn source_cli_workspace_has_package_lock(file_path: &Path) -> bool {
    containing_checkout_or_parent(file_path)
        .ok()
        .is_some_and(|workspace| workspace.join("package-lock.json").is_file())
}

fn bootstrap_source_cli_node_dependencies(runner_id: &str, remote_path: &str) -> Result<()> {
    let (output, exit_code) = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_path.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command: vec![
                "npm".to_string(),
                "ci".to_string(),
                "--omit=dev".to_string(),
                "--ignore-scripts".to_string(),
            ],
            env: HashMap::new(),
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
        },
    )?;
    if exit_code == 0 {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "provider_config",
        format!(
            "Lab offload could not install production dependencies for source-built CLI workspace `{remote_path}`"
        ),
        Some(remote_path.to_string()),
        Some(vec![
            format!("npm ci stderr: {}", output.stderr.trim()),
            "Build or package the CLI as a self-contained artifact, or make the source-built workspace installable on the runner.".to_string(),
        ]),
    ))
}

#[cfg(test)]
mod provider_config_candidate_paths_tests {
    use std::path::Path;
    use std::process::Command;

    use super::{
        agent_task_plan_extra_workspaces, agent_task_plan_spec,
        preflight_provider_config_source_cli_dependencies, provider_config_candidate_paths,
        provider_config_extra_workspaces, rig_component_path_env_extra_workspaces_from_entries,
        workspace_mapping_entries_for_git_dependency,
    };
    use crate::core::runner::{RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode};

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

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
                { "kind": "bundled-library", "library": "portable-ai-client", "source": "/local/portable-ai-client@custom-provider-auth", "target": "/runtime/includes/portable-ai-client" }
            ],
            "agents_api": "/local/agents-api",
            "source_cli": "/local/provider/packages/cli/dist/index.js",
            "model": "claude-opus-4-8"
        });

        let paths = provider_config_candidate_paths(&value);

        for expected in [
            "/local/data-machine@cook",
            "/local/data-machine",
            "/local/data-machine-code",
            "/local/ai-provider-for-claude-code",
            "/local/portable-ai-client@custom-provider-auth",
            "/local/agents-api",
            "/local/provider/packages/cli/dist/index.js",
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

    #[test]
    fn agent_task_plan_spec_allows_global_flags_before_agent_task() {
        let args = vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan=@/tmp/plan.json".to_string(),
        ];

        assert_eq!(
            agent_task_plan_spec(&args),
            Some("@/tmp/plan.json".to_string())
        );
    }

    #[test]
    fn provider_config_file_path_syncs_containing_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let provider = controller.path().join("provider-cli");
        let cli = provider.join("packages/cli/dist/index.js");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(cli.parent().unwrap()).expect("cli dist dir");
        std::fs::write(&cli, "#!/usr/bin/env node\n").expect("cli file");
        std::fs::write(provider.join("package-lock.json"), "{}\n").expect("package lock");
        git(&provider, &["init", "-b", "main"]);
        git(&provider, &["config", "user.email", "test@example.com"]);
        git(&provider, &["config", "user.name", "Homeboy Test"]);
        git(&provider, &["add", "."]);
        git(&provider, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({ "source_cli": cli }).to_string(),
        ];

        let workspaces = provider_config_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].path, provider.canonicalize().unwrap());
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[0].bootstrap_node_dependencies);
    }

    #[test]
    fn provider_config_file_path_merges_snapshot_includes_for_duplicate_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let provider = controller.path().join("provider-cli");
        let cli = provider.join("packages/cli/dist/index.js");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(cli.parent().unwrap()).expect("cli dist dir");
        std::fs::write(&cli, "#!/usr/bin/env node\n").expect("cli file");
        std::fs::write(provider.join("package-lock.json"), "{}\n").expect("package lock");
        git(&provider, &["init", "-b", "main"]);
        git(&provider, &["config", "user.email", "test@example.com"]);
        git(&provider, &["config", "user.name", "Homeboy Test"]);
        git(&provider, &["add", "."]);
        git(&provider, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({
                "provider_root": provider,
                "source_cli": cli,
            })
            .to_string(),
        ];

        let workspaces = provider_config_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[0].bootstrap_node_dependencies);
    }

    #[test]
    fn agent_task_run_plan_file_path_syncs_containing_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let planner = controller.path().join("plan-owner");
        let codebox = controller.path().join("wp-codebox");
        let codebox_bin = codebox.join("packages/cli/dist/index.js");
        let plan = planner.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::create_dir_all(codebox_bin.parent().unwrap()).expect("codebox cli dir");
        std::fs::write(&codebox_bin, "#!/usr/bin/env node\n").expect("codebox bin");
        std::fs::write(codebox.join("package-lock.json"), "{}\n").expect("package lock");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "site-generation-loop",
                "tasks": [{
                    "task_id": "task-1",
                    "executor": {
                        "backend": "wp-codebox",
                        "config": {
                            "wp_codebox_bin": codebox_bin,
                            "artifact_root": planner.join("artifacts")
                        }
                    },
                    "instructions": "test"
                }]
            })
            .to_string(),
        )
        .expect("plan file");
        git(&planner, &["init", "-b", "main"]);
        git(&planner, &["config", "user.email", "test@example.com"]);
        git(&planner, &["config", "user.name", "Homeboy Test"]);
        git(&planner, &["add", "."]);
        git(&planner, &["commit", "-m", "initial"]);
        git(&codebox, &["init", "-b", "main"]);
        git(&codebox, &["config", "user.email", "test@example.com"]);
        git(&codebox, &["config", "user.name", "Homeboy Test"]);
        git(&codebox, &["add", "."]);
        git(&codebox, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            format!("@{}", plan.display()),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[0].role, "agent_task_plan");
        assert_eq!(workspaces[0].path, planner.canonicalize().unwrap());
        assert!(workspaces[0].snapshot_includes.is_empty());
        assert!(!workspaces[0].bootstrap_node_dependencies);
        assert_eq!(workspaces[1].role, "agent_task_plan_config");
        assert_eq!(workspaces[1].path, codebox.canonicalize().unwrap());
        assert!(workspaces[1]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[1].bootstrap_node_dependencies);
    }

    #[test]
    fn agent_task_run_plan_file_inside_primary_workspace_needs_no_extra_sync() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let plan = source.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::write(
            &plan,
            "{\"schema\":\"homeboy/agent-task-plan/v1\",\"tasks\":[]}\n",
        )
        .expect("plan file");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            format!("--plan=@{}", plan.display()),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert!(workspaces.is_empty());
    }

    #[test]
    fn rig_component_path_env_extra_workspaces_syncs_existing_component_path() {
        crate::test_support::with_isolated_home(|home| {
            let source = home.path().join("primary");
            std::fs::create_dir_all(&source).expect("source path");
            let component_path = home.path().join("Developer/plugin/includes");
            std::fs::create_dir_all(&component_path).expect("component path");

            let workspaces = rig_component_path_env_extra_workspaces_from_entries(
                &source,
                [(
                    "HOMEBOY_TEST_COMPONENT_PATH".to_string(),
                    component_path.display().to_string(),
                )],
            )
            .expect("workspaces");

            assert_eq!(workspaces.len(), 1);
            assert_eq!(workspaces[0].role, "rig_component_path_env");
            assert_eq!(workspaces[0].path, component_path.canonicalize().unwrap());
        });
    }

    #[test]
    fn rig_component_path_env_extra_workspaces_rejects_missing_component_path() {
        crate::test_support::with_isolated_home(|home| {
            let missing = home.path().join("missing-plugin");

            let err = rig_component_path_env_extra_workspaces_from_entries(
                home.path(),
                [(
                    "HOMEBOY_MISSING_COMPONENT_PATH".to_string(),
                    missing.display().to_string(),
                )],
            )
            .expect_err("missing path");

            assert_eq!(err.details["field"], "HOMEBOY_MISSING_COMPONENT_PATH");
            assert!(err.message.contains("controller-side path does not exist"));
        });
    }

    #[test]
    #[cfg(unix)]
    fn agent_task_run_plan_syncs_symlinked_dependency_target_inside_primary_workspace() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let codebox = controller.path().join("wp-codebox");
        let codebox_bin = codebox.join("packages/cli/dist/index.js");
        let symlink = source.join(".ci/wp-codebox");
        let plan = source.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(symlink.parent().unwrap()).expect("ci dir");
        std::fs::create_dir_all(codebox_bin.parent().unwrap()).expect("codebox cli dir");
        std::fs::write(&codebox_bin, "#!/usr/bin/env node\n").expect("codebox bin");
        std::fs::write(codebox.join("package-lock.json"), "{}\n").expect("package lock");
        std::os::unix::fs::symlink(&codebox, &symlink).expect("codebox symlink");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "site-generation-loop",
                "tasks": [{
                    "task_id": "task-1",
                    "executor": {
                        "backend": "wp-codebox",
                        "config": {
                            "wp_codebox_bin": symlink.join("packages/cli/dist/index.js")
                        }
                    },
                    "instructions": "test"
                }]
            })
            .to_string(),
        )
        .expect("plan file");
        git(&codebox, &["init", "-b", "main"]);
        git(&codebox, &["config", "user.email", "test@example.com"]);
        git(&codebox, &["config", "user.name", "Homeboy Test"]);
        git(&codebox, &["add", "."]);
        git(&codebox, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            format!("@{}", plan.display()),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "agent_task_plan_config");
        assert_eq!(workspaces[0].path, codebox.canonicalize().unwrap());
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[0].bootstrap_node_dependencies);
    }

    #[test]
    fn rig_dependency_workspace_mapping_uses_dependency_sync_mode_and_subpath() {
        let dependency = RunnerGitDependencyMaterializationOutput {
            local_path: "/local/example-repo".to_string(),
            remote_path: "/remote/example-repo".to_string(),
            remote_url: "https://example.test/example/repo.git".to_string(),
            head: "snapshot:abc".to_string(),
            status: "snapshotted".to_string(),
            branch: Some("main".to_string()),
            before_sha: Some("abc".to_string()),
            after_sha: Some("abc".to_string()),
            upstream_sha: Some("abc".to_string()),
            upstream: Some("origin/main".to_string()),
            pinned_ref: None,
            required_subpath: Some("packages/component".to_string()),
            used_pinned_ref: false,
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            files: 7,
            bytes: 42,
        };

        let entries =
            workspace_mapping_entries_for_git_dependency("rig_component_dependency", &dependency);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].local_path, "/local/example-repo");
        assert_eq!(entries[0].remote_path, "/remote/example-repo");
        assert_eq!(entries[0].sync_mode, "snapshot");
        assert_eq!(entries[0].snapshot_identity, "snapshot:abc");
        assert_eq!(
            entries[0].dependency_freshness.as_ref().unwrap()["upstream"],
            "origin/main"
        );
        assert_eq!(
            entries[0].dependency_freshness.as_ref().unwrap()["after_sha"],
            "abc"
        );
        assert_eq!(
            entries[1].local_path,
            "/local/example-repo/packages/component"
        );
        assert_eq!(
            entries[1].remote_path,
            "/remote/example-repo/packages/component"
        );
        assert_eq!(entries[1].sync_mode, "snapshot");
        assert_eq!(entries[1].snapshot_identity, "snapshot:abc");
        assert!(entries[1].dependency_freshness.is_none());
    }

    #[test]
    fn source_cli_preflight_names_missing_workspace_package_and_importer() {
        let provider = tempfile::tempdir().expect("provider checkout");
        let cli = provider.path().join("packages/cli/dist/index.js");
        std::fs::create_dir_all(cli.parent().unwrap()).expect("cli dist dir");
        std::fs::write(
            &cli,
            "import { run } from '@example/provider-core';\nrun();\n",
        )
        .expect("cli file");
        git(provider.path(), &["init", "-b", "main"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({ "source_cli": cli }).to_string(),
        ];
        let excludes = vec!["node_modules".to_string(), "node_modules/**".to_string()];

        let err = preflight_provider_config_source_cli_dependencies(&args, &excludes)
            .expect_err("workspace package import should fail preflight");

        assert_eq!(err.details["field"], "provider_config");
        assert!(err.message.contains("@example/provider-core"));
        assert!(err.message.contains("index.js"));
    }
}

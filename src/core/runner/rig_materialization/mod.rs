use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::core::component::Component;
use crate::core::rig;
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{Error, Result};
use crate::extensions::deps_provider;

use super::{
    exec, load, materialize_git_dependency, preflight_remote_argv_path_translation, sync_workspace,
    workspace::{parent_remote_path, sanitize_path_segment},
    RunnerExecOptions, RunnerGitDependencyMaterializationOptions,
    RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};

mod rig_source_install;
use rig_source_install::{
    remote_package_path, remove_runner_installed_rig_source, rig_install_capability_preflight,
    validate_installed_rig_source,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RigComponentDependency {
    pub rig_id: String,
    pub component_id: String,
    pub local_checkout_root: String,
    pub declared_checkout_root: String,
    pub remote_checkout_root: String,
    pub required_subpath: Option<String>,
    pub remote_url: Option<String>,
    pub pinned_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct LabOffloadRigSync {
    pub rig_id: String,
    pub source: String,
    pub source_kind: LabOffloadRigSyncSource,
    pub package_source: LabOffloadRigPackageSource,
    pub workload_hashes: LabOffloadRigWorkloadHashes,
    pub source_snapshot: SourceSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_installed_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct LabOffloadRigPackageSource {
    pub source: String,
    pub source_root: String,
    pub package_path: String,
    pub install_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub linked: bool,
    pub materialized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct LabOffloadRigWorkloadHashes {
    pub source_snapshot_hash: String,
    pub workspace_snapshot_identity: String,
}

pub(super) struct LabOffloadPrimaryRigSource<'a> {
    pub local_path: &'a str,
    pub remote_path: &'a str,
    pub source_snapshot: &'a SourceSnapshot,
    pub workspace_snapshot_identity: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LabOffloadRigSyncSource {
    PrimarySnapshot,
    InstalledMetadata,
}

pub(super) fn sync_lab_offload_rigs(
    runner_id: &str,
    command_path: &str,
    remote_cwd: &str,
    args: &[String],
    primary: LabOffloadPrimaryRigSource<'_>,
) -> Result<Vec<LabOffloadRigSync>> {
    let rig_ids = lab_offload_rig_ids(args);
    if rig_ids.is_empty() {
        return Ok(Vec::new());
    }

    let primary_rig_ids = primary_source_rig_ids(primary.local_path)?;
    let mut synced_rigs = Vec::new();
    for rig_id in &rig_ids {
        let (source, source_kind, package_source, workload_hashes, source_snapshot) =
            if primary_rig_ids.contains(rig_id) {
                (
                    primary.remote_path.to_string(),
                    LabOffloadRigSyncSource::PrimarySnapshot,
                    LabOffloadRigPackageSource {
                        source: primary.local_path.to_string(),
                        source_root: primary.local_path.to_string(),
                        package_path: primary.local_path.to_string(),
                        install_source: primary.remote_path.to_string(),
                        rig_path: None,
                        discovery_path: None,
                        source_revision: primary.source_snapshot.git_sha.clone(),
                        linked: true,
                        materialized: false,
                    },
                    LabOffloadRigWorkloadHashes {
                        source_snapshot_hash: primary.source_snapshot.snapshot_hash.clone(),
                        workspace_snapshot_identity: primary
                            .workspace_snapshot_identity
                            .to_string(),
                    },
                    primary.source_snapshot.clone(),
                )
            } else {
                let metadata = rig::read_source_metadata(rig_id).ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "rig",
                        format!(
                            "runner dispatch cannot materialize rig `{rig_id}` because it has no installed source metadata"
                        ),
                        Some(rig_id.clone()),
                        Some(vec![
                            format!("Reinstall rig `{rig_id}` from a rig package before using --runner."),
                            "Run the rig sources command to inspect installed rig sources.".to_string(),
                        ]),
                    )
                })?;
                let source_root = metadata
                    .source_root
                    .clone()
                    .unwrap_or_else(|| metadata.package_path.clone());
                validate_installed_rig_source(rig_id, &source_root, &metadata.package_path)?;
                let synced = sync_workspace(
                    runner_id,
                    RunnerWorkspaceSyncOptions {
                        path: source_root.clone(),
                        mode: RunnerWorkspaceSyncMode::Snapshot,
                        controller_routed_git: false,
                        changed_since_base: None,
                        git_fetch_refs: Vec::new(),
                        snapshot_includes: Vec::new(),
                        allow_dirty_lab_workspace: false,
                        run_isolation_token: None,
                    },
                )?
                .0;
                let install_source =
                    remote_package_path(&source_root, &metadata.package_path, &synced.remote_path);
                let source_snapshot = SourceSnapshot::collect_local(
                    runner_id,
                    Path::new(&synced.local_path),
                    Some(&synced.remote_path),
                    "lab_rig_source",
                );
                (
                    install_source.clone(),
                    LabOffloadRigSyncSource::InstalledMetadata,
                    LabOffloadRigPackageSource {
                        source: metadata.source,
                        source_root,
                        package_path: metadata.package_path,
                        install_source,
                        rig_path: Some(metadata.rig_path),
                        discovery_path: metadata.discovery_path,
                        source_revision: metadata.source_revision,
                        linked: metadata.linked,
                        materialized: metadata.materialized,
                    },
                    LabOffloadRigWorkloadHashes {
                        source_snapshot_hash: source_snapshot.snapshot_hash.clone(),
                        workspace_snapshot_identity: synced.snapshot_identity,
                    },
                    source_snapshot,
                )
            };

        let removed_source =
            remove_runner_installed_rig_source(runner_id, command_path, remote_cwd, rig_id)?;

        let install_command = vec![
            command_path.to_string(),
            "rig".to_string(),
            "install".to_string(),
            source.clone(),
            "--id".to_string(),
            rig_id.clone(),
        ];

        // Path-translation preflight: the rig install source has already been
        // resolved to a runner-side remote path (primary snapshot remote path or
        // a materialized installed-source path). Assert that no controller-local
        // primary source path survived un-translated into the dispatched argv
        // before we hand it to the remote runner, so a missed remap fails loudly
        // here instead of installing from a non-existent local path (#5285).
        preflight_remote_argv_path_translation(
            "Runner rig materialization",
            runner_id,
            &install_command,
            Path::new(primary.local_path),
            remote_cwd,
        )?;

        let (output, exit_code) = exec(
            runner_id,
            RunnerExecOptions {
                cwd: Some(remote_cwd.to_string()),
                project_id: None,
                allow_diagnostic_ssh: false,
                command: install_command,
                env: HashMap::new(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                // Validate remote capability parity before dispatch: the install
                // is executed by the runner-side `homeboy` binary, so require
                // that tool on the runner. `exec` no-ops this gate for local and
                // already-capable SSH runners, so behavior is unchanged on a
                // correctly provisioned runner and fails early otherwise (#5285).
                capability_preflight: Some(rig_install_capability_preflight()),
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
            },
        )?;

        if exit_code != 0 {
            return Err(Error::validation_invalid_argument(
                "rig",
                format!("runner dispatch could not install rig `{rig_id}` on runner `{runner_id}`"),
                Some(rig_id.clone()),
                Some(vec![
                    output.stderr.trim().to_string(),
                    "Run the command with --force-hot to execute locally while investigating runner rig setup.".to_string(),
                    format!("Lab source snapshot remote path: {}", primary.remote_path),
                    format!("Selected rig install source: {source}"),
                ]),
            ));
        }

        let remote_installed_path = installed_rig_path_from_stdout(&output.stdout, rig_id);

        synced_rigs.push(LabOffloadRigSync {
            rig_id: rig_id.clone(),
            source,
            source_kind,
            package_source,
            workload_hashes,
            source_snapshot,
            remote_installed_path,
            removed_source,
        });
    }

    Ok(synced_rigs)
}

fn installed_rig_path_from_stdout(stdout: &str, rig_id: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    value
        .get("data")
        .unwrap_or(&value)
        .get("installed")?
        .as_array()?
        .iter()
        .find(|installed| installed.get("id").and_then(|value| value.as_str()) == Some(rig_id))
        .and_then(|installed| installed.get("path"))
        .and_then(|path| path.as_str())
        .filter(|path| !path.trim().is_empty())
        .map(str::to_string)
}

pub(super) fn remap_bench_rig_default_component_to_primary_snapshot(
    args: &[String],
    primary_remote_path: &str,
) -> Vec<String> {
    if !is_bench_rig_run(args) || has_path_arg(args) {
        return args.to_vec();
    }

    let mut out = Vec::with_capacity(args.len() + 2);
    let mut inserted = false;
    let mut passthrough = false;
    for arg in args {
        if !inserted && !passthrough && arg == "--" {
            out.push("--path".to_string());
            out.push(primary_remote_path.to_string());
            inserted = true;
        }
        if arg == "--" {
            passthrough = true;
        }
        out.push(arg.clone());
    }
    if !inserted {
        out.push("--path".to_string());
        out.push(primary_remote_path.to_string());
    }
    out
}

fn primary_source_rig_ids(primary_local_path: &str) -> Result<HashSet<String>> {
    let path = Path::new(primary_local_path);
    if !path.join("rig.json").is_file() && !path.join("rigs").is_dir() {
        return Ok(HashSet::new());
    }
    Ok(rig::discover_rigs(path)?
        .into_iter()
        .map(|discovered| discovered.id)
        .collect())
}

fn is_bench_rig_run(args: &[String]) -> bool {
    matches!(args.get(1).map(String::as_str), Some("bench"))
        && !lab_offload_rig_ids(args).is_empty()
}

fn has_path_arg(args: &[String]) -> bool {
    let iter = args.iter().skip(1);
    for arg in iter {
        if arg == "--" {
            return false;
        }
        if arg == "--path" {
            return true;
        }
        if arg.starts_with("--path=") {
            return true;
        }
    }
    false
}

/// Result of materializing a rig's component dependencies on the runner.
pub(super) struct LabOffloadRigComponentSync {
    /// Per-dependency materialization outputs (remote paths, freshness, etc.).
    pub materializations: Vec<RunnerGitDependencyMaterializationOutput>,
    /// Generic `${components.<id>.path}` override env vars mapping each rig
    /// component to its runner-side materialized path, so checks that execute
    /// on the runner resolve component paths to the runner workspace instead of
    /// the controller path the rig spec declares.
    pub component_path_env: Vec<(String, String)>,
}

pub(super) fn sync_lab_offload_rig_component_dependencies(
    runner_id: &str,
    args: &[String],
    primary_local_path: &str,
    primary_remote_path: &str,
    runner_workspace_root: Option<&str>,
    allow_dirty_lab_workspace: bool,
) -> Result<LabOffloadRigComponentSync> {
    let dependencies = lab_offload_rig_component_dependencies(
        args,
        Some((primary_local_path, primary_remote_path)),
        runner_workspace_root,
    )?;
    if dependencies.is_empty() {
        return Ok(LabOffloadRigComponentSync {
            materializations: Vec::new(),
            component_path_env: Vec::new(),
        });
    }

    let runner = load(runner_id)?;
    let mut synced = Vec::new();
    let mut component_path_env = Vec::new();
    let mut seen = HashSet::new();
    for dependency in dependencies {
        // Always record the runner-side effective component path so a remote
        // rig check resolves `${components.<id>.path}` to the materialized path,
        // even when the checkout root is the already-synced primary workspace
        // (which is not re-materialized below).
        component_path_env.push((
            crate::core::rig::expand::rig_component_path_override_env_name(
                &dependency.rig_id,
                &dependency.component_id,
            ),
            remote_component_path(
                &dependency.remote_checkout_root,
                dependency.required_subpath.as_deref(),
            ),
        ));

        if !should_materialize_dependency(&dependency, primary_remote_path) {
            continue;
        }
        if !seen.insert(dependency.remote_checkout_root.clone()) {
            continue;
        }
        prepare_rig_component_dependency(&dependency)?;
        synced.push(materialize_git_dependency(
            &runner,
            RunnerGitDependencyMaterializationOptions {
                local_path: dependency.local_checkout_root,
                remote_path: dependency.remote_checkout_root,
                remote_url: dependency.remote_url,
                required_subpath: dependency.required_subpath,
                pinned_ref: dependency.pinned_ref,
                allow_dirty: allow_dirty_lab_workspace,
            },
        )?);
    }

    Ok(LabOffloadRigComponentSync {
        materializations: synced,
        component_path_env,
    })
}

fn prepare_rig_component_dependency(dependency: &RigComponentDependency) -> Result<()> {
    let path = Path::new(&dependency.local_checkout_root);
    let component = Component {
        id: dependency.component_id.clone(),
        local_path: dependency.local_checkout_root.clone(),
        remote_url: dependency.remote_url.clone(),
        ..Component::default()
    };
    let providers = match deps_provider::resolve_dependency_providers(&component, path) {
        Ok(providers) => providers,
        Err(_) => return Ok(()),
    };

    for provider in providers {
        provider.install(&component, path)?;
    }
    Ok(())
}

/// Join a runner-side checkout root with the component's required subpath.
fn remote_component_path(remote_checkout_root: &str, required_subpath: Option<&str>) -> String {
    match required_subpath.filter(|value| !value.trim().is_empty()) {
        Some(subpath) => Path::new(remote_checkout_root)
            .join(subpath)
            .display()
            .to_string(),
        None => remote_checkout_root.to_string(),
    }
}

pub(super) fn lab_offload_rig_component_checkout_root(args: &[String]) -> Result<Option<PathBuf>> {
    let Some(path_override) = component_path_override(args) else {
        return Ok(None);
    };
    let rig_ids = lab_offload_rig_ids(args);
    if rig_ids.len() != 1 {
        return Ok(None);
    }
    let spec = rig::load(&rig_ids[0])?;
    if spec.components.len() != 1 {
        return Ok(None);
    }
    let Some((component_id, component)) = spec.components.iter().next() else {
        return Ok(None);
    };
    let declared_checkout_root = component
        .checkout_root
        .as_deref()
        .unwrap_or(component.path.as_str());
    let declared_required_subpath = required_component_subpath(
        Path::new(&expanded_local_path(&spec, declared_checkout_root)),
        Path::new(&expanded_local_path(&spec, &component.path)),
        &rig_ids[0],
        component_id,
    )?;
    Ok(Some(PathBuf::from(
        checkout_root_for_component_path_override(
            &expanded_local_path(&spec, &path_override),
            declared_required_subpath.as_deref(),
        ),
    )))
}

pub(super) fn lab_offload_rig_component_dependencies(
    args: &[String],
    primary_workspace: Option<(&str, &str)>,
    runner_workspace_root: Option<&str>,
) -> Result<Vec<RigComponentDependency>> {
    let mut dependencies = Vec::new();
    let component_path_override = component_path_override(args);
    for rig_id in lab_offload_rig_ids(args) {
        let spec = rig::load(&rig_id)?;
        let single_component = spec.components.len() == 1;
        for (component_id, component) in &spec.components {
            let declared_checkout_root = component
                .checkout_root
                .as_deref()
                .unwrap_or(component.path.as_str());
            let declared_local_checkout_root = expanded_local_path(&spec, declared_checkout_root);
            let declared_local_component_path = expanded_local_path(&spec, &component.path);
            let declared_required_subpath = required_component_subpath(
                Path::new(&declared_local_checkout_root),
                Path::new(&declared_local_component_path),
                &rig_id,
                component_id,
            )?;
            let (checkout_root, local_checkout_root, local_component_path) = if single_component {
                if let Some(path_override) = component_path_override.as_deref() {
                    let local_component_path = expanded_local_path(&spec, path_override);
                    let local_checkout_root = checkout_root_for_component_path_override(
                        &local_component_path,
                        declared_required_subpath.as_deref(),
                    );
                    (
                        local_checkout_root.clone(),
                        local_checkout_root,
                        local_component_path,
                    )
                } else {
                    (
                        declared_checkout_root.to_string(),
                        declared_local_checkout_root,
                        declared_local_component_path,
                    )
                }
            } else {
                (
                    declared_checkout_root.to_string(),
                    declared_local_checkout_root,
                    declared_local_component_path,
                )
            };
            let required_subpath = required_component_subpath(
                Path::new(&local_checkout_root),
                Path::new(&local_component_path),
                &rig_id,
                component_id,
            )?;
            dependencies.push(RigComponentDependency {
                rig_id: rig_id.clone(),
                component_id: component_id.clone(),
                remote_checkout_root: remote_checkout_root_for_local(
                    &checkout_root,
                    &local_checkout_root,
                    primary_workspace,
                    runner_workspace_root,
                ),
                local_checkout_root,
                declared_checkout_root: checkout_root.to_string(),
                required_subpath,
                remote_url: component.remote_url.clone(),
                pinned_ref: component.r#ref.clone(),
            });
        }
    }
    Ok(dependencies)
}

fn component_path_override(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            return None;
        }
        if arg == "--path" {
            return iter.next().cloned();
        }
        if let Some(path) = arg.strip_prefix("--path=") {
            return Some(path.to_string());
        }
    }
    None
}

fn checkout_root_for_component_path_override(
    local_component_path: &str,
    required_subpath: Option<&str>,
) -> String {
    let mut checkout_root = PathBuf::from(local_component_path);
    if let Some(required_subpath) = required_subpath {
        for _ in Path::new(required_subpath).components() {
            checkout_root.pop();
        }
    }
    checkout_root.display().to_string()
}

fn expanded_local_path(spec: &rig::RigSpec, value: &str) -> String {
    rig::expand::expand_vars(spec, value)
}

fn remote_checkout_root_for_local(
    declared_checkout_root: &str,
    local_checkout_root: &str,
    primary_workspace: Option<(&str, &str)>,
    runner_workspace_root: Option<&str>,
) -> String {
    let Some((primary_local_path, primary_remote_path)) = primary_workspace else {
        return local_checkout_root.to_string();
    };
    if normalize_path_for_prefix(Path::new(local_checkout_root))
        == normalize_path_for_prefix(Path::new(primary_local_path))
    {
        return primary_remote_path.to_string();
    }
    // A declared `~/...` checkout root is portable: it should land at the same
    // home-relative location on the runner. Expand `~` against the runner home
    // (derived from the runner workspace root) so the materialized remote path
    // is a real absolute path instead of a literal `~` subdirectory.
    if let Some(portable) =
        expand_portable_runner_path(declared_checkout_root, runner_workspace_root)
    {
        return portable;
    }
    let name = Path::new(local_checkout_root)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("dependency");
    format!(
        "{}/{}",
        parent_remote_path(primary_remote_path),
        sanitize_path_segment(name)
    )
}

fn is_portable_runner_path(path: &str) -> bool {
    path == "~" || path.starts_with("~/")
}

/// Resolve a portable `~`/`~/...` checkout root to an absolute runner path.
///
/// Returns `None` for non-portable paths so callers fall back to deriving a
/// materialized `_lab_workspaces` path. When the runner home cannot be derived
/// the portable tail is left unresolved (still `None`) so we never emit a
/// literal `~` directory on the runner.
fn expand_portable_runner_path(path: &str, runner_workspace_root: Option<&str>) -> Option<String> {
    if !is_portable_runner_path(path) {
        return None;
    }
    let runner_home = runner_home_from_workspace_root(runner_workspace_root?)?;
    let tail = path.strip_prefix('~').unwrap_or(path);
    let tail = tail.strip_prefix('/').unwrap_or(tail);
    if tail.is_empty() {
        return Some(runner_home);
    }
    Some(format!("{}/{}", runner_home.trim_end_matches('/'), tail))
}

/// Derive the runner home directory from the runner workspace root.
///
/// Runner workspace roots are absolute (validated elsewhere). The runner home
/// is the deepest ancestor that looks like a user home root (`/home/<user>` or
/// `/Users/<user>`), falling back to the workspace root's parent. This stays
/// generic: it makes no assumption about any specific runtime or component.
fn runner_home_from_workspace_root(workspace_root: &str) -> Option<String> {
    let root = Path::new(workspace_root);
    if !root.is_absolute() {
        return None;
    }
    let mut components: Vec<&std::ffi::OsStr> = Vec::new();
    for component in root.components() {
        if let std::path::Component::Normal(value) = component {
            components.push(value);
        }
    }
    // Match the common `/home/<user>` and `/Users/<user>` home roots when the
    // workspace root nests beneath them, so `~` resolves to the user home even
    // when the workspace root is e.g. `/home/<user>/Developer`.
    for marker in ["home", "Users"] {
        if let Some(position) = components.iter().position(|value| {
            value
                .to_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(marker))
        }) {
            if position + 1 < components.len() {
                let user = components[position + 1].to_str()?;
                return Some(format!("/{}/{}", components[position].to_str()?, user));
            }
        }
    }
    // Fall back to the workspace root's parent directory.
    root.parent()
        .map(|parent| parent.display().to_string())
        .filter(|parent| !parent.is_empty())
}

fn should_materialize_dependency(
    dependency: &RigComponentDependency,
    primary_remote_path: &str,
) -> bool {
    dependency.remote_checkout_root != primary_remote_path
}

fn required_component_subpath(
    checkout_root: &Path,
    component_path: &Path,
    rig_id: &str,
    component_id: &str,
) -> Result<Option<String>> {
    let checkout_root = normalize_path_for_prefix(checkout_root);
    let component_path = normalize_path_for_prefix(component_path);
    if checkout_root == component_path {
        return Ok(None);
    }
    let subpath = component_path.strip_prefix(&checkout_root).map_err(|_| {
        Error::validation_invalid_argument(
            "checkout_root",
            format!(
                "rig `{rig_id}` component `{component_id}` declares checkout_root outside its component path"
            ),
            Some(checkout_root.display().to_string()),
            Some(vec![format!(
                "Set checkout_root to the repository root that contains {}.",
                component_path.display()
            )]),
        )
    })?;
    Ok(Some(subpath.display().to_string()))
}

fn normalize_path_for_prefix(path: &Path) -> PathBuf {
    path.components().collect()
}

fn lab_offload_rig_ids(args: &[String]) -> Vec<String> {
    let mut rig_ids = Vec::new();

    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        let raw = if arg == "--rig" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--rig=")
        };
        if let Some(raw) = raw {
            for rig_id in raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                push_unique(&mut rig_ids, rig_id.to_string());
            }
        }
    }

    rig_ids
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_unique_bench_rig_ids_for_lab_materialization() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "baseline,candidate".to_string(),
            "--scenario".to_string(),
            "smoke".to_string(),
            "--rig=candidate".to_string(),
        ];

        assert_eq!(
            lab_offload_rig_ids(&args),
            vec!["baseline".to_string(), "candidate".to_string()]
        );
    }

    #[test]
    fn extracts_trace_rig_ids_and_ignores_passthrough_args_for_lab_materialization() {
        let trace = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--rig".to_string(),
            "candidate".to_string(),
        ];
        assert_eq!(lab_offload_rig_ids(&trace), vec!["candidate".to_string()]);

        let passthrough = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--".to_string(),
            "--rig".to_string(),
            "candidate".to_string(),
        ];
        assert!(lab_offload_rig_ids(&passthrough).is_empty());
    }

    #[test]
    fn collects_rig_component_dependency_checkout_roots() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("Developer/woocommerce");
            std::fs::create_dir_all(checkout.join("plugins/woocommerce")).expect("checkout");
            let rig_dir = crate::core::paths::rigs().expect("rig dir");
            std::fs::create_dir_all(&rig_dir).expect("create rig dir");
            std::fs::write(
                rig_dir.join("woocommerce-performance.json"),
                serde_json::json!({
                    "id": "woocommerce-performance",
                    "components": {
                        "woocommerce": {
                            "path": format!("{}/plugins/woocommerce", checkout.display()),
                            "checkout_root": checkout.display().to_string(),
                            "remote_url": "https://github.com/woocommerce/woocommerce.git"
                        }
                    }
                })
                .to_string(),
            )
            .expect("save rig");

            let dependencies = lab_offload_rig_component_dependencies(
                &[
                    "homeboy".to_string(),
                    "bench".to_string(),
                    "--rig".to_string(),
                    "woocommerce-performance".to_string(),
                ],
                None,
                None,
            )
            .expect("dependencies");

            assert_eq!(dependencies.len(), 1);
            assert_eq!(dependencies[0].component_id, "woocommerce");
            assert_eq!(
                dependencies[0].local_checkout_root,
                checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].declared_checkout_root,
                checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].remote_checkout_root,
                checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].required_subpath.as_deref(),
                Some("plugins/woocommerce")
            );
        });
    }

    #[test]
    fn bench_path_override_updates_single_component_dependency_checkout_root() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("Developer/example-repo");
            let override_checkout = home.path().join("Developer/example-repo@bench-evidence");
            let override_component = override_checkout.join("packages/example-component");
            std::fs::create_dir_all(checkout.join("packages/example-component")).expect("checkout");
            std::fs::create_dir_all(&override_component).expect("override checkout");
            let rig_dir = crate::core::paths::rigs().expect("rig dir");
            std::fs::create_dir_all(&rig_dir).expect("create rig dir");
            std::fs::write(
                rig_dir.join("example-monorepo.json"),
                serde_json::json!({
                    "id": "example-monorepo",
                    "components": {
                        "example-component": {
                            "path": format!("{}/packages/example-component", checkout.display()),
                            "checkout_root": checkout.display().to_string(),
                            "remote_url": "https://example.test/example/repo.git"
                        }
                    }
                })
                .to_string(),
            )
            .expect("save rig");

            let dependencies = lab_offload_rig_component_dependencies(
                &[
                    "homeboy".to_string(),
                    "bench".to_string(),
                    "--rig".to_string(),
                    "example-monorepo".to_string(),
                    "--path".to_string(),
                    override_component.display().to_string(),
                ],
                None,
                None,
            )
            .expect("dependencies");

            assert_eq!(dependencies.len(), 1);
            assert_eq!(dependencies[0].component_id, "example-component");
            assert_eq!(
                dependencies[0].local_checkout_root,
                override_checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].declared_checkout_root,
                override_checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].remote_checkout_root,
                override_checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].required_subpath.as_deref(),
                Some("packages/example-component")
            );
        });
    }

    #[test]
    fn bench_path_override_derives_lab_source_checkout_root() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("Developer/example-repo");
            let override_checkout = home.path().join("Developer/example-repo@bench-evidence");
            let override_component = override_checkout.join("packages/example-component");
            std::fs::create_dir_all(checkout.join("packages/example-component")).expect("checkout");
            std::fs::create_dir_all(&override_component).expect("override checkout");
            let rig_dir = crate::core::paths::rigs().expect("rig dir");
            std::fs::create_dir_all(&rig_dir).expect("create rig dir");
            std::fs::write(
                rig_dir.join("example-monorepo.json"),
                serde_json::json!({
                    "id": "example-monorepo",
                    "components": {
                        "example-component": {
                            "path": format!("{}/packages/example-component", checkout.display()),
                            "checkout_root": checkout.display().to_string()
                        }
                    }
                })
                .to_string(),
            )
            .expect("save rig");

            let source_path = lab_offload_rig_component_checkout_root(&[
                "homeboy".to_string(),
                "bench".to_string(),
                "--rig".to_string(),
                "example-monorepo".to_string(),
                "--path".to_string(),
                override_component.display().to_string(),
            ])
            .expect("source path")
            .expect("rig source path");

            assert_eq!(source_path, override_checkout);
        });
    }

    #[test]
    fn expands_package_root_for_remote_component_dependency_root() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("Developer/studio-web");
            std::fs::create_dir_all(checkout.join("rigs/studio-web-product-matrix"))
                .expect("rig package");
            let rig_dir = crate::core::paths::rigs().expect("rig dir");
            std::fs::create_dir_all(&rig_dir).expect("create rig dir");
            std::fs::write(
                rig_dir.join("studio-web-product-matrix.json"),
                serde_json::json!({
                    "id": "studio-web-product-matrix",
                    "components": {
                        "studio-web": {
                            "path": "${package.root}",
                            "remote_url": "https://github.example.com/example-org/studio-web.git"
                        }
                    }
                })
                .to_string(),
            )
            .expect("save rig");
            std::fs::create_dir_all(crate::core::paths::rig_sources().expect("rig sources"))
                .expect("create rig sources");
            crate::core::rig::install::write_source_metadata(
                "studio-web-product-matrix",
                &crate::core::rig::install::RigSourceMetadata {
                    source: checkout.display().to_string(),
                    source_root: Some(checkout.display().to_string()),
                    package_path: checkout.display().to_string(),
                    rig_path: checkout
                        .join("rigs/studio-web-product-matrix/rig.json")
                        .display()
                        .to_string(),
                    discovery_path: Some(checkout.display().to_string()),
                    source_revision: None,
                    linked: true,
                    materialized: false,
                },
            )
            .expect("source metadata");

            let dependencies = lab_offload_rig_component_dependencies(
                &[
                    "homeboy".to_string(),
                    "bench".to_string(),
                    "--rig".to_string(),
                    "studio-web-product-matrix".to_string(),
                ],
                Some((
                    &checkout.display().to_string(),
                    "/home/user/Developer/_lab_workspaces/studio-web-snapshot",
                )),
                None,
            )
            .expect("dependencies");

            assert_eq!(dependencies.len(), 1);
            assert_eq!(
                dependencies[0].local_checkout_root,
                checkout.display().to_string()
            );
            assert_eq!(
                dependencies[0].remote_checkout_root,
                "/home/user/Developer/_lab_workspaces/studio-web-snapshot"
            );
            assert!(!dependencies[0]
                .remote_checkout_root
                .contains("${package.root}"));
        });
    }

    #[test]
    fn primary_source_rig_ids_discovers_rigs_from_current_source_tree() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("Developer/studio-web-release-clean");
            let rig_dir = checkout.join("rigs/studio-web-product-matrix");
            std::fs::create_dir_all(&rig_dir).expect("rig dir");
            std::fs::write(
                rig_dir.join("rig.json"),
                serde_json::json!({
                    "id": "studio-web-product-matrix",
                    "components": {},
                    "bench": { "default_component": "studio-web" }
                })
                .to_string(),
            )
            .expect("rig spec");

            let rig_ids =
                primary_source_rig_ids(&checkout.display().to_string()).expect("primary rigs");

            assert!(rig_ids.contains("studio-web-product-matrix"));
        });
    }

    #[test]
    fn bench_rig_default_component_args_receive_primary_snapshot_path() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "studio-web-product-matrix".to_string(),
            "--scenario".to_string(),
            "editable_preview_ready".to_string(),
        ];

        let rewritten = remap_bench_rig_default_component_to_primary_snapshot(
            &args,
            "/home/user/Developer/_lab_workspaces/studio-web-release-clean-abc",
        );

        assert_eq!(
            rewritten,
            vec![
                "homeboy",
                "bench",
                "--rig",
                "studio-web-product-matrix",
                "--scenario",
                "editable_preview_ready",
                "--path",
                "/home/user/Developer/_lab_workspaces/studio-web-release-clean-abc",
            ]
        );
    }

    #[test]
    fn bench_rig_path_injection_preserves_passthrough_boundary() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig=studio-web-product-matrix".to_string(),
            "--".to_string(),
            "--runner-owned".to_string(),
        ];

        let rewritten = remap_bench_rig_default_component_to_primary_snapshot(
            &args,
            "/home/user/Developer/_lab_workspaces/studio-web-release-clean-abc",
        );

        assert_eq!(
            rewritten,
            vec![
                "homeboy",
                "bench",
                "--rig=studio-web-product-matrix",
                "--path",
                "/home/user/Developer/_lab_workspaces/studio-web-release-clean-abc",
                "--",
                "--runner-owned",
            ]
        );
    }

    #[test]
    fn bench_rig_path_injection_keeps_explicit_path() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "studio-web-product-matrix".to_string(),
            "--path".to_string(),
            "/custom/source".to_string(),
        ];

        assert_eq!(
            remap_bench_rig_default_component_to_primary_snapshot(&args, "/snapshot"),
            args
        );
    }

    #[test]
    fn installed_rig_path_from_stdout_reads_success_envelope() {
        let stdout = serde_json::json!({
            "success": true,
            "data": {
                "command": "rig.install",
                "installed": [
                    {
                        "id": "other-rig",
                        "path": "/home/user/.config/homeboy/rigs/other.json"
                    },
                    {
                        "id": "target-rig",
                        "path": "/home/user/.config/homeboy/rigs/target.json"
                    }
                ]
            }
        })
        .to_string();

        assert_eq!(
            installed_rig_path_from_stdout(&stdout, "target-rig").as_deref(),
            Some("/home/user/.config/homeboy/rigs/target.json")
        );
    }

    #[test]
    fn installed_rig_path_from_stdout_handles_plain_output() {
        let stdout = serde_json::json!({
            "command": "rig.install",
            "installed": [
                {
                    "id": "target-rig",
                    "path": "/home/user/.config/homeboy/rigs/target.json"
                }
            ]
        })
        .to_string();

        assert_eq!(
            installed_rig_path_from_stdout(&stdout, "target-rig").as_deref(),
            Some("/home/user/.config/homeboy/rigs/target.json")
        );
    }

    #[test]
    fn maps_non_primary_rig_dependency_to_runner_workspace_parent() {
        let remote = remote_checkout_root_for_local(
            "/Users/user/Developer/studio@fix-many-sites-memory",
            "/Users/user/Developer/studio@fix-many-sites-memory",
            Some((
                "/Users/user/Developer/homeboy-rigs/example-org/studio",
                "/home/user/Developer/_lab_workspaces/studio-rigs-snapshot",
            )),
            Some("/home/user/Developer"),
        );

        assert_eq!(
            remote,
            "/home/user/Developer/_lab_workspaces/studio-fix-many-sites-memory"
        );
        assert!(!remote.contains("/Users/"));
    }

    #[test]
    fn portable_declared_rig_dependency_path_expands_to_runner_home() {
        // Regression for #3767: a portable `~/...` declared checkout root must
        // resolve to an absolute runner path (under the runner home derived from
        // the workspace root), never a literal `~` subdirectory.
        let remote = remote_checkout_root_for_local(
            "~/Developer/studio@fix-many-sites-memory",
            "/Users/user/Developer/studio@fix-many-sites-memory",
            Some((
                "/Users/user/Developer/homeboy-rigs/example-org/studio",
                "/home/user/Developer/_lab_workspaces/studio-rigs-snapshot",
            )),
            Some("/home/user/Developer"),
        );

        assert_eq!(remote, "/home/user/Developer/studio@fix-many-sites-memory");
        assert!(!remote.contains('~'));
    }

    #[test]
    fn portable_rig_dependency_without_runner_home_falls_back_to_materialized_path() {
        // When the runner home cannot be derived we must still avoid emitting a
        // literal `~`; fall back to the materialized `_lab_workspaces` path.
        let remote = remote_checkout_root_for_local(
            "~/Developer/studio@fix-many-sites-memory",
            "/Users/user/Developer/studio@fix-many-sites-memory",
            Some((
                "/Users/user/Developer/homeboy-rigs/example-org/studio",
                "/home/user/Developer/_lab_workspaces/studio-rigs-snapshot",
            )),
            None,
        );

        assert!(!remote.contains('~'));
        assert_eq!(
            remote,
            "/home/user/Developer/_lab_workspaces/studio-fix-many-sites-memory"
        );
    }

    #[test]
    fn runner_home_resolves_from_nested_workspace_root() {
        assert_eq!(
            runner_home_from_workspace_root("/home/user/Developer"),
            Some("/home/user".to_string())
        );
        assert_eq!(
            runner_home_from_workspace_root("/Users/user/Developer/lab"),
            Some("/Users/user".to_string())
        );
        // Workspace root that is itself a home falls back to its parent.
        assert_eq!(
            runner_home_from_workspace_root("/srv/homeboy"),
            Some("/srv".to_string())
        );
    }

    #[test]
    fn expand_portable_runner_path_joins_tail_to_runner_home() {
        assert_eq!(
            expand_portable_runner_path("~/Developer/x", Some("/home/user/Developer")),
            Some("/home/user/Developer/x".to_string())
        );
        assert_eq!(
            expand_portable_runner_path("~", Some("/home/user/Developer")),
            Some("/home/user".to_string())
        );
        assert_eq!(
            expand_portable_runner_path("/abs/path", Some("/home/user/Developer")),
            None
        );
    }

    #[test]
    fn primary_workspace_dependency_is_not_materialized_again() {
        let primary_remote_path = "/home/user/Developer/_lab_workspaces/studio-web-snapshot";
        let dependencies = vec![RigComponentDependency {
            rig_id: "studio-web-product-matrix".to_string(),
            component_id: "studio-web".to_string(),
            local_checkout_root: "/Users/user/Developer/studio-web".to_string(),
            declared_checkout_root: "/Users/user/Developer/studio-web".to_string(),
            remote_checkout_root: primary_remote_path.to_string(),
            required_subpath: None,
            remote_url: Some("https://github.example.com/example-org/studio-web.git".to_string()),
            pinned_ref: None,
        }];

        assert!(dependencies
            .into_iter()
            .filter(|dependency| should_materialize_dependency(dependency, primary_remote_path))
            .collect::<Vec<_>>()
            .is_empty());
    }

    #[test]
    fn rejects_checkout_root_outside_component_path() {
        let err = required_component_subpath(
            Path::new("/tmp/wordpress"),
            Path::new("/tmp/woocommerce/plugins/woocommerce"),
            "rig",
            "woocommerce",
        )
        .expect_err("root outside component path");

        assert_eq!(err.details["field"], "checkout_root");
        assert!(err.message.contains("component `woocommerce`"));
    }
}

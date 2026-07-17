use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use homeboy_agents::agent_task_provider;
use homeboy_agents::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::worktree::TaskWorktreeState;
use homeboy_core::{component, worktree, Error, Result};

use super::lab_workspaces_deps::{
    accepted_extra_lab_workspaces, add_candidate_extra_workspace, bare_module_imports,
    bootstrap_source_cli_dependencies, canonical_existing_dir, component_contract_candidate_paths,
    containing_checkout_or_parent, discovered_validation_dependency_workspaces,
    provider_config_candidate_paths, provider_config_source_cli_files,
};
use super::{
    sync_workspace, RunnerGitDependencyMaterializationOutput, RunnerValidationDependencySyncOutput,
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

pub(super) const LAB_EXTRA_WORKSPACES_ENV: &str = concat!("HOME", "BOY_LAB_EXTRA_WORKSPACES");
pub(super) const LAB_EXTRA_WORKSPACES_JSON_ENV: &str =
    concat!("HOME", "BOY_LAB_EXTRA_WORKSPACES_JSON");
pub(super) const LAB_WORKSPACE_MAPPING_SCHEMA: &str = concat!("home", "boy/workspace-map/v1");

/// Config channel for declared runtime overlays. A runtime overlay is a
/// first-class, ecosystem-agnostic contract for materializing a built runtime
/// (e.g. a packaged CLI) on the remote Lab runner without syncing its entire
/// dependency tree: an extra workspace/artifact directory to sync PLUS an
/// optional opaque dependency-install step (a command + working dir, supplied
/// as data) that Homeboy runs on the runner AFTER the sync and BEFORE the hot
/// command. The install command is opaque data — core only forwards it to the
/// runner and never assumes any package manager, language, or tooling.
pub(super) const LAB_RUNTIME_OVERLAYS_JSON_ENV: &str =
    concat!("HOME", "BOY_LAB_RUNTIME_OVERLAYS_JSON");
pub(super) const LAB_RUNTIME_OVERLAY_SCHEMA: &str = concat!("home", "boy/lab-runtime-overlay/v1");

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct LabWorkspaceMappingEntry {
    role: String,
    local_path: String,
    remote_path: String,
    sync_mode: String,
    snapshot_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dependency_freshness: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_provenance: Option<serde_json::Value>,
}

impl LabWorkspaceMappingEntry {
    pub(super) fn role(&self) -> &str {
        &self.role
    }

    pub(super) fn local_path(&self) -> &str {
        &self.local_path
    }

    pub(super) fn remote_path(&self) -> &str {
        &self.remote_path
    }

    pub(super) fn dependency_freshness(&self) -> Option<&serde_json::Value> {
        self.dependency_freshness.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExtraLabWorkspace {
    pub(super) role: String,
    pub(super) path: PathBuf,
    pub(super) snapshot_includes: Vec<String>,
    pub(super) bootstrap_node_dependencies: bool,
    pub(super) bootstrap_command: Option<Vec<String>>,
    pub(super) allow_dirty_lab_workspace: bool,
    pub(super) source_provenance: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspaceRefResolution {
    pub(super) raw_ref: String,
    pub(super) handle: String,
    pub(super) subpath: Option<String>,
    pub(super) source_kind: String,
    pub(super) source_provenance: Option<serde_json::Value>,
    pub(super) workspace_path: PathBuf,
    pub(super) resolved_path: PathBuf,
}

/// An opaque dependency-install step declared by a runtime overlay. The
/// `command` is an argv vector supplied as data by the extension/config; core
/// runs it on the remote runner verbatim and never inspects or hardcodes any
/// ecosystem tooling. `workdir` selects which synced directory the install runs
/// in: `None` runs in the overlay's own synced remote path, while a relative
/// value resolves against that remote path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(super) struct RuntimeOverlayInstallStep {
    pub(super) command: Vec<String>,
    #[serde(default)]
    pub(super) workdir: Option<String>,
}

/// Raw, untrusted runtime-overlay declaration as supplied by config/env. Parsed
/// into a validated [`RuntimeOverlay`] by [`parse_runtime_overlays`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(super) struct RuntimeOverlaySpec {
    /// Controller-local directory holding the built runtime artifact to sync.
    pub(super) path: String,
    /// Logical role label for the synced workspace mapping entry.
    #[serde(default)]
    pub(super) role: Option<String>,
    /// Relative subpaths included when snapshotting the artifact directory.
    #[serde(default)]
    pub(super) snapshot_includes: Vec<String>,
    /// Optional opaque dependency-install step run on the runner after sync.
    #[serde(default)]
    pub(super) install: Option<RuntimeOverlayInstallStep>,
    /// Optional environment variable name to surface the overlay's resolved
    /// remote path to the hot command (e.g. so a CLI command env entry points at
    /// the real remote runtime directory). The name is opaque config data.
    #[serde(default)]
    pub(super) expose_remote_path_env: Option<String>,
}

/// A validated runtime overlay: an extra workspace to sync plus an optional
/// opaque install step and an optional env var that surfaces the resolved
/// remote path to the hot command. Kept ecosystem-agnostic — every behavioral
/// value here originates from config/extension data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeOverlay {
    pub(super) workspace: ExtraLabWorkspace,
    pub(super) install: Option<RuntimeOverlayInstallStep>,
    pub(super) expose_remote_path_env: Option<String>,
}

/// A runtime overlay that has been synced to the runner and (if declared) had
/// its opaque install step executed. Records the resolved remote path and the
/// env var surfacing it, so callers can fold the overlay into the command env
/// and offload metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SyncedRuntimeOverlay {
    pub(super) role: String,
    pub(super) local_path: String,
    pub(super) remote_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) install_workdir: Option<String>,
    pub(super) install_ran: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) expose_remote_path_env: Option<String>,
    /// Build-freshness provenance for the synced artifact: the source SHA the
    /// built dist reflects vs the current source checkout HEAD, so a stale build
    /// is auditable from `homeboy runs` (#6965).
    pub(super) build_provenance: super::runtime_overlay_freshness::RuntimeOverlayBuildProvenance,
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
                allow_dirty_lab_workspace: extra.allow_dirty_lab_workspace,
                run_isolation_token: None,
            },
        )?
        .0;
        let mut entry = workspace_mapping_entry(&extra.role, &synced);
        entry.source_provenance = extra.source_provenance.clone();
        if let Some(command) = &extra.bootstrap_command {
            bootstrap_source_cli_dependencies(
                runner_id,
                command,
                &synced.local_path,
                &synced.remote_path,
            )?;
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
        source_provenance: None,
    }
}

/// Build a workspace-mapping entry for a declared dependency checkout that the
/// primary workspace sync already materialized on the runner (as a sibling of
/// the primary remote path). Folding these into the offload workspace mapping
/// propagates their local->remote path pairs into the remote command's path
/// remaps, so an extension dependency resolver running on the runner receives
/// usable remote paths instead of controller-local ones. Kept generic: the
/// dependency graph is an opaque id->path mapping with no ecosystem semantics.
pub(super) fn workspace_mapping_entry_for_validation_dependency(
    dependency: &RunnerValidationDependencySyncOutput,
) -> LabWorkspaceMappingEntry {
    LabWorkspaceMappingEntry {
        role: dependency.role.clone(),
        local_path: dependency.local_path.clone(),
        remote_path: dependency.remote_path.clone(),
        sync_mode: RunnerWorkspaceSyncMode::Snapshot.label().to_string(),
        snapshot_identity: dependency.id.clone(),
        dependency_freshness: Some(serde_json::json!({
            "id": dependency.id.as_str(),
            "local_path": dependency.local_path.as_str(),
            "evidence_path": dependency.evidence_path.as_str(),
            "source_provenance": "validation_dependency_sibling",
        })),
        source_provenance: None,
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
            "dirty_overlay": dependency.dirty_overlay,
            "source_provenance": if dependency.dirty_overlay {
                "dirty_snapshot"
            } else {
                "clean_git"
            },
        })),
        source_provenance: None,
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
            source_provenance: None,
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

/// Read the declared runtime overlays for this Lab offload from the config/env
/// channel. Returns an empty list when no overlays are declared, leaving the
/// existing single-checkout / no-overlay offload behavior unchanged.
pub(super) fn lab_runtime_overlays() -> Result<Vec<RuntimeOverlay>> {
    let raw = match std::env::var(LAB_RUNTIME_OVERLAYS_JSON_ENV) {
        Ok(raw) if !raw.trim().is_empty() => raw,
        _ => return Ok(Vec::new()),
    };
    let specs: Vec<RuntimeOverlaySpec> = serde_json::from_str(&raw).map_err(|err| {
        Error::validation_invalid_argument(
            LAB_RUNTIME_OVERLAYS_JSON_ENV,
            format!(
                "{LAB_RUNTIME_OVERLAYS_JSON_ENV} must be a JSON array of runtime-overlay objects: {err}"
            ),
            Some(raw.clone()),
            Some(vec![
                "Each overlay is `{\"path\": <artifact dir>, \"install\": {\"command\": [..], \"workdir\": <relative>}, \"expose_remote_path_env\": <ENV>}`. install and expose_remote_path_env are optional.".to_string(),
            ]),
        )
    })?;
    parse_runtime_overlays(specs)
}

/// Validate raw runtime-overlay specs into [`RuntimeOverlay`] values. The
/// artifact directory must exist; the install command (when present) must be a
/// non-empty argv. Pure and side-effect-free apart from canonicalizing the
/// artifact path, so it is unit-testable without a runner.
pub(super) fn parse_runtime_overlays(
    specs: Vec<RuntimeOverlaySpec>,
) -> Result<Vec<RuntimeOverlay>> {
    let mut overlays = Vec::with_capacity(specs.len());
    for spec in specs {
        let role = spec
            .role
            .filter(|role| !role.trim().is_empty())
            .unwrap_or_else(|| "runtime_overlay".to_string());
        let path = canonical_existing_dir(&spec.path, "runtime_overlay.path")?;
        if let Some(install) = spec.install.as_ref() {
            if install.command.iter().all(|arg| arg.trim().is_empty()) {
                return Err(Error::validation_invalid_argument(
                    "runtime_overlay.install.command",
                    "A runtime-overlay install step must declare a non-empty command argv."
                        .to_string(),
                    Some(spec.path.clone()),
                    Some(vec![
                        "Provide the install command as an argv array, e.g. `\"command\": [\"<tool>\", \"install\"]`.".to_string(),
                    ]),
                ));
            }
        }
        let expose_remote_path_env = spec
            .expose_remote_path_env
            .filter(|name| !name.trim().is_empty());
        overlays.push(RuntimeOverlay {
            workspace: ExtraLabWorkspace {
                role,
                path,
                snapshot_includes: spec.snapshot_includes,
                bootstrap_node_dependencies: false,
                bootstrap_command: None,
                allow_dirty_lab_workspace: false,
                source_provenance: None,
            },
            install: spec.install,
            expose_remote_path_env,
        });
    }
    Ok(overlays)
}

/// Resolve the remote working directory for an overlay install step. `None`
/// runs in the overlay's own synced remote path; a relative value resolves
/// against that remote path. Absolute values are honored verbatim so an overlay
/// can install into a sibling materialized directory. Kept pure for testing.
pub(super) fn runtime_overlay_install_workdir(remote_path: &str, workdir: Option<&str>) -> String {
    match workdir.map(str::trim).filter(|value| !value.is_empty()) {
        None => remote_path.to_string(),
        Some(value) if Path::new(value).is_absolute() => value.to_string(),
        Some(value) => Path::new(remote_path).join(value).display().to_string(),
    }
}

/// Sync each declared runtime overlay to the runner and, when an overlay
/// declares an opaque install step, run it on the runner AFTER the sync and
/// BEFORE the hot command. Overlays are processed in declaration order so a
/// later overlay can depend on an earlier one's materialized output. Returns
/// the synced overlays (with resolved remote paths) and folds their workspace
/// mapping entries into `workspace_mapping`. An empty overlay list is a no-op,
/// keeping non-overlay offload unchanged.
pub(super) fn sync_lab_runtime_overlays(
    runner_id: &str,
    primary_local_path: &str,
    overlays: Vec<RuntimeOverlay>,
    workspace_mapping: &mut Vec<LabWorkspaceMappingEntry>,
) -> Result<Vec<SyncedRuntimeOverlay>> {
    if overlays.is_empty() {
        return Ok(Vec::new());
    }
    let primary = canonical_existing_dir(primary_local_path, "path")?;
    let mut seen = HashSet::from([primary]);
    let mut synced_overlays = Vec::new();

    for overlay in overlays {
        let local_path = canonical_existing_dir(
            &overlay.workspace.path.display().to_string(),
            "runtime_overlay",
        )?;
        let already_synced = !seen.insert(local_path.clone());

        // Detect a stale runner-side component build before snapshotting it:
        // compare the source checkout HEAD against the SHA the built artifact
        // reflects (derived from its on-disk build time). A stale dist would
        // silently ship old code to the runner (#6965). Computed on the
        // controller-local artifact dir, where mtimes are authoritative.
        let build_provenance =
            super::runtime_overlay_freshness::assess_runtime_overlay_build_freshness(&local_path);
        if let Some(warning) = super::runtime_overlay_freshness::stale_runtime_overlay_warning(
            &overlay.workspace.role,
            &local_path.display().to_string(),
            &build_provenance,
        ) {
            if super::runtime_overlay_freshness::require_fresh_runtime_overlay() {
                return Err(Error::validation_invalid_argument(
                    "runtime_overlay",
                    warning,
                    Some(local_path.display().to_string()),
                    Some(vec![
                        "Rebuild the overlay artifact (re-run its build step) so the dist reflects the current source HEAD before retrying.".to_string(),
                    ]),
                ));
            }
            eprintln!("{warning}");
        }

        let synced = sync_workspace(
            runner_id,
            RunnerWorkspaceSyncOptions {
                path: local_path.display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: overlay.workspace.snapshot_includes.clone(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )?
        .0;
        let entry = workspace_mapping_entry(&overlay.workspace.role, &synced);
        if !already_synced {
            workspace_mapping.push(entry);
        }

        let mut install_workdir = None;
        let mut install_ran = false;
        if let Some(install) = overlay.install.as_ref() {
            let workdir =
                runtime_overlay_install_workdir(&synced.remote_path, install.workdir.as_deref());
            run_runtime_overlay_install_step(runner_id, &synced.local_path, &workdir, install)?;
            install_workdir = Some(workdir);
            install_ran = true;
        }

        synced_overlays.push(SyncedRuntimeOverlay {
            role: overlay.workspace.role.clone(),
            local_path: synced.local_path.clone(),
            remote_path: synced.remote_path.clone(),
            install_workdir,
            install_ran,
            expose_remote_path_env: overlay.expose_remote_path_env.clone(),
            build_provenance,
        });
    }

    Ok(synced_overlays)
}

/// Run a runtime overlay's opaque install command on the remote runner. The
/// command argv is forwarded verbatim — core asserts only that no
/// controller-local workspace path survived un-translated into the argv before
/// dispatch, then executes it with the resolved remote workdir as cwd. No
/// ecosystem tooling is assumed; the command is data supplied by config.
fn run_runtime_overlay_install_step(
    runner_id: &str,
    local_path: &str,
    remote_workdir: &str,
    install: &RuntimeOverlayInstallStep,
) -> Result<()> {
    let command: Vec<String> = install
        .command
        .iter()
        .map(|arg| arg.trim().to_string())
        .filter(|arg| !arg.is_empty())
        .collect();

    super::lab_workspaces_deps::preflight_runtime_overlay_install_argv(
        runner_id,
        &command,
        Path::new(local_path),
        remote_workdir,
    )?;

    let (output, exit_code) = super::exec(
        runner_id,
        super::RunnerExecOptions::raw_command(command).with_cwd(remote_workdir),
    )?;
    if exit_code == 0 {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "runtime_overlay.install",
        format!(
            "Lab offload runtime-overlay install step failed (exit {exit_code}) in remote workdir `{remote_workdir}`"
        ),
        Some(remote_workdir.to_string()),
        Some(vec![
            format!("install stderr: {}", output.stderr.trim()),
            "Verify the overlay install command succeeds on the runner, or package the runtime as a self-contained artifact so no install step is required.".to_string(),
        ]),
    ))
}

/// Build the env-var deltas that surface synced runtime-overlay remote paths to
/// the hot command. Only overlays that declared `expose_remote_path_env`
/// contribute an entry; the result is empty otherwise. Pure for testing.
pub(super) fn runtime_overlay_env_overrides(
    synced_overlays: &[SyncedRuntimeOverlay],
) -> Vec<(String, String)> {
    synced_overlays
        .iter()
        .filter_map(|overlay| {
            overlay
                .expose_remote_path_env
                .as_ref()
                .map(|name| (name.clone(), overlay.remote_path.clone()))
        })
        .collect()
}

/// Metadata block describing the synced runtime overlays for offload evidence.
pub(super) fn lab_runtime_overlay_metadata(
    synced_overlays: &[SyncedRuntimeOverlay],
) -> serde_json::Value {
    serde_json::json!({
        "schema": LAB_RUNTIME_OVERLAY_SCHEMA,
        "count": synced_overlays.len(),
        "overlays": synced_overlays,
    })
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
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());

    let mut seen = BTreeSet::new();
    let mut workspaces: Vec<ExtraLabWorkspace> = Vec::new();
    for spec in provider_config_specs(args) {
        let raw = match homeboy_core::config::read_json_spec_to_string(&spec) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
        };
        for candidate in provider_config_candidate_paths(&value) {
            add_candidate_extra_workspace(
                &candidate,
                "provider_config",
                &source_canon,
                &mut seen,
                &mut workspaces,
            )?;
        }
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

    if let Some(path) = agent_task_plan_file_path(&spec, source_path) {
        if path.is_file() {
            let workspace_path = containing_checkout_or_parent(&path)?;
            if let Ok(canon) = workspace_path.canonicalize() {
                if canon != source_canon && !canon.starts_with(&source_canon) {
                    seen.insert(canon.clone());
                    workspaces.push(ExtraLabWorkspace {
                        role: "agent_task_plan".to_string(),
                        path: canon,
                        snapshot_includes: Vec::new(),
                        bootstrap_node_dependencies: false,
                        bootstrap_command: None,
                        allow_dirty_lab_workspace: false,
                        source_provenance: None,
                    });
                }
            }
        }
    }

    let raw = match read_agent_task_plan_spec_to_string(&spec, source_path) {
        Ok(raw) => raw,
        Err(_) => return Ok(workspaces),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(workspaces),
    };
    for candidate in component_contract_candidate_paths(&value) {
        add_candidate_extra_workspace(
            &candidate,
            "component_contract",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }
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

/// Discover controller-local workspaces embedded in a batch-cook fanout input
/// before the offloaded `agent-task fanout run-plan` command reaches the runner.
/// Child cooks often carry controller paths in `cwd` or `workspace`; syncing
/// them here lets the later argument remapper rewrite the JSON to runner paths.
pub(super) fn agent_task_fanout_extra_workspaces(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let Some(spec) = agent_task_fanout_input_spec(args) else {
        return Ok(Vec::new());
    };
    let raw = match read_fanout_input_spec_to_string(&spec, source_path) {
        Ok(raw) => raw,
        Err(_) => return Ok(Vec::new()),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(Vec::new()),
    };
    let Some(cooks) = value.get("cooks").and_then(serde_json::Value::as_array) else {
        return Ok(Vec::new());
    };

    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();
    for cook in cooks {
        for field in ["cwd", "workspace"] {
            let Some(candidate) = cook.get(field).and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(path) = fanout_workspace_candidate_path(candidate, source_path) else {
                continue;
            };
            add_candidate_extra_workspace(
                &path.display().to_string(),
                "agent_task_fanout_cook_workspace",
                &source_canon,
                &mut seen,
                &mut workspaces,
            )?;
        }
    }

    Ok(workspaces)
}

/// Resolve runtime components declared by the selected agent-task provider and
/// add their controller-local checkouts to the Lab workspace handoff.
pub(super) fn agent_task_provider_runtime_component_extra_workspaces(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let Some(spec) = agent_task_plan_spec(args) else {
        return Ok(Vec::new());
    };
    let raw = read_agent_task_plan_spec_to_string(&spec, source_path)?;
    let plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "plan",
            format!("invalid agent-task run-plan --plan payload: {error}"),
            Some(spec.clone()),
            Some(vec![
                "Pass a valid AgentTaskPlan JSON payload or @file to `agent-task run-plan --plan`."
                    .to_string(),
            ]),
        )
    })?;

    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();
    for component_id in agent_task_provider::lab_runtime_component_ids_for_plan(&plan) {
        let resolved = component::resolve_effective(Some(&component_id), None, None).map_err(|error| {
            Error::validation_invalid_argument(
                "lab_runtime_components",
                format!(
                    "agent-task provider requires Lab runtime component `{component_id}`, but Homeboy could not resolve it: {}",
                    error
                ),
                Some(component_id.clone()),
                Some(vec![
                    format!("Register component `{component_id}` on the controller or provide a component with that id before Lab offload."),
                    "Run `homeboy component list` to inspect configured components.".to_string(),
                ]),
            )
        })?;
        add_candidate_extra_workspace(
            &resolved.local_path,
            "agent_task_runtime_component",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }
    Ok(workspaces)
}

pub(super) fn declared_path_input_values(args: &[String], path_inputs: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for input in path_inputs {
        values.extend(if input.starts_with("--") {
            values_for_declared_path_flag(args, input)
        } else {
            values_for_declared_setting_path(args, input)
        });
    }
    values
}

fn values_for_declared_path_flag(args: &[String], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == flag {
            if let Some(value) = iter.peek() {
                values.push((*value).to_string());
            }
        } else if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            values.push(value.to_string());
        }
    }
    values
}

fn values_for_declared_setting_path(args: &[String], setting_path: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        let (raw, json) = if arg == "--setting" || arg == "--setting-json" {
            let json = arg == "--setting-json";
            let Some(raw) = iter.next() else { continue };
            (raw.as_str(), json)
        } else if let Some(raw) = arg.strip_prefix("--setting=") {
            (raw, false)
        } else if let Some(raw) = arg.strip_prefix("--setting-json=") {
            (raw, true)
        } else {
            continue;
        };
        let Some((key, value)) = raw.split_once('=') else {
            continue;
        };
        if key == setting_path {
            values.push(value.to_string());
        } else if json {
            let Some(suffix) = setting_path
                .strip_prefix(key)
                .and_then(|suffix| suffix.strip_prefix('.'))
            else {
                continue;
            };
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) {
                if let Some(current) = suffix
                    .split('.')
                    .try_fold(&parsed, |current, segment| current.get(segment))
                {
                    collect_path_setting_json_values(current, &mut values);
                }
            }
        }
    }
    values
}

pub(super) fn path_values_extra_workspaces(
    values: Vec<String>,
    source_path: &Path,
    role: &str,
) -> Result<Vec<ExtraLabWorkspace>> {
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();

    for value in values {
        add_candidate_extra_workspace(&value, role, &source_canon, &mut seen, &mut workspaces)?;
    }

    Ok(workspaces)
}

pub(super) fn runtime_refresh_source_extra_workspaces(
    args: &[String],
    source_path: &Path,
    allow_dirty_lab_workspace: bool,
) -> Result<Vec<ExtraLabWorkspace>> {
    let Some(source) = runtime_refresh_source_arg(args) else {
        return Ok(Vec::new());
    };
    let source = PathBuf::from(shellexpand::tilde(&source).to_string());
    if !source.is_dir() {
        return Ok(Vec::new());
    }

    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();
    add_candidate_extra_workspace(
        &source.display().to_string(),
        "runtime_refresh_source",
        &source_canon,
        &mut seen,
        &mut workspaces,
    )?;
    for workspace in &mut workspaces {
        workspace.allow_dirty_lab_workspace = allow_dirty_lab_workspace;
        workspace.source_provenance = Some(runtime_refresh_source_provenance(&workspace.path));
    }
    Ok(workspaces)
}

pub(super) fn extension_source_extra_workspaces(
    args: &[String],
    source_path: &Path,
    allow_dirty_lab_workspace: bool,
) -> Result<Vec<ExtraLabWorkspace>> {
    let Some(source) = extension_source_arg(args) else {
        return Ok(Vec::new());
    };
    let source = PathBuf::from(shellexpand::tilde(&source).to_string());
    if !source.is_dir() {
        return Ok(Vec::new());
    }

    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();
    add_candidate_extra_workspace(
        &source.display().to_string(),
        "extension_source",
        &source_canon,
        &mut seen,
        &mut workspaces,
    )?;
    for workspace in &mut workspaces {
        workspace.allow_dirty_lab_workspace = allow_dirty_lab_workspace;
        workspace.source_provenance = Some(runtime_refresh_source_provenance(&workspace.path));
    }
    Ok(workspaces)
}

fn extension_source_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "extension" {
            let Some(subcommand) = iter.next() else {
                break;
            };
            if subcommand == "refresh" {
                return iter.find(|value| !value.starts_with('-')).cloned();
            }
            if subcommand == "dev-run" {
                while let Some(value) = iter.next() {
                    if value == "--" {
                        break;
                    }
                    if value == "--source" {
                        return iter.next().cloned();
                    }
                    if let Some(source) = value.strip_prefix("--source=") {
                        return Some(source.to_string());
                    }
                }
            }
            break;
        }
    }
    None
}

fn runtime_refresh_source_arg(args: &[String]) -> Option<String> {
    if !args.windows(2).any(|window| {
        matches!(window, [command, subcommand] if command == "runtime" && subcommand == "refresh")
    }) {
        return None;
    }

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--source" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--source=") {
            return Some(value.to_string());
        }
    }
    None
}

fn runtime_refresh_source_provenance(path: &Path) -> serde_json::Value {
    let git_branch = super::workspace::git_output(path, &["branch", "--show-current"])
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            super::workspace::git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()
        });
    let git_sha = super::workspace::git_output(path, &["rev-parse", "HEAD"])
        .ok()
        .filter(|value| !value.is_empty());
    let git_remote = super::workspace::git_output(path, &["config", "--get", "remote.origin.url"])
        .ok()
        .filter(|value| !value.is_empty());
    let dirty = super::workspace::git_output(path, &["status", "--porcelain=v1"])
        .ok()
        .map(|status| !status.is_empty());

    serde_json::json!({
        "schema": "homeboy/runtime-refresh-source/v1",
        "local_path": path.display().to_string(),
        "git_branch": git_branch,
        "git_sha": git_sha,
        "git_remote": git_remote,
        "dirty": dirty,
    })
}

pub(super) fn resolve_path_setting_workspace_refs_in_args(
    args: &[String],
) -> Result<(Vec<String>, Vec<WorkspaceRefResolution>)> {
    let mut resolutions = Vec::new();
    let rewritten = rewrite_path_setting_workspace_refs_in_args(args, &mut resolutions)?;
    Ok((rewritten, resolutions))
}

pub(super) fn workspace_ref_extra_workspaces(
    resolutions: &[WorkspaceRefResolution],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();

    for resolution in resolutions {
        add_candidate_workspace_ref_extra_workspace(
            resolution,
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }

    Ok(workspaces)
}

fn add_candidate_workspace_ref_extra_workspace(
    resolution: &WorkspaceRefResolution,
    source_canon: &Path,
    seen: &mut BTreeSet<PathBuf>,
    workspaces: &mut Vec<ExtraLabWorkspace>,
) -> Result<()> {
    if !resolution.workspace_path.exists() {
        return Err(Error::validation_invalid_argument(
            "workspace_ref",
            format!(
                "Lab offload workspace ref `{}` resolved to a controller path that does not exist",
                resolution.raw_ref
            ),
            Some(resolution.resolved_path.display().to_string()),
            Some(vec![
                "Use an active Homeboy worktree handle and an existing optional subpath, e.g. @workspace:repo@branch/path/to/file.".to_string(),
            ]),
        ));
    }
    let path = resolution.workspace_path.clone();
    let canon = canonical_existing_dir(&path.display().to_string(), "workspace_ref")?;
    if canon == source_canon || canon.starts_with(source_canon) || !seen.insert(canon.clone()) {
        return Ok(());
    }
    workspaces.push(ExtraLabWorkspace {
        role: "path_setting_workspace_ref".to_string(),
        path: canon,
        snapshot_includes: Vec::new(),
        bootstrap_node_dependencies: false,
        bootstrap_command: None,
        allow_dirty_lab_workspace: false,
        source_provenance: Some(serde_json::json!({
            "source_provenance": "workspace_ref",
            "ref": resolution.raw_ref,
            "handle": resolution.handle,
            "subpath": resolution.subpath,
            "workspace_source": resolution.source_kind,
            "workspace_provenance": resolution.source_provenance,
            "workspace_path": resolution.workspace_path.display().to_string(),
            "resolved_path": resolution.resolved_path.display().to_string(),
        })),
    });
    Ok(())
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
                    "Set the variable to an existing checkout/component path before offload, unset it to use the rig default, or use --placement local to keep the check local.".to_string(),
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
    name.starts_with("HOMEBOY_RIG_COMPONENT_PATH__")
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

    for spec in provider_config_specs(args) {
        let raw = match homeboy_core::config::read_json_spec_to_string(&spec) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
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
    }

    Ok(())
}

fn provider_config_specs(args: &[String]) -> Vec<String> {
    const PROVIDER_CONFIG_FLAGS: &[&str] = &["--provider-config", "--dispatch-provider-config"];

    let mut specs = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if PROVIDER_CONFIG_FLAGS.iter().any(|flag| arg == *flag) {
            if let Some(spec) = iter.next() {
                specs.push(spec.clone());
            }
            continue;
        }
        for flag in PROVIDER_CONFIG_FLAGS {
            if let Some(value) = arg
                .strip_prefix(flag)
                .and_then(|rest| rest.strip_prefix('='))
            {
                specs.push(value.to_string());
            }
        }
    }
    specs
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

fn agent_task_fanout_input_spec(args: &[String]) -> Option<String> {
    let fanout_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "fanout")
            .and_then(|_| args.get(index + 2))
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 2)
    })?;

    let mut iter = args.iter().skip(fanout_index + 1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--input" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--input=") {
            return Some(value.to_string());
        }
    }
    None
}

fn agent_task_plan_file_path(spec: &str, source_path: &Path) -> Option<PathBuf> {
    let path = spec.strip_prefix('@')?;
    if path.trim().is_empty() || path.contains("://") {
        return None;
    }

    let expanded = PathBuf::from(shellexpand::tilde(path).to_string());
    if expanded.is_file() || expanded.is_absolute() {
        return Some(expanded);
    }

    let source_relative = source_path.join(&expanded);
    if source_relative.is_file() {
        return Some(source_relative);
    }

    Some(expanded)
}

fn read_agent_task_plan_spec_to_string(spec: &str, source_path: &Path) -> Result<String> {
    let Some(_) = spec.strip_prefix('@') else {
        return homeboy_core::config::read_json_spec_to_string(spec);
    };
    let Some(path) = agent_task_plan_file_path(spec, source_path) else {
        return homeboy_core::config::read_json_spec_to_string(spec);
    };
    std::fs::read_to_string(&path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read agent-task plan {}", path.display())),
        )
    })
}

fn read_fanout_input_spec_to_string(spec: &str, source_path: &Path) -> Result<String> {
    let Some(_) = spec.strip_prefix('@') else {
        return homeboy_core::config::read_json_spec_to_string(spec);
    };
    let Some(path) = agent_task_plan_file_path(spec, source_path) else {
        return homeboy_core::config::read_json_spec_to_string(spec);
    };
    std::fs::read_to_string(&path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read agent-task fanout input {}", path.display())),
        )
    })
}

fn fanout_workspace_candidate_path(value: &str, source_path: &Path) -> Option<PathBuf> {
    let path = PathBuf::from(shellexpand::tilde(value).to_string());
    if path.is_dir() {
        return Some(path);
    }
    if path.is_relative() {
        let source_relative = source_path.join(&path);
        if source_relative.is_dir() {
            return Some(source_relative);
        }
    }
    homeboy_core::worktree::resolve(value)
        .ok()
        .map(|record| PathBuf::from(record.worktree_path))
        .filter(|path| path.is_dir())
}

pub(super) fn path_setting_values(args: &[String]) -> Vec<String> {
    let mut values = Vec::new();
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
        if arg == "--setting" || arg == "--setting-json" {
            if let Some(raw) = iter.next() {
                push_path_setting_value(raw, &mut values);
            }
            continue;
        }
        if let Some(raw) = arg
            .strip_prefix("--setting=")
            .or_else(|| arg.strip_prefix("--setting-json="))
        {
            push_path_setting_value(raw, &mut values);
        }
    }
    values
}

mod workspace_ref_rewrite;
use workspace_ref_rewrite::*;

#[cfg(test)]
mod tests;

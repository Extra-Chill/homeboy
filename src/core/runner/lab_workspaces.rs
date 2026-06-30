use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::agent_task_scheduler::AgentTaskPlan;
use crate::core::worktree::TaskWorktreeState;
use crate::core::{agent_task_provider, component, worktree, Error, Result};

use super::lab_workspaces_deps::{
    accepted_extra_lab_workspaces, add_candidate_extra_workspace, bare_module_imports,
    bootstrap_source_cli_node_dependencies, canonical_existing_dir,
    component_contract_candidate_paths, containing_checkout_or_parent,
    discovered_validation_dependency_workspaces, provider_config_candidate_paths,
    provider_config_source_cli_files,
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
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )?
        .0;
        let mut entry = workspace_mapping_entry(&extra.role, &synced);
        entry.source_provenance = extra.source_provenance.clone();
        if extra.bootstrap_node_dependencies {
            bootstrap_source_cli_node_dependencies(
                runner_id,
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
        super::RunnerExecOptions {
            cwd: Some(remote_workdir.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env: std::collections::HashMap::new(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
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
        let raw = match crate::core::config::read_json_spec_to_string(&spec) {
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

pub(super) fn path_setting_extra_workspaces(
    args: &[String],
    source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    let source_canon = source_path
        .canonicalize()
        .unwrap_or_else(|_| source_path.to_path_buf());
    let mut seen = BTreeSet::new();
    let mut workspaces = Vec::new();

    for value in path_setting_values(args) {
        add_candidate_extra_workspace(
            &value,
            "path_setting",
            &source_canon,
            &mut seen,
            &mut workspaces,
        )?;
    }

    Ok(workspaces)
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
        let raw = match crate::core::config::read_json_spec_to_string(&spec) {
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
        return crate::core::config::read_json_spec_to_string(spec);
    };
    let Some(path) = agent_task_plan_file_path(spec, source_path) else {
        return crate::core::config::read_json_spec_to_string(spec);
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
        return crate::core::config::read_json_spec_to_string(spec);
    };
    let Some(path) = agent_task_plan_file_path(spec, source_path) else {
        return crate::core::config::read_json_spec_to_string(spec);
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
    crate::core::worktree::resolve(value)
        .ok()
        .map(|record| PathBuf::from(record.worktree_path))
        .filter(|path| path.is_dir())
}

fn path_setting_values(args: &[String]) -> Vec<String> {
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

fn push_path_setting_value(raw: &str, values: &mut Vec<String>) {
    let Some((key, value)) = raw.split_once('=') else {
        return;
    };
    if key.trim().is_empty() || value.trim().is_empty() {
        return;
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(value) {
        collect_path_setting_json_values(&json, values);
    } else {
        values.push(value.to_string());
    }
}

fn collect_path_setting_json_values(value: &serde_json::Value, values: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => values.push(text.to_string()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_path_setting_json_values(item, values);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_path_setting_json_values(item, values);
            }
        }
        _ => {}
    }
}

fn rewrite_path_setting_workspace_refs_in_args(
    args: &[String],
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg.clone());
            continue;
        }
        if arg == "--setting" {
            out.push(arg.clone());
            if let Some(raw) = iter.next() {
                out.push(rewrite_path_setting_workspace_ref_pair(
                    raw,
                    false,
                    resolutions,
                )?);
            }
            continue;
        }
        if arg == "--setting-json" {
            out.push(arg.clone());
            if let Some(raw) = iter.next() {
                out.push(rewrite_path_setting_workspace_ref_pair(
                    raw,
                    true,
                    resolutions,
                )?);
            }
            continue;
        }
        if let Some(raw) = arg.strip_prefix("--setting=") {
            out.push(format!(
                "--setting={}",
                rewrite_path_setting_workspace_ref_pair(raw, false, resolutions)?
            ));
            continue;
        }
        if let Some(raw) = arg.strip_prefix("--setting-json=") {
            out.push(format!(
                "--setting-json={}",
                rewrite_path_setting_workspace_ref_pair(raw, true, resolutions)?
            ));
            continue;
        }
        out.push(arg.clone());
    }
    Ok(out)
}

fn rewrite_path_setting_workspace_ref_pair(
    raw: &str,
    is_json: bool,
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<String> {
    let Some((key, value)) = raw.split_once('=') else {
        return Ok(raw.to_string());
    };
    if is_json {
        let mut json: serde_json::Value = match serde_json::from_str(value) {
            Ok(value) => value,
            Err(_) => {
                let rewritten = rewrite_workspace_ref_value(value, resolutions)?;
                return Ok(format!("{key}={rewritten}"));
            }
        };
        rewrite_workspace_refs_in_json_value(&mut json, resolutions)?;
        return serde_json::to_string(&json)
            .map(|value| format!("{key}={value}"))
            .map_err(|err| Error::internal_json(err.to_string(), Some(key.to_string())));
    }

    let rewritten = rewrite_workspace_ref_value(value, resolutions)?;
    Ok(format!("{key}={rewritten}"))
}

fn rewrite_workspace_refs_in_json_value(
    value: &mut serde_json::Value,
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<()> {
    match value {
        serde_json::Value::String(text) => {
            if let Some(rewritten) = maybe_resolve_workspace_ref(text, resolutions)? {
                *text = rewritten;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rewrite_workspace_refs_in_json_value(item, resolutions)?;
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                rewrite_workspace_refs_in_json_value(item, resolutions)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn rewrite_workspace_ref_value(
    value: &str,
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<String> {
    maybe_resolve_workspace_ref(value, resolutions)
        .map(|resolved| resolved.unwrap_or_else(|| value.to_string()))
}

fn maybe_resolve_workspace_ref(
    value: &str,
    resolutions: &mut Vec<WorkspaceRefResolution>,
) -> Result<Option<String>> {
    let Some((handle, subpath)) = parse_workspace_ref(value) else {
        return Ok(None);
    };
    let record = worktree::resolve_workspace_ref(&handle).map_err(|_| {
        Error::validation_invalid_argument(
            "workspace_ref",
            format!("Lab offload workspace ref `{value}` does not match a known workspace handle"),
            Some(value.to_string()),
            Some(vec![
                "Create a Homeboy task worktree or adopt an existing path with `homeboy worktree adopt <handle> <path>`.".to_string(),
            ]),
        )
    })?;
    if record.state() != &TaskWorktreeState::Active {
        return Err(Error::validation_invalid_argument(
            "workspace_ref",
            format!(
                "Lab offload workspace ref `{value}` points at a stale {}",
                record.source_kind()
            ),
            Some(record.handle().to_string()),
            Some(vec![
                "Use an active workspace handle or adopt an existing path before Lab offload."
                    .to_string(),
            ]),
        ));
    }
    let workspace_path = PathBuf::from(record.path());
    let mut resolved = workspace_path.clone();
    if let Some(subpath) = subpath.as_deref() {
        resolved.push(subpath);
    }
    if !resolved.exists() {
        return Err(Error::validation_invalid_argument(
            "workspace_ref",
            format!("Lab offload workspace ref `{value}` resolved to a missing controller path"),
            Some(resolved.display().to_string()),
            Some(vec![
                "Use an existing optional subpath under the referenced Homeboy worktree."
                    .to_string(),
            ]),
        ));
    }
    resolutions.push(WorkspaceRefResolution {
        raw_ref: value.to_string(),
        handle,
        subpath,
        source_kind: record.source_kind().to_string(),
        source_provenance: record.provenance().cloned(),
        workspace_path,
        resolved_path: resolved.clone(),
    });
    Ok(Some(resolved.display().to_string()))
}

fn parse_workspace_ref(value: &str) -> Option<(String, Option<String>)> {
    let rest = value.strip_prefix("@workspace:")?;
    let rest = rest.trim();
    if rest.is_empty() || rest.contains("://") {
        return None;
    }
    let (handle, subpath) = rest
        .split_once('/')
        .map(|(handle, subpath)| (handle, Some(subpath)))
        .unwrap_or((rest, None));
    if handle.trim().is_empty() || subpath.is_some_and(|value| value.trim().is_empty()) {
        return None;
    }
    Some((handle.to_string(), subpath.map(str::to_string)))
}

fn subcommand_index(args: &[String], subcommand: &str) -> Option<usize> {
    args.iter().position(|arg| arg == subcommand)
}

#[cfg(test)]
mod provider_config_candidate_paths_tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{
        agent_task_fanout_extra_workspaces, agent_task_plan_extra_workspaces, agent_task_plan_spec,
        path_setting_extra_workspaces, path_setting_values,
        preflight_provider_config_source_cli_dependencies, provider_config_candidate_paths,
        provider_config_extra_workspaces, resolve_path_setting_workspace_refs_in_args,
        rig_component_path_env_extra_workspaces_from_entries,
        workspace_mapping_entries_for_git_dependency, workspace_ref_extra_workspaces,
        ExtraLabWorkspace,
    };
    use crate::core::runner::{
        ByteFileCounts, RunnerGitDependencyMaterializationOutput, RunnerWorkspaceSyncMode,
    };
    use crate::core::worktree;

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
            "workspace_root": "/local/sample-plugin@cook",
            "mounts": [{ "source": "/local/sample-plugin@cook", "target": "/workspace/sample-plugin" }],
            "runtime_component_paths": {
                "agent_runtime": "/local/sample-plugin",
                "agent_runtime_tools": "/local/sample-plugin-code"
            },
            "provider_plugin_paths": ["/local/ai-provider-for-claude-code"],
            "runtime_overlays": [
                { "kind": "bundled-library", "library": "portable-ai-client", "source": "/local/portable-ai-client@custom-provider-auth", "target": "/runtime/includes/portable-ai-client" }
            ],
            "provider_support": "/local/provider-support",
            "source_cli": "/local/provider/packages/cli/dist/index.js",
            "model": "claude-opus-4-8"
        });

        let paths = provider_config_candidate_paths(&value);

        for expected in [
            "/local/sample-plugin@cook",
            "/local/sample-plugin",
            "/local/sample-plugin-code",
            "/local/ai-provider-for-claude-code",
            "/local/portable-ai-client@custom-provider-auth",
            "/local/provider-support",
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
    fn dispatch_provider_config_file_path_syncs_containing_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let provider = controller.path().join("dispatch-provider");
        let contract = provider.join("contracts/component.json");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(contract.parent().unwrap()).expect("contract dir");
        std::fs::write(&contract, "{}\n").expect("contract file");
        git(&provider, &["init", "-b", "main"]);
        git(&provider, &["config", "user.email", "test@example.com"]);
        git(&provider, &["config", "user.name", "Homeboy Test"]);
        git(&provider, &["add", "."]);
        git(&provider, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "controller".to_string(),
            "run-from-spec".to_string(),
            "loop.json".to_string(),
            "--dispatch-provider-config".to_string(),
            serde_json::json!({
                "provider_plugin_paths": [provider.join("provider-plugin")],
                "component_contracts": [{ "path": contract }],
            })
            .to_string(),
        ];

        let workspaces = provider_config_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "provider_config");
        assert_eq!(workspaces[0].path, provider.canonicalize().unwrap());
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"contracts".to_string()));
    }

    #[test]
    fn agent_task_run_plan_file_path_syncs_containing_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let planner = controller.path().join("plan-owner");
        let tool = controller.path().join("tool-runner");
        let tool_bin = tool.join("packages/cli/dist/index.js");
        let plan = planner.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool cli dir");
        std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
        std::fs::write(tool.join("package-lock.json"), "{}\n").expect("package lock");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "site-generation-loop",
                "tasks": [{
                    "task_id": "task-1",
                    "executor": {
                        "backend": "tool-runner",
                        "config": {
                            "tool_bin": tool_bin,
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
        git(&tool, &["init", "-b", "main"]);
        git(&tool, &["config", "user.email", "test@example.com"]);
        git(&tool, &["config", "user.name", "Homeboy Test"]);
        git(&tool, &["add", "."]);
        git(&tool, &["commit", "-m", "initial"]);

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
        assert_eq!(workspaces[1].path, tool.canonicalize().unwrap());
        assert!(workspaces[1]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[1].bootstrap_node_dependencies);
    }

    #[test]
    fn agent_task_fanout_extra_workspaces_syncs_child_cook_paths() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let child = controller.path().join("homeboy@cook-one");
        let spec = source.join("fanout.json");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&child).expect("child dir");
        std::fs::write(
            &spec,
            serde_json::json!({
                "schema": "homeboy/agent-task-batch-cook-fanout-plan/v1",
                "fanout_id": "fanout/test",
                "cooks": [{
                    "cook_id": "one",
                    "prompt": "fix it",
                    "cwd": child,
                    "to_worktree": "homeboy@fix-one",
                    "head": "fix/one",
                    "verify": ["cargo test -p homeboy"]
                }]
            })
            .to_string(),
        )
        .expect("fanout spec");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "fanout".to_string(),
            "run-plan".to_string(),
            "--input".to_string(),
            format!("@{}", spec.display()),
        ];

        let workspaces = agent_task_fanout_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "agent_task_fanout_cook_workspace");
        assert_eq!(workspaces[0].path, child.canonicalize().unwrap());
    }

    #[test]
    fn agent_task_run_plan_component_contract_paths_get_component_contract_evidence_role() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let component = controller.path().join("domain-component");
        let plan = source.join(".ci/plan.json");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::create_dir_all(&component).expect("component dir");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "plan-with-components",
                "component_contracts": [{
                    "slug": "domain-component",
                    "path": component,
                    "loadAs": "plugin",
                    "activate": true
                }],
                "tasks": [{ "task_id": "task-1", "instructions": "test", "executor": { "backend": "test" } }]
            })
            .to_string(),
        )
        .expect("plan file");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            format!("--plan=@{}", plan.display()),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "component_contract");
        assert_eq!(workspaces[0].path, component.canonicalize().unwrap());
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
    fn agent_task_run_plan_relative_file_reads_from_primary_workspace() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let tool = controller.path().join("tool-runner");
        let tool_bin = tool.join("packages/cli/dist/index.js");
        let plan = source.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(plan.parent().unwrap()).expect("plan dir");
        std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool cli dir");
        std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
        std::fs::write(tool.join("package-lock.json"), "{}\n").expect("package lock");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "site-generation-loop",
                "tasks": [{
                    "task_id": "task-1",
                    "executor": {
                        "backend": "tool-runner",
                        "config": { "tool_bin": tool_bin }
                    }
                }]
            })
            .to_string(),
        )
        .expect("plan file");
        git(&tool, &["init", "-b", "main"]);
        git(&tool, &["config", "user.email", "test@example.com"]);
        git(&tool, &["config", "user.name", "Homeboy Test"]);
        git(&tool, &["add", "."]);
        git(&tool, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            "@.ci/site-generation-loop.agent-task-plan.json".to_string(),
        ];

        let workspaces = agent_task_plan_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "agent_task_plan_config");
        assert_eq!(workspaces[0].path, tool.canonicalize().unwrap());
    }

    #[test]
    fn path_setting_local_file_syncs_containing_checkout() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let tool = controller.path().join("tool-runner");
        let tool_bin = tool.join("packages/cli/dist/index.js");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool cli dir");
        std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
        std::fs::write(tool.join("package-lock.json"), "{}\n").expect("package lock");
        git(&tool, &["init", "-b", "main"]);
        git(&tool, &["config", "user.email", "test@example.com"]);
        git(&tool, &["config", "user.name", "Homeboy Test"]);
        git(&tool, &["add", "."]);
        git(&tool, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            format!("tool_bin={}", tool_bin.display()),
        ];

        let workspaces = path_setting_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "path_setting");
        assert_eq!(workspaces[0].path, tool.canonicalize().unwrap());
        assert!(workspaces[0]
            .snapshot_includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(workspaces[0].bootstrap_node_dependencies);
    }

    #[test]
    fn path_setting_bench_env_directory_values_sync_extra_workspaces() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let fixture_root = controller
            .path()
            .join("blocks-engine@matrix/fixtures/websites");
        let transformer_root = controller.path().join("blocks-engine@matrix");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&fixture_root).expect("fixture root");
        std::fs::write(transformer_root.join("README.md"), "fixture owner\n").expect("repo marker");
        git(&transformer_root, &["init", "-b", "main"]);
        git(
            &transformer_root,
            &["config", "user.email", "test@example.com"],
        );
        git(&transformer_root, &["config", "user.name", "Homeboy Test"]);
        git(&transformer_root, &["add", "."]);
        git(&transformer_root, &["commit", "-m", "initial"]);

        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "static-site-importer-fixture-matrix".to_string(),
            "--setting".to_string(),
            format!(
                "bench_env.SSI_FIXTURE_MATRIX_FIXTURE_ROOT={}",
                fixture_root.display()
            ),
            format!(
                "--setting=bench_env.SSI_FIXTURE_MATRIX_BLOCKS_ENGINE_PHP_TRANSFORMER_PATH={}",
                transformer_root.display()
            ),
        ];

        let workspaces = path_setting_extra_workspaces(&args, &source).expect("workspaces");

        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].role, "path_setting");
        assert_eq!(workspaces[0].path, transformer_root.canonicalize().unwrap());
    }

    #[test]
    fn path_setting_workspace_ref_resolves_to_controller_path_and_syncs_workspace() {
        crate::test_support::with_isolated_home(|home| {
            let store = crate::core::paths::homeboy_data()
                .expect("homeboy data")
                .join("task-worktrees");
            std::fs::create_dir_all(&store).expect("worktree store");
            let source = home.path().join("primary");
            let worktree = home.path().join("repo@cook");
            let nested = worktree.join("fixtures/input.json");
            std::fs::create_dir_all(&source).expect("source dir");
            std::fs::create_dir_all(nested.parent().unwrap()).expect("nested dir");
            std::fs::write(&nested, "{}\n").expect("nested file");
            std::fs::write(
                store.join("repo_cook.json"),
                serde_json::json!({
                    "id": "repo@cook",
                    "component_id": "repo",
                    "source_checkout": home.path().join("repo").display().to_string(),
                    "worktree_path": worktree.display().to_string(),
                    "branch": "cook",
                    "base_ref": "HEAD",
                    "cleanup_policy": "preserve_on_failure",
                    "created_at": "2026-01-01T00:00:00Z",
                    "state": "active"
                })
                .to_string(),
            )
            .expect("worktree record");
            let args = vec![
                "homeboy".to_string(),
                "trace".to_string(),
                "--setting".to_string(),
                "fixture=@workspace:repo@cook/fixtures/input.json".to_string(),
            ];

            let (rewritten, resolutions) =
                resolve_path_setting_workspace_refs_in_args(&args).expect("resolve refs");
            let expected = format!("fixture={}", nested.display());

            assert_eq!(rewritten[3], expected);
            assert_eq!(resolutions.len(), 1);
            assert_eq!(resolutions[0].handle, "repo@cook");
            assert_eq!(
                resolutions[0].subpath.as_deref(),
                Some("fixtures/input.json")
            );

            let workspaces =
                workspace_ref_extra_workspaces(&resolutions, &source).expect("extra workspaces");
            assert_eq!(workspaces.len(), 1);
            assert_eq!(workspaces[0].role, "path_setting_workspace_ref");
            assert_eq!(workspaces[0].path, worktree.canonicalize().unwrap());
            assert_eq!(
                workspaces[0].source_provenance.as_ref().unwrap()["ref"],
                "@workspace:repo@cook/fixtures/input.json"
            );
        });
    }

    #[test]
    fn path_setting_workspace_ref_resolves_inside_setting_json() {
        crate::test_support::with_isolated_home(|home| {
            let store = crate::core::paths::homeboy_data()
                .expect("homeboy data")
                .join("task-worktrees");
            std::fs::create_dir_all(&store).expect("worktree store");
            let worktree = home.path().join("repo@cook");
            let nested = worktree.join("data/corpus");
            std::fs::create_dir_all(&nested).expect("nested dir");
            std::fs::write(
                store.join("repo_cook.json"),
                serde_json::json!({
                    "id": "repo@cook",
                    "component_id": "repo",
                    "source_checkout": home.path().join("repo").display().to_string(),
                    "worktree_path": worktree.display().to_string(),
                    "branch": "cook",
                    "base_ref": "HEAD",
                    "cleanup_policy": "preserve_on_failure",
                    "created_at": "2026-01-01T00:00:00Z",
                    "state": "active"
                })
                .to_string(),
            )
            .expect("worktree record");
            let args = vec![
                "homeboy".to_string(),
                "bench".to_string(),
                "--setting-json".to_string(),
                r#"paths={"corpus":"@workspace:repo@cook/data/corpus","label":"kept"}"#.to_string(),
            ];

            let (rewritten, resolutions) =
                resolve_path_setting_workspace_refs_in_args(&args).expect("resolve refs");
            let raw = rewritten[3].strip_prefix("paths=").expect("paths setting");
            let json: serde_json::Value = serde_json::from_str(raw).expect("setting json");

            assert_eq!(json["corpus"], nested.display().to_string());
            assert_eq!(json["label"], "kept");
            assert_eq!(resolutions.len(), 1);
            assert!(path_setting_values(&rewritten)
                .iter()
                .any(|value| value == &nested.display().to_string()));
        });
    }

    #[test]
    fn path_setting_workspace_ref_resolves_adopted_workspace() {
        crate::test_support::with_isolated_home(|home| {
            let source = home.path().join("primary");
            let workspace = home.path().join("external-workspace");
            let nested = workspace.join("fixtures/input.json");
            std::fs::create_dir_all(&source).expect("source dir");
            std::fs::create_dir_all(nested.parent().unwrap()).expect("nested dir");
            std::fs::write(&nested, "{}\n").expect("nested file");
            worktree::adopt(worktree::WorktreeAdoptOptions {
                handle: "external".to_string(),
                path: workspace.display().to_string(),
                kind: Some("local_checkout".to_string()),
                provenance: Some(serde_json::json!({
                    "source": "test-harness",
                    "note": "opaque caller metadata"
                })),
            })
            .expect("adopt workspace");
            let args = vec![
                "homeboy".to_string(),
                "trace".to_string(),
                "--setting".to_string(),
                "fixture=@workspace:external/fixtures/input.json".to_string(),
            ];

            let (rewritten, resolutions) =
                resolve_path_setting_workspace_refs_in_args(&args).expect("resolve refs");
            let expected = format!(
                "fixture={}",
                workspace
                    .canonicalize()
                    .unwrap()
                    .join("fixtures/input.json")
                    .display()
            );

            assert_eq!(rewritten[3], expected);
            assert_eq!(resolutions.len(), 1);
            assert_eq!(resolutions[0].handle, "external");
            assert_eq!(resolutions[0].source_kind, "adopted_workspace");
            assert_eq!(
                resolutions[0].source_provenance.as_ref().unwrap()["source"],
                "test-harness"
            );
            assert_eq!(
                resolutions[0].subpath.as_deref(),
                Some("fixtures/input.json")
            );

            let workspaces =
                workspace_ref_extra_workspaces(&resolutions, &source).expect("extra workspaces");
            assert_eq!(workspaces.len(), 1);
            assert_eq!(workspaces[0].path, workspace.canonicalize().unwrap());
            let provenance = workspaces[0].source_provenance.as_ref().unwrap();
            assert_eq!(provenance["workspace_source"], "adopted_workspace");
            assert_eq!(
                provenance["workspace_provenance"]["note"],
                "opaque caller metadata"
            );
        });
    }

    #[test]
    fn path_setting_workspace_ref_missing_adopted_path_fails_locally() {
        crate::test_support::with_isolated_home(|home| {
            let workspace = home.path().join("external-workspace");
            std::fs::create_dir_all(&workspace).expect("workspace dir");
            worktree::adopt(worktree::WorktreeAdoptOptions {
                handle: "external".to_string(),
                path: workspace.display().to_string(),
                kind: None,
                provenance: None,
            })
            .expect("adopt workspace");
            std::fs::remove_dir_all(&workspace).expect("remove adopted workspace");
            let args = vec![
                "homeboy".to_string(),
                "trace".to_string(),
                "--setting=fixture=@workspace:external".to_string(),
            ];

            let err = resolve_path_setting_workspace_refs_in_args(&args)
                .expect_err("missing adopted workspace path should fail");

            assert_eq!(err.details["field"], "workspace_ref");
            assert!(err
                .message
                .contains("resolved to a missing controller path"));
        });
    }

    #[test]
    fn path_setting_workspace_ref_missing_handle_fails_locally() {
        crate::test_support::with_isolated_home(|_| {
            let args = vec![
                "homeboy".to_string(),
                "trace".to_string(),
                "--setting=fixture=@workspace:missing@cook/file.json".to_string(),
            ];

            let err = resolve_path_setting_workspace_refs_in_args(&args)
                .expect_err("missing workspace ref should fail");

            assert_eq!(err.details["field"], "workspace_ref");
            assert!(err
                .message
                .contains("does not match a known workspace handle"));
        });
    }

    #[test]
    fn path_setting_workspace_ref_removed_record_fails_as_stale() {
        crate::test_support::with_isolated_home(|home| {
            let store = crate::core::paths::homeboy_data()
                .expect("homeboy data")
                .join("task-worktrees");
            std::fs::create_dir_all(&store).expect("worktree store");
            let worktree = home.path().join("repo@old");
            std::fs::create_dir_all(&worktree).expect("worktree dir");
            std::fs::write(
                store.join("repo_old.json"),
                serde_json::json!({
                    "id": "repo@old",
                    "component_id": "repo",
                    "source_checkout": home.path().join("repo").display().to_string(),
                    "worktree_path": worktree.display().to_string(),
                    "branch": "old",
                    "base_ref": "HEAD",
                    "cleanup_policy": "preserve_on_failure",
                    "created_at": "2026-01-01T00:00:00Z",
                    "state": "removed"
                })
                .to_string(),
            )
            .expect("worktree record");
            let args = vec![
                "homeboy".to_string(),
                "trace".to_string(),
                "--setting".to_string(),
                "fixture=@workspace:repo@old".to_string(),
            ];

            let err = resolve_path_setting_workspace_refs_in_args(&args)
                .expect_err("removed workspace ref should fail");

            assert_eq!(err.details["field"], "workspace_ref");
            assert!(err.message.contains("stale task_worktree"));
        });
    }

    #[test]
    fn workspace_ref_provenance_is_recorded_on_mapping_entry() {
        let workspace = ExtraLabWorkspace {
            role: "path_setting_workspace_ref".to_string(),
            path: PathBuf::from("/local/repo@cook"),
            snapshot_includes: Vec::new(),
            bootstrap_node_dependencies: false,
            source_provenance: Some(serde_json::json!({
                "source_provenance": "workspace_ref",
                "ref": "@workspace:repo@cook/file.json"
            })),
        };

        assert_eq!(
            workspace.source_provenance.as_ref().unwrap()["source_provenance"],
            "workspace_ref"
        );
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
                    "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__PLUGIN".to_string(),
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
                    "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__PLUGIN".to_string(),
                    missing.display().to_string(),
                )],
            )
            .expect_err("missing path");

            assert_eq!(
                err.details["field"],
                "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__PLUGIN"
            );
            assert!(err.message.contains("controller-side path does not exist"));
        });
    }

    #[test]
    #[cfg(unix)]
    fn agent_task_run_plan_syncs_symlinked_dependency_target_inside_primary_workspace() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let tool = controller.path().join("tool-runner");
        let tool_bin = tool.join("packages/cli/dist/index.js");
        let symlink = source.join(".ci/tool-runner");
        let plan = source.join(".ci/site-generation-loop.agent-task-plan.json");
        std::fs::create_dir_all(symlink.parent().unwrap()).expect("ci dir");
        std::fs::create_dir_all(tool_bin.parent().unwrap()).expect("tool cli dir");
        std::fs::write(&tool_bin, "#!/usr/bin/env node\n").expect("tool bin");
        std::fs::write(tool.join("package-lock.json"), "{}\n").expect("package lock");
        std::os::unix::fs::symlink(&tool, &symlink).expect("tool symlink");
        std::fs::write(
            &plan,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "site-generation-loop",
                "tasks": [{
                    "task_id": "task-1",
                    "executor": {
                        "backend": "tool-runner",
                        "config": {
                            "tool_bin": symlink.join("packages/cli/dist/index.js")
                        }
                    },
                    "instructions": "test"
                }]
            })
            .to_string(),
        )
        .expect("plan file");
        git(&tool, &["init", "-b", "main"]);
        git(&tool, &["config", "user.email", "test@example.com"]);
        git(&tool, &["config", "user.name", "Homeboy Test"]);
        git(&tool, &["add", "."]);
        git(&tool, &["commit", "-m", "initial"]);

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
        assert_eq!(workspaces[0].path, tool.canonicalize().unwrap());
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
            dirty_overlay: false,
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            counts: ByteFileCounts {
                files: 7,
                bytes: 42,
            },
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

#[cfg(test)]
mod validation_dependency_mapping_tests {
    use super::workspace_mapping_entry_for_validation_dependency;
    use crate::core::runner::RunnerValidationDependencySyncOutput;

    fn dependency_output() -> RunnerValidationDependencySyncOutput {
        RunnerValidationDependencySyncOutput {
            id: "shared-runtime".to_string(),
            role: "validation_dependency".to_string(),
            local_path: "/Users/dev/Developer/shared-runtime".to_string(),
            remote_path: "/srv/_lab_workspaces/shared-runtime".to_string(),
            evidence_path: "/srv/_lab_workspaces/shared-runtime/.homeboy/lab-source-evidence.json"
                .to_string(),
        }
    }

    #[test]
    fn maps_dependency_local_path_to_materialized_remote_path() {
        let entry = workspace_mapping_entry_for_validation_dependency(&dependency_output());

        // The controller-local checkout path must remap to the runner-side
        // materialized path so remote dependency resolvers receive usable paths
        // instead of controller-local ones (#3292).
        assert_eq!(entry.local_path(), "/Users/dev/Developer/shared-runtime");
        assert_eq!(entry.remote_path(), "/srv/_lab_workspaces/shared-runtime");
    }

    #[test]
    fn preserves_dependency_role_and_identity_metadata() {
        let entry = workspace_mapping_entry_for_validation_dependency(&dependency_output());

        let value = serde_json::to_value(&entry).expect("serialize mapping entry");
        assert_eq!(value["role"], "validation_dependency");
        assert_eq!(value["snapshot_identity"], "shared-runtime");
        assert_eq!(
            value["dependency_freshness"]["source_provenance"],
            "validation_dependency_sibling"
        );
        assert_eq!(value["dependency_freshness"]["id"], "shared-runtime");
    }
}

#[cfg(test)]
mod runtime_overlay_tests {
    use super::{
        lab_runtime_overlay_metadata, parse_runtime_overlays, runtime_overlay_env_overrides,
        runtime_overlay_install_workdir, sync_lab_runtime_overlays, LabWorkspaceMappingEntry,
        RuntimeOverlayInstallStep, RuntimeOverlaySpec, SyncedRuntimeOverlay,
        LAB_RUNTIME_OVERLAY_SCHEMA,
    };

    fn synced(role: &str, remote: &str, env: Option<&str>) -> SyncedRuntimeOverlay {
        SyncedRuntimeOverlay {
            role: role.to_string(),
            local_path: format!("/local/{role}"),
            remote_path: remote.to_string(),
            install_workdir: None,
            install_ran: false,
            expose_remote_path_env: env.map(str::to_string),
            build_provenance:
                crate::core::runner::runtime_overlay_freshness::RuntimeOverlayBuildProvenance::unverifiable(),
        }
    }

    #[test]
    fn parses_artifact_only_overlay_without_install_step() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = RuntimeOverlaySpec {
            path: dir.path().display().to_string(),
            role: None,
            snapshot_includes: vec!["cli".to_string()],
            install: None,
            expose_remote_path_env: None,
        };

        let overlays = parse_runtime_overlays(vec![spec]).expect("parse overlays");

        assert_eq!(overlays.len(), 1);
        let overlay = &overlays[0];
        // Default role is applied and the artifact path is canonicalized to the
        // existing directory; no install step and no env surfacing requested.
        assert_eq!(overlay.workspace.role, "runtime_overlay");
        assert_eq!(overlay.workspace.snapshot_includes, vec!["cli".to_string()]);
        assert!(overlay.install.is_none());
        assert!(overlay.expose_remote_path_env.is_none());
        assert!(!overlay.workspace.bootstrap_node_dependencies);
    }

    #[test]
    fn parses_overlay_with_opaque_install_step_and_env_surfacing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = RuntimeOverlaySpec {
            path: dir.path().display().to_string(),
            role: Some("cli-runtime".to_string()),
            snapshot_includes: Vec::new(),
            install: Some(RuntimeOverlayInstallStep {
                // Opaque, ecosystem-agnostic placeholder argv supplied as data.
                command: vec!["install-tool".to_string(), "deps".to_string()],
                workdir: Some("cli".to_string()),
            }),
            expose_remote_path_env: Some("RUNTIME_CLI_DIR".to_string()),
        };

        let overlays = parse_runtime_overlays(vec![spec]).expect("parse overlays");

        let overlay = &overlays[0];
        assert_eq!(overlay.workspace.role, "cli-runtime");
        let install = overlay.install.as_ref().expect("install step");
        assert_eq!(install.command, vec!["install-tool", "deps"]);
        assert_eq!(install.workdir.as_deref(), Some("cli"));
        assert_eq!(
            overlay.expose_remote_path_env.as_deref(),
            Some("RUNTIME_CLI_DIR")
        );
    }

    #[test]
    fn rejects_install_step_with_empty_command_argv() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = RuntimeOverlaySpec {
            path: dir.path().display().to_string(),
            role: None,
            snapshot_includes: Vec::new(),
            install: Some(RuntimeOverlayInstallStep {
                command: vec!["   ".to_string()],
                workdir: None,
            }),
            expose_remote_path_env: None,
        };

        let err = parse_runtime_overlays(vec![spec]).expect_err("empty command rejected");
        assert!(err.message.contains("non-empty command argv"));
    }

    #[test]
    fn rejects_overlay_with_missing_artifact_directory() {
        let spec = RuntimeOverlaySpec {
            path: "/definitely/not/a/real/overlay/dir".to_string(),
            role: None,
            snapshot_includes: Vec::new(),
            install: None,
            expose_remote_path_env: None,
        };

        assert!(parse_runtime_overlays(vec![spec]).is_err());
    }

    #[test]
    fn install_workdir_defaults_to_overlay_remote_path() {
        assert_eq!(
            runtime_overlay_install_workdir("/srv/_lab/overlay", None),
            "/srv/_lab/overlay"
        );
        assert_eq!(
            runtime_overlay_install_workdir("/srv/_lab/overlay", Some("  ")),
            "/srv/_lab/overlay"
        );
    }

    #[test]
    fn install_workdir_resolves_relative_against_remote_and_honors_absolute() {
        assert_eq!(
            runtime_overlay_install_workdir("/srv/_lab/overlay", Some("cli")),
            "/srv/_lab/overlay/cli"
        );
        assert_eq!(
            runtime_overlay_install_workdir("/srv/_lab/overlay", Some("/srv/_lab/sibling")),
            "/srv/_lab/sibling"
        );
    }

    #[test]
    fn env_overrides_only_surface_overlays_that_declared_an_env_var() {
        let overlays = vec![
            synced("cli", "/srv/_lab/cli", Some("RUNTIME_CLI_DIR")),
            synced("data", "/srv/_lab/data", None),
        ];

        let overrides = runtime_overlay_env_overrides(&overlays);

        assert_eq!(
            overrides,
            vec![("RUNTIME_CLI_DIR".to_string(), "/srv/_lab/cli".to_string())]
        );
    }

    #[test]
    fn env_overrides_empty_when_no_overlays() {
        assert!(runtime_overlay_env_overrides(&[]).is_empty());
    }

    #[test]
    fn metadata_records_schema_count_and_overlays() {
        let overlays = vec![synced("cli", "/srv/_lab/cli", Some("RUNTIME_CLI_DIR"))];

        let value = lab_runtime_overlay_metadata(&overlays);

        assert_eq!(value["schema"], LAB_RUNTIME_OVERLAY_SCHEMA);
        assert_eq!(value["count"], 1);
        assert_eq!(value["overlays"][0]["role"], "cli");
        assert_eq!(value["overlays"][0]["remote_path"], "/srv/_lab/cli");
        assert_eq!(
            value["overlays"][0]["expose_remote_path_env"],
            "RUNTIME_CLI_DIR"
        );
    }

    #[test]
    fn empty_overlay_list_is_a_no_op_and_leaves_mapping_unchanged() {
        // Components WITHOUT overlays must not sync anything or mutate the
        // workspace mapping — sync_lab_runtime_overlays short-circuits before
        // touching the runner, so this is safe to call without one.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut mapping: Vec<LabWorkspaceMappingEntry> = Vec::new();

        let synced = sync_lab_runtime_overlays(
            "unused-runner",
            &dir.path().display().to_string(),
            Vec::new(),
            &mut mapping,
        )
        .expect("no-op overlay sync");

        assert!(synced.is_empty());
        assert!(mapping.is_empty());
    }
}

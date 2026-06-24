//! Rig primitive — code-defined, reproducible local dev environments.
//!
//! A **rig** is a named bundle of components, local services, pre-flight
//! checks, and a build pipeline, declared as JSON. `rig up` materializes it,
//! `rig check` reports health, `rig down` tears it down.
//!
//! Phase 1 scope:
//! - Spec schema with components, services, symlinks, shared paths, and linear pipelines
//! - Service kinds: `http-static`, `command`, `external` (adopted)
//! - Pipeline step kinds: `service`, `build`, `extension`, `git`, `stack`,
//!   `command`, `symlink`, `shared-path`, `patch`, `check`
//! - Check probes: `http`, `file` (+ `contains`), `command`, `newer_than`
//!   (mtime / process-start staleness)
//! - State file at `~/.config/homeboy/rigs/{id}.state/state.json`
//! - CLI verbs: `list`, `show`, `up`, `check`, `down`, `status`
//!
//! Deferred to later phases (see example-org/homeboy#1462+): deeper stack
//! lifecycle automation, extension-registered service kinds, spec sharing.

pub mod app;
pub mod artifact_index;
pub mod capabilities;
pub mod check;
mod discovery;
pub mod expand;
pub mod install;
mod json_config;
pub mod lease;
pub mod lint;
pub mod pipeline;
pub mod runner;
pub mod service;
pub mod source;
pub mod spec;
pub mod stack;
pub mod state;
pub mod toolchain;
pub mod workloads;

pub use app::{AppLauncherAction, AppLauncherOptions, AppLauncherReport};
pub use artifact_index::{
    for_run as artifact_index_for_run,
    for_run_with_artifacts as artifact_index_for_run_with_artifacts, RigRunArtifactIndex,
    RigRunArtifactRef, RigRunFailedStepRef,
};
pub use capabilities::{evaluate_requirements, plan_requirement_checks, RigRequirementCheckPlan};
pub use install::{
    discover_rigs, discover_stacks, install, read_source_metadata, read_stack_source_metadata,
    DiscoveredRig, DiscoveredStack, InstalledStack, RigInstallResult, RigSourceMetadata,
    StackSourceMetadata,
};
pub use lease::{acquire_active_run_lease, active_run_leases, ActiveRigRunLease, RigRunLease};
pub use pipeline::{PipelineOutcome, PipelineStepOutcome};
pub use runner::{
    head_sha_and_branch, run_bench_prepare, run_check, run_check_groups, run_down,
    run_fuzz_prepare, run_repair, run_status, run_up, snapshot_state, BenchPrepareReport,
    CheckReport, DownReport, FuzzPrepareReport, RepairReport, RepairResourceReport,
    RigStatusReport, SymlinkStatusReport, SymlinkStatusState, UpReport,
};
pub use service::{DiscoveredProcess, ServiceStatus};
pub use source::{
    list_sources, remove_source, update_all_sources, update_source, update_source_for_rig,
    InvalidRigSourceMetadata, RemovedRigSourceRig, RemovedRigSourceStack, RigSourceGroup,
    RigSourceListResult, RigSourceRemoveResult, RigSourceRig, RigSourceStack,
    RigSourceUpdateResult, RigSourceUpdatedRig, RigSourceUpdatedStack, SkippedRigSourceRig,
    SkippedRigSourceStack, SkippedRigSourceUpdate,
};
pub use spec::{
    AppLauncherPlatform, AppLauncherPreflight, AppLauncherSpec, BenchSpec, CheckSpec,
    ComponentSpec, DiscoverSpec, ExecutableRequirementSpec, FilesystemAssertionKind,
    FilesystemAssertionSpec, NewerThanSpec, PatchOp, PipelineStep, RigRequirementsSpec,
    RigResourcesSpec, RigSpec, ServiceKind, ServiceSpec, SharedPathOp, SharedPathSpec, StackOp,
    SymlinkSpec, TimeSource, TraceConfig, TraceDependencySpec, TraceExperimentArtifactSpec,
    TraceExperimentCommandSpec, TraceExperimentSpec, TraceGuardrailSpec,
    TraceNativePublicPreviewSpec, TracePhaseTemplateSpec, TracePreviewAssetFanoutSpec,
    TraceProfileSpec, TracePublicPreviewMode, TracePublicPreviewSpec, TraceVariantSpec,
    WorkloadSpec,
};
pub use stack::{
    plan_stack_sync, run_component_sync, run_sync, RigStackPlanEntry, RigStackSyncEntry,
    RigStackSyncReport,
};
pub use state::{
    ComponentSnapshot, MaterializedRigState, RigState, RigStateSnapshot, ServiceState,
};
pub use workloads::{
    check_groups_for_extension_workloads, extension_ids_for_workloads, extension_workload_inputs,
    invocation_requirements_for_extension_workloads, runner_capabilities_for_extension,
    trace_dependencies_for_extension, workload_path_expansions_for_extension,
    workloads_for_extension, RigExtensionWorkloadInputs, RigWorkloadKind, RigWorkloadPathExpansion,
};

use crate::core::error::{Error, Result};
use crate::core::paths;
use discovery::discover_rigs_for_install;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static LOCAL_PACKAGE_ROOTS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();

fn remember_local_package_root(id: &str, package_root: &Path) {
    let roots = LOCAL_PACKAGE_ROOTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut roots) = roots.lock() {
        roots.insert(id.to_string(), package_root.to_path_buf());
    }
}

fn forget_local_package_root(id: &str) {
    if let Some(roots) = LOCAL_PACKAGE_ROOTS.get() {
        if let Ok(mut roots) = roots.lock() {
            roots.remove(id);
        }
    }
}

pub(crate) fn local_package_root(id: &str) -> Option<PathBuf> {
    LOCAL_PACKAGE_ROOTS
        .get()
        .and_then(|roots| roots.lock().ok()?.get(id).cloned())
}

/// Byte-compare the contents of two files.
///
/// Returns `true` only when both files are readable and have identical bytes.
/// Any I/O error on either side yields `false`.
pub(crate) fn files_match(left: &Path, right: &Path) -> bool {
    match (fs::read(left), fs::read(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn read_config(id: &str) -> Result<(RigSpec, Option<String>)> {
    let path = paths::rig_config(id)?;
    if !path.exists() {
        if let Some(error) = stale_source_error(id, &path) {
            return Err(error);
        }
        let suggestions = list_ids().unwrap_or_default();
        return Err(Error::rig_not_found(id, suggestions));
    }
    let content = fs::read_to_string(&path).map_err(|e| {
        Error::internal_unexpected(format!("Failed to read rig {}: {}", path.display(), e))
    })?;
    let mut spec: RigSpec = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse rig spec {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    apply_trace_workload_defaults(&mut spec)?;
    let declared_id = (!spec.id.is_empty() && spec.id != id).then(|| spec.id.clone());
    spec.id = id.to_string();
    forget_local_package_root(id);
    Ok((spec, declared_id))
}

fn read_spec_from_path(path: &Path, id_hint: Option<&str>, package_root: &Path) -> Result<RigSpec> {
    let value = match install::materialize_rig_spec(path, package_root)? {
        Some(value) => value,
        None => {
            let content = fs::read_to_string(path).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("read rig spec {}", path.display())),
                )
            })?;
            serde_json::from_str(&content).map_err(|e| {
                Error::validation_invalid_json(
                    e,
                    Some(format!("parse rig spec {}", path.display())),
                    Some(content.chars().take(200).collect()),
                )
            })?
        }
    };
    let mut spec: RigSpec = serde_json::from_value(value).map_err(|e| {
        Error::validation_invalid_json(e, Some(format!("parse rig spec {}", path.display())), None)
    })?;
    if spec.id.is_empty() {
        spec.id = id_hint.unwrap_or_default().to_string();
    }
    spec.id = crate::core::extension::slugify_id(&spec.id)?;
    remember_local_package_root(&spec.id, package_root);
    apply_trace_workload_defaults(&mut spec)?;
    Ok(spec)
}

fn local_package_root_for_rig_json(path: &Path) -> PathBuf {
    let Some(rig_dir) = path.parent() else {
        return PathBuf::from(".");
    };
    if rig_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("rigs")
    {
        return rig_dir
            .parent()
            .and_then(Path::parent)
            .unwrap_or(rig_dir)
            .to_path_buf();
    }
    rig_dir.to_path_buf()
}

fn absolute_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .map_err(|e| Error::internal_io(e.to_string(), Some("get current dir".into())))?
        .join(path))
}

fn apply_trace_workload_defaults(spec: &mut RigSpec) -> Result<()> {
    for (extension_id, defaults) in spec.trace_workload_defaults.clone() {
        let Some(workloads) = spec.trace_workloads.get_mut(&extension_id) else {
            continue;
        };
        for workload in workloads {
            workload.apply_defaults(&defaults);
        }
    }

    for (extension_id, workloads) in spec.trace_workloads.iter_mut() {
        for workload in workloads {
            let Some(template_name) = workload.trace_phase_template.as_deref() else {
                continue;
            };
            let template = spec.trace_phase_templates.get(template_name).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "trace_phase_template",
                    format!(
                        "trace workload '{}' for extension '{}' references unknown trace phase template '{}'",
                        workload.path(),
                        extension_id,
                        template_name
                    ),
                    Some(template_name.to_string()),
                    Some(spec.trace_phase_templates.keys().cloned().collect()),
                )
            })?;
            workload.apply_phase_template(template);
        }
    }
    Ok(())
}

fn stale_source_error(id: &str, config_path: &Path) -> Option<Error> {
    let metadata = read_source_metadata(id)?;
    let package_present = Path::new(&metadata.package_path).exists();
    let rig_present = Path::new(&metadata.rig_path).is_file();
    let config_entry_present = fs::symlink_metadata(config_path).is_ok();
    if package_present && rig_present && !config_entry_present {
        return None;
    }

    let problem = if metadata.linked && !package_present {
        format!(
            "Rig '{}' is installed from linked rig source '{}' but that source path is missing",
            id, metadata.package_path
        )
    } else if !rig_present {
        format!(
            "Rig '{}' has installed source metadata but the recorded rig spec is missing: {}",
            id, metadata.rig_path
        )
    } else {
        format!(
            "Rig '{}' has installed source metadata but its config path is missing: {}",
            id,
            config_path.display()
        )
    };

    Some(
        Error::validation_invalid_argument("rig_id", problem, Some(id.to_string()), None)
            .with_hint("Run `homeboy rig sources list` to inspect installed rig sources")
            .with_hint(format!(
                "Restore the source path or remove the stale source: homeboy rig sources remove {}",
                metadata.package_path
            ))
            .with_hint(format!(
                "After removing it, reinstall the rig source: homeboy rig install {} --id {}",
                metadata.source, id
            )),
    )
}

/// Load a rig spec by ID from `~/.config/homeboy/rigs/{id}.json`.
pub fn load(id: &str) -> Result<RigSpec> {
    read_config(id).map(|(spec, _)| spec)
}

/// Load a rig spec directly from a local package directory or `rig.json` path
/// without installing it into the global rig registry.
pub fn load_local_source(source: &str, id: Option<&str>) -> Result<RigSpec> {
    let path = absolute_path(source)?;
    if path.is_file() {
        if path.file_name().and_then(|name| name.to_str()) != Some("rig.json") {
            return Err(Error::validation_invalid_argument(
                "source",
                "Rig check path must point at rig.json or a package directory",
                Some(source.to_string()),
                None,
            ));
        }
        let package_root = local_package_root_for_rig_json(&path);
        let id_hint = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        return read_spec_from_path(&path, id.or(id_hint), &package_root);
    }
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("Path does not exist: {}", path.display()),
            Some(source.to_string()),
            None,
        ));
    }

    let mut rigs = if id.is_some() {
        discover_rigs_for_install(&path, id, false)?
    } else {
        discover_rigs(&path)?
    };
    if let Some(id) = id {
        let id = crate::core::extension::slugify_id(id)?;
        if rigs.is_empty() {
            return Err(Error::validation_invalid_argument(
                "id",
                format!("Rig '{}' not found in package", id),
                Some(id),
                None,
            ));
        }
    } else if rigs.len() != 1 {
        let available = rigs.iter().map(|rig| rig.id.clone()).collect::<Vec<_>>();
        return Err(Error::validation_invalid_argument(
            "id",
            format!(
                "Package contains multiple rigs; pass --id <rig>. Available: {}",
                available.join(", ")
            ),
            Some(source.to_string()),
            Some(available),
        ));
    }

    let rig = rigs.remove(0);
    read_spec_from_path(&rig.rig_path, Some(&rig.id), &path)
}

/// A loaded rig spec paired with the resolved on-disk package root of its
/// installed source, when one is recorded.
///
/// Command modules repeatedly need both the parsed [`RigSpec`] and the
/// `package_root` derived from the rig's source metadata, so this bundles the
/// pair (and its resolution) in one place instead of re-deriving the same
/// `{ spec, package_root }` field group in every command context.
#[derive(Debug, Clone)]
pub struct RigSourceContext {
    pub spec: RigSpec,
    pub package_root: Option<std::path::PathBuf>,
}

impl RigSourceContext {
    /// Build a source context from an already-loaded spec, resolving the
    /// package root from the rig's recorded source metadata.
    pub fn from_spec(spec: RigSpec) -> Self {
        let package_root = read_source_metadata(&spec.id)
            .map(|metadata| std::path::PathBuf::from(metadata.package_path));
        Self { spec, package_root }
    }

    /// Load a rig spec by ID and resolve its package root.
    pub fn load(id: &str) -> Result<Self> {
        Ok(Self::from_spec(load(id)?))
    }
}

/// Return the JSON-declared rig ID when it differs from the installed ID.
pub fn declared_id(id: &str) -> Result<Option<String>> {
    read_config(id).map(|(_, declared_id)| declared_id)
}

/// List all rig specs in `~/.config/homeboy/rigs/`.
pub fn list() -> Result<Vec<RigSpec>> {
    let dir = paths::rigs()?;
    let mut rigs = Vec::new();
    for entry in json_config::sorted_json_config_entries(
        &dir,
        "list rigs",
        "read rig entry",
        |e, context| Error::internal_unexpected(format!("Failed to {}: {}", context, e)),
    )? {
        if let Ok(spec) = load(&entry.id) {
            rigs.push(spec);
        }
    }
    Ok(rigs)
}

/// Return sorted rig IDs (cheaper than load+collect when you only need IDs,
/// e.g. for error suggestions).
pub fn list_ids() -> Result<Vec<String>> {
    let dir = paths::rigs()?;
    json_config::sorted_json_config_entries(&dir, "list rigs", "read rig entry", |e, context| {
        Error::internal_unexpected(format!("Failed to {}: {}", context, e))
    })
    .map(|entries| {
        entries
            .into_iter()
            .filter(|entry| entry.path.exists())
            .map(|entry| entry.id)
            .collect()
    })
}

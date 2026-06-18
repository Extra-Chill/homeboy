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
    for_run as artifact_index_for_run, RigRunArtifactIndex, RigRunArtifactRef, RigRunFailedStepRef,
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
    head_sha_and_branch, run_bench_prepare, run_check, run_check_groups, run_down, run_repair,
    run_status, run_up, snapshot_state, BenchPrepareReport, CheckReport, DownReport, RepairReport,
    RepairResourceReport, RigStatusReport, SymlinkStatusReport, SymlinkStatusState, UpReport,
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
    SymlinkSpec, TimeSource, TraceDependencySpec, TraceExperimentArtifactSpec,
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
    check_groups_for_extension_workloads, extension_ids_for_workloads,
    invocation_requirements_for_extension_workloads, runner_capabilities_for_extension,
    trace_dependencies_for_extension, workload_path_expansions_for_extension,
    workloads_for_extension, RigWorkloadKind, RigWorkloadPathExpansion,
};

use crate::core::error::{Error, Result};
use crate::core::paths;
use std::fs;
use std::path::Path;

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
    Ok((spec, declared_id))
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

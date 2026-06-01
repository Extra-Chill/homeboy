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
//! Deferred to later phases (see Automattic/homeboy#1462+): deeper stack
//! lifecycle automation, extension-registered service kinds, spec sharing.

pub mod app;
pub mod check;
pub mod expand;
pub mod install;
mod json_config;
pub mod lease;
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
    ComponentSpec, DiscoverSpec, NewerThanSpec, PatchOp, PipelineStep, RigResourcesSpec, RigSpec,
    ServiceKind, ServiceSpec, SharedPathOp, SharedPathSpec, StackOp, SymlinkSpec, TimeSource,
    TraceExperimentArtifactSpec, TraceExperimentCommandSpec, TraceExperimentSpec,
    TraceGuardrailSpec, TraceProfileSpec, TraceVariantSpec, WorkloadSpec,
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
    invocation_requirements_for_extension_workloads, workloads_for_extension, RigWorkloadKind,
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
    let declared_id = (!spec.id.is_empty() && spec.id != id).then(|| spec.id.clone());
    spec.id = id.to_string();
    Ok((spec, declared_id))
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
    .map(|entries| entries.into_iter().map(|entry| entry.id).collect())
}

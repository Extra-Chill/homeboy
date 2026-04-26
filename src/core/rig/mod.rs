//! Rig primitive — code-defined, reproducible local dev environments.
//!
//! A **rig** is a named bundle of components, local services, pre-flight
//! checks, and a build pipeline, declared as JSON. `rig up` materializes it,
//! `rig check` reports health, `rig down` tears it down.
//!
//! Phase 1 scope:
//! - Spec schema with components, services, symlinks, shared paths, and linear pipelines
//! - Service kinds: `http-static`, `command`, `external` (adopted)
//! - Pipeline step kinds: `service`, `build`, `git`, `command`, `symlink`,
//!   `shared-path`, `patch`, `check`
//! - Check probes: `http`, `file` (+ `contains`), `command`, `newer_than`
//!   (mtime / process-start staleness)
//! - State file at `~/.config/homeboy/rigs/{id}.state/state.json`
//! - CLI verbs: `list`, `show`, `up`, `check`, `down`, `status`
//!
//! Deferred to later phases (see Automattic/homeboy#1462+): stack integration,
//! DAG pipelines, extension-registered service kinds, `.app` wrappers,
//! bench composition, spec sharing.

pub mod check;
pub mod expand;
pub mod pipeline;
pub mod runner;
pub mod service;
pub mod spec;
pub mod state;

pub use pipeline::{PipelineOutcome, PipelineStepOutcome};
pub use runner::{
    run_check, run_down, run_status, run_up, snapshot_state, CheckReport, ComponentSnapshot,
    DownReport, RigStateSnapshot, RigStatusReport, UpReport,
};
pub use service::{DiscoveredProcess, ServiceStatus};
pub use spec::{
    BenchSpec, CheckSpec, ComponentSpec, DiscoverSpec, NewerThanSpec, PatchOp, PipelineStep,
    RigSpec, ServiceKind, ServiceSpec, SharedPathOp, SharedPathSpec, SymlinkSpec, TimeSource,
};
pub use state::{RigState, ServiceState};

use crate::error::{Error, Result};
use crate::paths;
use std::fs;

/// Load a rig spec by ID from `~/.config/homeboy/rigs/{id}.json`.
pub fn load(id: &str) -> Result<RigSpec> {
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
    if spec.id.is_empty() {
        spec.id = id.to_string();
    }
    Ok(spec)
}

/// List all rig specs in `~/.config/homeboy/rigs/`.
pub fn list() -> Result<Vec<RigSpec>> {
    let dir = paths::rigs()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut rigs = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|e| Error::internal_unexpected(format!("Failed to list rigs: {}", e)))?
    {
        let entry = entry
            .map_err(|e| Error::internal_unexpected(format!("Failed to read rig entry: {}", e)))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if let Ok(spec) = load(&stem) {
            rigs.push(spec);
        }
    }
    rigs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rigs)
}

/// Return sorted rig IDs (cheaper than load+collect when you only need IDs,
/// e.g. for error suggestions).
pub fn list_ids() -> Result<Vec<String>> {
    let dir = paths::rigs()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|e| Error::internal_unexpected(format!("Failed to list rigs: {}", e)))?
    {
        let entry = entry
            .map_err(|e| Error::internal_unexpected(format!("Failed to read rig entry: {}", e)))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            ids.push(stem.to_string());
        }
    }
    ids.sort();
    Ok(ids)
}

//! Top-level rig operations: `up`, `check`, `down`, `status`, `snapshot`.
//!
//! Each function returns a report struct that the CLI layer serializes to
//! JSON. Reports are the contract — they should be stable across minor
//! homeboy versions.

use std::collections::BTreeMap;

use serde::Serialize;

use super::expand::expand_vars;
use super::pipeline::{cleanup_shared_paths, run_pipeline, PipelineOutcome};
use super::service::{self, ServiceStatus};
use super::spec::{RigSpec, ServiceKind};
use super::state::{now_rfc3339, RigState};
use crate::engine::command::run_in_optional;
use crate::error::Result;

/// Report from `rig up`.
#[derive(Debug, Clone, Serialize)]
pub struct UpReport {
    pub rig_id: String,
    pub pipeline: PipelineOutcome,
    pub success: bool,
}

/// Report from `rig check`.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub rig_id: String,
    pub pipeline: PipelineOutcome,
    pub success: bool,
}

/// Report from `rig down`.
#[derive(Debug, Clone, Serialize)]
pub struct DownReport {
    pub rig_id: String,
    pub stopped: Vec<String>,
    pub pipeline: Option<PipelineOutcome>,
    pub success: bool,
}

/// Report from `rig status`.
#[derive(Debug, Clone, Serialize)]
pub struct RigStatusReport {
    pub rig_id: String,
    pub description: String,
    pub services: Vec<ServiceStatusReport>,
    pub last_up: Option<String>,
    pub last_check: Option<String>,
    pub last_check_result: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatusReport {
    pub id: String,
    pub kind: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub log_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

/// Materialize a rig: run the `up` pipeline, stash timestamp in state.
pub fn run_up(rig: &RigSpec) -> Result<UpReport> {
    let outcome = run_pipeline(rig, "up", true)?;

    if outcome.is_success() {
        let mut state = RigState::load(&rig.id)?;
        state.last_up = Some(now_rfc3339());
        state.save(&rig.id)?;
    }

    Ok(UpReport {
        rig_id: rig.id.clone(),
        success: outcome.is_success(),
        pipeline: outcome,
    })
}

/// Run the `check` pipeline. Unlike `up`, does NOT fail-fast — reports every
/// failing check so the user can fix them all in one pass.
pub fn run_check(rig: &RigSpec) -> Result<CheckReport> {
    let outcome = run_pipeline(rig, "check", false)?;

    let mut state = RigState::load(&rig.id)?;
    state.last_check = Some(now_rfc3339());
    state.last_check_result = Some(if outcome.is_success() { "pass" } else { "fail" }.to_string());
    state.save(&rig.id)?;

    Ok(CheckReport {
        rig_id: rig.id.clone(),
        success: outcome.is_success(),
        pipeline: outcome,
    })
}

/// Tear down a rig. Runs the `down` pipeline if defined, then stops every
/// service the rig knows about (belt + suspenders — spec authors sometimes
/// forget to add `service stop` steps to `down`).
pub fn run_down(rig: &RigSpec) -> Result<DownReport> {
    let pipeline = if rig.pipeline.contains_key("down") {
        Some(run_pipeline(rig, "down", false)?)
    } else {
        None
    };

    cleanup_shared_paths(rig)?;

    let mut stopped = Vec::new();
    for service_id in rig.services.keys() {
        service::stop(rig, service_id)?;
        stopped.push(service_id.clone());
    }
    stopped.sort();

    let success = pipeline.as_ref().is_none_or(|p| p.is_success());
    Ok(DownReport {
        rig_id: rig.id.clone(),
        stopped,
        pipeline,
        success,
    })
}

/// Summarize current rig state (no mutations).
pub fn run_status(rig: &RigSpec) -> Result<RigStatusReport> {
    let state = RigState::load(&rig.id)?;
    let mut services = Vec::with_capacity(rig.services.len());

    for (id, spec) in &rig.services {
        let live = service::status(&rig.id, id)?;
        let (status_str, pid) = match live {
            ServiceStatus::Running(pid) => ("running", Some(pid)),
            ServiceStatus::Stopped => ("stopped", None),
            ServiceStatus::Stale(pid) => ("stale", Some(pid)),
        };
        let started_at = state.services.get(id).and_then(|s| s.started_at.clone());
        let log_path = service::log_path(&rig.id, id)?
            .to_string_lossy()
            .into_owned();
        services.push(ServiceStatusReport {
            id: id.clone(),
            kind: service_kind_label(spec.kind).to_string(),
            status: status_str.to_string(),
            pid,
            port: spec.port,
            log_path,
            started_at,
        });
    }
    services.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(RigStatusReport {
        rig_id: rig.id.clone(),
        description: rig.description.clone(),
        services,
        last_up: state.last_up,
        last_check: state.last_check,
        last_check_result: state.last_check_result,
    })
}

fn service_kind_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::HttpStatic => "http-static",
        ServiceKind::Command => "command",
        ServiceKind::External => "external",
    }
}

/// Captured component state for one entry in a rig's components map.
///
/// Captured at the start of every `homeboy rig bench` run so bench results
/// can be tagged with the exact code state they were measured against.
/// Without this, bench-to-bench comparisons can't distinguish "the code got
/// slower" from "I'm comparing against a different commit." Surfaced in
/// the bench command output alongside the bench result; persisting into
/// the baseline JSON is a follow-up (see Extra-Chill/homeboy#1466 docs).
#[derive(Debug, Clone, Serialize)]
pub struct ComponentSnapshot {
    /// Resolved filesystem path (after `~` / `${env.X}` / `${components.X}`
    /// expansion). Useful for humans reviewing a snapshot offline.
    pub path: String,
    /// `git rev-parse HEAD` for the path's repo. `None` if the path is not
    /// a git repo or the command fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// `git rev-parse --abbrev-ref HEAD` — current branch name, or `HEAD`
    /// for detached. `None` if the path is not a git repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Snapshot of every component in a rig at a moment in time. Sorted by
/// component ID for stable output (BTreeMap).
#[derive(Debug, Clone, Serialize)]
pub struct RigStateSnapshot {
    pub rig_id: String,
    pub captured_at: String,
    pub components: BTreeMap<String, ComponentSnapshot>,
}

/// Capture the current state of every component in a rig.
///
/// Resolves each `ComponentSpec.path` (with `${env.X}` / `${components.X}`
/// / `~` expansion), then queries git for HEAD SHA and current branch.
/// Components whose paths aren't git repos are still included with `sha`
/// / `branch` set to `None` — bench results should still record they were
/// part of the rig at measurement time.
pub fn snapshot_state(rig: &RigSpec) -> RigStateSnapshot {
    let mut components = BTreeMap::new();
    for (id, comp) in &rig.components {
        let expanded = expand_vars(rig, &comp.path);
        let resolved = shellexpand::tilde(&expanded).into_owned();
        let sha = run_in_optional(&resolved, "git", &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let branch = run_in_optional(&resolved, "git", &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        components.insert(
            id.clone(),
            ComponentSnapshot {
                path: resolved,
                sha,
                branch,
            },
        );
    }
    RigStateSnapshot {
        rig_id: rig.id.clone(),
        captured_at: now_rfc3339(),
        components,
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/runner_test.rs"]
mod runner_test;

//! Top-level rig operations: `up`, `check`, `down`, `status`.
//!
//! Each function returns a report struct that the CLI layer serializes to
//! JSON. Reports are the contract — they should be stable across minor
//! homeboy versions.

use serde::Serialize;

use super::pipeline::{run_pipeline, PipelineOutcome};
use super::service::{self, ServiceStatus};
use super::spec::RigSpec;
use super::state::{now_rfc3339, RigState};
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
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
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
    state.last_check_result = Some(
        if outcome.is_success() {
            "pass"
        } else {
            "fail"
        }
        .to_string(),
    );
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

    for id in rig.services.keys() {
        let live = service::status(&rig.id, id)?;
        let (status_str, pid) = match live {
            ServiceStatus::Running(pid) => ("running", Some(pid)),
            ServiceStatus::Stopped => ("stopped", None),
            ServiceStatus::Stale(pid) => ("stale", Some(pid)),
        };
        let started_at = state
            .services
            .get(id)
            .and_then(|s| s.started_at.clone());
        services.push(ServiceStatusReport {
            id: id.clone(),
            status: status_str.to_string(),
            pid,
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

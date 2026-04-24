//! Linear pipeline executor.
//!
//! MVP is a straight for-loop over `PipelineStep`s. DAG + caching is
//! explicitly Phase 3 (tracked in Extra-Chill/homeboy#1464).
//!
//! Every step emits a `PipelineStepOutcome`. The runner aggregates them into
//! a `PipelineOutcome` with overall success/failure.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;

use super::check;
use super::expand::expand_vars;
use super::service;
use super::spec::{PipelineStep, RigSpec, ServiceOp, SymlinkOp, SymlinkSpec};
use crate::error::{Error, Result};

/// Result of one pipeline step.
#[derive(Debug, Clone, Serialize)]
pub struct PipelineStepOutcome {
    /// Step kind (`service`, `command`, `symlink`, `check`).
    pub kind: String,
    /// Human-readable label for the step.
    pub label: String,
    /// `"pass"`, `"fail"`, or `"skip"`.
    pub status: String,
    /// Error message when `status = "fail"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregate result of a pipeline run.
#[derive(Debug, Clone, Serialize)]
pub struct PipelineOutcome {
    pub name: String,
    pub steps: Vec<PipelineStepOutcome>,
    pub passed: usize,
    pub failed: usize,
}

impl PipelineOutcome {
    pub fn is_success(&self) -> bool {
        self.failed == 0
    }
}

/// Run a named pipeline from a rig spec.
///
/// `fail_fast = true` means the pipeline aborts on first failure (good for
/// `up` — later steps usually depend on earlier ones). `fail_fast = false`
/// runs every step (good for `check` — report everything wrong at once).
pub fn run_pipeline(rig: &RigSpec, name: &str, fail_fast: bool) -> Result<PipelineOutcome> {
    let steps = rig.pipeline.get(name).cloned().unwrap_or_default();
    let mut outcomes = Vec::with_capacity(steps.len());
    let mut failed = 0;
    let mut passed = 0;
    let mut aborted = false;

    for (idx, step) in steps.iter().enumerate() {
        if aborted {
            outcomes.push(PipelineStepOutcome {
                kind: step_kind(step).to_string(),
                label: step_label(rig, step, idx),
                status: "skip".to_string(),
                error: None,
            });
            continue;
        }

        let label = step_label(rig, step, idx);
        crate::log_status!("rig", "{}: {}", name, label);

        let result = run_step(rig, step);

        let outcome = match &result {
            Ok(()) => PipelineStepOutcome {
                kind: step_kind(step).to_string(),
                label: label.clone(),
                status: "pass".to_string(),
                error: None,
            },
            Err(e) => PipelineStepOutcome {
                kind: step_kind(step).to_string(),
                label: label.clone(),
                status: "fail".to_string(),
                error: Some(e.to_string()),
            },
        };

        match &result {
            Ok(()) => passed += 1,
            Err(_) => {
                failed += 1;
                if fail_fast {
                    aborted = true;
                }
            }
        }

        outcomes.push(outcome);
    }

    Ok(PipelineOutcome {
        name: name.to_string(),
        steps: outcomes,
        passed,
        failed,
    })
}

fn run_step(rig: &RigSpec, step: &PipelineStep) -> Result<()> {
    match step {
        PipelineStep::Service { id, op } => run_service_step(rig, id, *op),
        PipelineStep::Command {
            cmd,
            cwd,
            env,
            label: _,
        } => run_command_step(rig, cmd, cwd.as_deref(), env),
        PipelineStep::Symlink { op } => run_symlink_step(rig, *op),
        PipelineStep::Check { spec, .. } => check::evaluate(rig, spec),
    }
}

fn run_service_step(rig: &RigSpec, service_id: &str, op: ServiceOp) -> Result<()> {
    match op {
        ServiceOp::Start => {
            service::start(rig, service_id)?;
            Ok(())
        }
        ServiceOp::Stop => service::stop(rig, service_id),
        ServiceOp::Health => {
            let spec = rig.services.get(service_id).ok_or_else(|| {
                Error::rig_service_failed(
                    &rig.id,
                    service_id,
                    "service not declared in rig spec",
                )
            })?;
            if let Some(health) = &spec.health {
                check::evaluate(rig, health)?;
            }
            match service::status(&rig.id, service_id)? {
                service::ServiceStatus::Running(_) => Ok(()),
                service::ServiceStatus::Stopped => Err(Error::rig_service_failed(
                    &rig.id,
                    service_id,
                    "service is stopped",
                )),
                service::ServiceStatus::Stale(pid) => Err(Error::rig_service_failed(
                    &rig.id,
                    service_id,
                    format!("recorded PID {} is not alive", pid),
                )),
            }
        }
    }
}

fn run_command_step(
    rig: &RigSpec,
    cmd: &str,
    cwd: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<()> {
    let expanded = expand_vars(rig, cmd);
    let mut command = Command::new("sh");
    command.arg("-c").arg(&expanded);

    if let Some(cwd) = cwd {
        let resolved = expand_vars(rig, cwd);
        command.current_dir(PathBuf::from(resolved));
    }
    for (k, v) in env {
        command.env(k, expand_vars(rig, v));
    }

    let status = command.status().map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "command",
            format!("spawn failed for `{}`: {}", expanded, e),
        )
    })?;

    if !status.success() {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "command",
            format!(
                "`{}` exited {}",
                expanded,
                status.code().unwrap_or(-1)
            ),
        ));
    }
    Ok(())
}

fn run_symlink_step(rig: &RigSpec, op: SymlinkOp) -> Result<()> {
    for link in &rig.symlinks {
        match op {
            SymlinkOp::Ensure => ensure_symlink(rig, link)?,
            SymlinkOp::Verify => verify_symlink(rig, link)?,
        }
    }
    Ok(())
}

fn ensure_symlink(rig: &RigSpec, link: &SymlinkSpec) -> Result<()> {
    let link_path = PathBuf::from(expand_vars(rig, &link.link));
    let target_path = PathBuf::from(expand_vars(rig, &link.target));

    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::rig_pipeline_failed(
                &rig.id,
                "symlink",
                format!("create parent of {}: {}", link_path.display(), e),
            )
        })?;
    }

    // If the link exists pointing at the right target, no-op.
    if link_path.exists() || link_path.is_symlink() {
        if let Ok(current) = std::fs::read_link(&link_path) {
            if current == target_path {
                return Ok(());
            }
        }
        std::fs::remove_file(&link_path).map_err(|e| {
            Error::rig_pipeline_failed(
                &rig.id,
                "symlink",
                format!("remove existing {}: {}", link_path.display(), e),
            )
        })?;
    }

    std::os::unix::fs::symlink(&target_path, &link_path).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!(
                "create {} → {}: {}",
                link_path.display(),
                target_path.display(),
                e
            ),
        )
    })?;
    Ok(())
}

fn verify_symlink(rig: &RigSpec, link: &SymlinkSpec) -> Result<()> {
    let link_path = PathBuf::from(expand_vars(rig, &link.link));
    let target_path = PathBuf::from(expand_vars(rig, &link.target));

    if !link_path.is_symlink() {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!("{} is not a symlink", link_path.display()),
        ));
    }
    let current = std::fs::read_link(&link_path).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!("read {}: {}", link_path.display(), e),
        )
    })?;
    if current != target_path {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!(
                "{} points at {}, expected {}",
                link_path.display(),
                current.display(),
                target_path.display()
            ),
        ));
    }
    Ok(())
}

fn step_kind(step: &PipelineStep) -> &'static str {
    match step {
        PipelineStep::Service { .. } => "service",
        PipelineStep::Command { .. } => "command",
        PipelineStep::Symlink { .. } => "symlink",
        PipelineStep::Check { .. } => "check",
    }
}

fn step_label(rig: &RigSpec, step: &PipelineStep, idx: usize) -> String {
    match step {
        PipelineStep::Service { id, op } => format!("service {} {}", id, serialize_op(*op)),
        PipelineStep::Command { cmd, label, .. } => label
            .clone()
            .unwrap_or_else(|| truncate(&expand_vars(rig, cmd), 80)),
        PipelineStep::Symlink { op } => format!("symlink {}", serialize_symlink_op(*op)),
        PipelineStep::Check { label, .. } => {
            label.clone().unwrap_or_else(|| format!("check #{}", idx + 1))
        }
    }
}

fn serialize_op(op: ServiceOp) -> &'static str {
    match op {
        ServiceOp::Start => "start",
        ServiceOp::Stop => "stop",
        ServiceOp::Health => "health",
    }
}

fn serialize_symlink_op(op: SymlinkOp) -> &'static str {
    match op {
        SymlinkOp::Ensure => "ensure",
        SymlinkOp::Verify => "verify",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

//! Safe reconciliation of stale/suspect/unreconciled active agent-task runs.
//! Pure move out of the former `agent_task_service.rs` god-file.

use crate::core::agent_task_lifecycle;
use crate::core::Result;

use super::discovery::{discover_runs, AgentTaskDiscoveryFilter, AgentTaskLiveness};

/// Report returned by [`reconcile_stale_active_runs`]. Lists every active run
/// that was classified non-active, and for the reconcilable ones records the
/// outcome of the safe cancel attempt.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskReconcileReport {
    pub schema: &'static str,
    /// `true` when no records were actually mutated (preview mode).
    pub dry_run: bool,
    pub considered: usize,
    pub reconciled: usize,
    pub failed: usize,
    pub runs: Vec<AgentTaskReconcileRun>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskReconcileRun {
    pub run_id: String,
    pub liveness: AgentTaskLiveness,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    /// `reconciled`, `would-reconcile` (dry run), or `failed`.
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Safely reconcile stale/suspect/unreconciled active runs without manual state
/// edits (#5682). Each candidate is cancelled through the lifecycle cancel path,
/// which terminates a still-live owner process tree only when one actually
/// exists and otherwise just marks the orphaned `running` record cancelled —
/// the exact safe operation an operator would otherwise be tempted to do by
/// hand-editing run JSON.
///
/// Genuinely-active runs (live owner/runner with a fresh heartbeat, or queued
/// work) are never touched. With `dry_run`, candidates are reported but no
/// record is mutated so an operator can preview the blast radius first.
pub fn reconcile_stale_active_runs(dry_run: bool) -> Result<AgentTaskReconcileReport> {
    let report = discover_runs(AgentTaskDiscoveryFilter::Active)?;

    let mut runs = Vec::new();
    let mut reconciled = 0usize;
    let mut failed = 0usize;

    for run in report.runs {
        let Some(liveness) = run.liveness else {
            continue;
        };
        if !liveness.is_reconcilable() {
            continue;
        }

        if dry_run {
            runs.push(AgentTaskReconcileRun {
                run_id: run.run_id,
                liveness,
                source: run.source,
                stale_reason: run.stale_reason,
                action: "would-reconcile",
                error: None,
            });
            continue;
        }

        let reason = run
            .stale_reason
            .clone()
            .unwrap_or_else(|| format!("reconciled stale-{} run", liveness.as_str()));
        match agent_task_lifecycle::cancel_run(&run.run_id, Some(&reason)) {
            Ok(_) => {
                reconciled += 1;
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    stale_reason: run.stale_reason,
                    action: "reconciled",
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    stale_reason: run.stale_reason,
                    action: "failed",
                    error: Some(error.message),
                });
            }
        }
    }

    Ok(AgentTaskReconcileReport {
        schema: "homeboy/agent-task-reconcile/v1",
        dry_run,
        considered: runs.len(),
        reconciled,
        failed,
        runs,
    })
}

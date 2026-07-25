//! Safe reconciliation of stale/suspect/unreconciled active agent-task runs.
//! Pure move out of the former `agent_task_service.rs` god-file.

use crate::agent_task_lifecycle;
use homeboy_core::Result;

use super::discovery::{discover_runs, AgentTaskDiscoveryFilter, AgentTaskLiveness};

/// Report returned by [`reconcile_stale_active_runs`]. Lists every active run
/// that was classified non-active, and for the reconcilable ones records the
/// outcome of the safe cancel attempt.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskReconcileReport {
    pub schema: &'static str,
    /// `fleet` for the explicit bulk operation or `run:<id>` for a scoped repair.
    pub scope: String,
    /// `preview` unless the caller supplied explicit `--apply` authorization.
    pub authorization: &'static str,
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
    /// State observed from the durable record after its runner/provider refresh.
    pub authoritative_state: agent_task_lifecycle::AgentTaskRunState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    /// `reconciled`, `would-reconcile` (preview), `no-op`, or `failed`.
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
    // Read the durable snapshot first so an expired controller handoff remains
    // visible to this managed recovery command. `status` also converges expiry,
    // but would otherwise terminalize it before this report can record the
    // actionable retry outcome.
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

        // A runner-backed record is only a local projection. Refresh it from
        // the daemon before treating a dead controller owner as authority: the
        // daemon may still be active or may have already published the terminal
        // aggregate and artifacts while the controller caller was gone.
        if run.runner_id.is_some() && run.runner_job_id.is_some() {
            match agent_task_lifecycle::status(&run.run_id) {
                Ok(refreshed) if refreshed.state.is_terminal() => continue,
                Ok(refreshed) if !refreshed.is_stale_running() => continue,
                Ok(_) => {}
                Err(error) => {
                    failed += 1;
                    runs.push(AgentTaskReconcileRun {
                        run_id: run.run_id,
                        liveness,
                        source: run.source,
                        authoritative_state: agent_task_lifecycle::AgentTaskRunState::Running,
                        stale_reason: run.stale_reason,
                        action: "failed",
                        error: Some(error.message),
                    });
                    continue;
                }
            }
        }

        let expired_handoff =
            agent_task_lifecycle::has_expired_unaccepted_lab_handoff(&run.run_id)?;
        if dry_run {
            runs.push(AgentTaskReconcileRun {
                run_id: run.run_id,
                liveness,
                source: run.source,
                authoritative_state: agent_task_lifecycle::AgentTaskRunState::Running,
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
        let result = if expired_handoff {
            agent_task_lifecycle::expire_unaccepted_lab_handoff(&run.run_id).map(|_| ())
        } else {
            agent_task_lifecycle::cancel_run(&run.run_id, Some(&reason)).map(|_| ())
        };
        match result {
            Ok(_) => {
                reconciled += 1;
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    authoritative_state: agent_task_lifecycle::AgentTaskRunState::Running,
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
                    authoritative_state: agent_task_lifecycle::AgentTaskRunState::Running,
                    stale_reason: run.stale_reason,
                    action: "failed",
                    error: Some(error.message),
                });
            }
        }
    }

    Ok(AgentTaskReconcileReport {
        schema: "homeboy/agent-task-reconcile/v1",
        scope: "fleet".to_string(),
        authorization: if dry_run { "preview" } else { "explicit-apply" },
        dry_run,
        considered: runs.len(),
        reconciled,
        failed,
        runs,
    })
}

/// Reconcile one durable run after refreshing its authoritative lifecycle and
/// runner projection. This is intentionally a separate operation from fleet
/// reconciliation: a state or ownership change becomes a no-op rather than a
/// reason to inspect or mutate any other record (#10001).
pub fn reconcile_run(run_id: &str, dry_run: bool) -> Result<AgentTaskReconcileReport> {
    let authoritative = agent_task_lifecycle::status(run_id)?;
    let resolved_run_id = authoritative.run_id.clone();
    let source = authoritative
        .runner_id()
        .map(|runner_id| format!("runner:{runner_id}"))
        .unwrap_or_else(|| "local".to_string());
    let candidate = discover_runs(AgentTaskDiscoveryFilter::Active)?
        .runs
        .into_iter()
        .find(|run| run.run_id == resolved_run_id);

    let mut runs = Vec::new();
    let mut reconciled = 0;
    let mut failed = 0;
    match candidate {
        Some(run) if run.liveness.is_some_and(AgentTaskLiveness::is_reconcilable) => {
            let liveness = run.liveness.expect("checked reconcilable liveness");
            // Re-read immediately before apply. A newly-live runner or a changed
            // owner is authoritative and turns this scoped request into a no-op.
            let refreshed = agent_task_lifecycle::status(&resolved_run_id)?;
            let still_reconcilable = discover_runs(AgentTaskDiscoveryFilter::Active)?
                .runs
                .into_iter()
                .find(|run| run.run_id == resolved_run_id)
                .and_then(|run| run.liveness)
                .is_some_and(AgentTaskLiveness::is_reconcilable);
            if refreshed.state.is_terminal() || !still_reconcilable {
                runs.push(AgentTaskReconcileRun {
                    run_id: resolved_run_id,
                    liveness,
                    source: run.source,
                    authoritative_state: refreshed.state,
                    stale_reason: run.stale_reason,
                    action: "no-op",
                    error: None,
                });
            } else if dry_run {
                runs.push(AgentTaskReconcileRun {
                    run_id: resolved_run_id,
                    liveness,
                    source: run.source,
                    authoritative_state: refreshed.state,
                    stale_reason: run.stale_reason,
                    action: "would-reconcile",
                    error: None,
                });
            } else {
                let reason = run
                    .stale_reason
                    .clone()
                    .unwrap_or_else(|| format!("reconciled stale-{} run", liveness.as_str()));
                match agent_task_lifecycle::cancel_run(&resolved_run_id, Some(&reason)) {
                    Ok(record) => {
                        reconciled += 1;
                        runs.push(AgentTaskReconcileRun {
                            run_id: resolved_run_id,
                            liveness,
                            source: run.source,
                            authoritative_state: record.state,
                            stale_reason: run.stale_reason,
                            action: "reconciled",
                            error: None,
                        });
                    }
                    Err(error) => {
                        failed += 1;
                        runs.push(AgentTaskReconcileRun {
                            run_id: resolved_run_id,
                            liveness,
                            source: run.source,
                            authoritative_state: refreshed.state,
                            stale_reason: run.stale_reason,
                            action: "failed",
                            error: Some(error.message),
                        });
                    }
                }
            }
        }
        Some(run) => runs.push(AgentTaskReconcileRun {
            run_id: resolved_run_id,
            liveness: run.liveness.unwrap_or(AgentTaskLiveness::Active),
            source: run.source,
            authoritative_state: authoritative.state,
            stale_reason: run.stale_reason,
            action: "no-op",
            error: None,
        }),
        None => runs.push(AgentTaskReconcileRun {
            run_id: resolved_run_id,
            liveness: AgentTaskLiveness::Active,
            source,
            authoritative_state: authoritative.state,
            stale_reason: None,
            action: "no-op",
            error: None,
        }),
    }

    Ok(AgentTaskReconcileReport {
        schema: "homeboy/agent-task-reconcile/v1",
        scope: format!("run:{}", authoritative.run_id),
        authorization: if dry_run { "preview" } else { "explicit-apply" },
        dry_run,
        considered: runs.iter().filter(|run| run.action != "no-op").count(),
        reconciled,
        failed,
        runs,
    })
}

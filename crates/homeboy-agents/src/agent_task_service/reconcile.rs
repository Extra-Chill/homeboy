//! Safe reconciliation of stale/suspect/unreconciled active agent-task runs.
//! Pure move out of the former `agent_task_service.rs` god-file.
//!
//! Also hosts the agent-task side of the daemon's orchestration tick. The
//! daemon lives in `homeboy-core`, which must not depend on this subsystem, so
//! the tick dispatches through a registered driver — see
//! [`register_orchestration_driver`].

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
    /// The identifier supplied by the caller. It remains distinct from a Cook
    /// alias's exact attempt ids so recovery output cannot hide what was asked.
    pub requested_run_id: Option<String>,
    /// Exact durable records inspected by a scoped operation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resolved_run_ids: Vec<String>,
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
/// [`agent_task_lifecycle::status`] against an explicitly injected root.
fn rooted_status(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    Ok(agent_task_lifecycle::status_in_store(
        lifecycle_store,
        run_id,
        agent_task_lifecycle::AgentTaskStatusOptions::default(),
        false,
    )?
    .record)
}

/// [`agent_task_lifecycle::exact_status`] against an explicitly injected root.
fn rooted_exact_status(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    Ok(agent_task_lifecycle::status_in_store(
        lifecycle_store,
        run_id,
        agent_task_lifecycle::AgentTaskStatusOptions::default(),
        true,
    )?
    .record)
}

pub fn reconcile_stale_active_runs(dry_run: bool) -> Result<AgentTaskReconcileReport> {
    // One store for the whole sweep. Every run this classifies, expires, and
    // cancels is decided from a record read here; a decision taken against one
    // installation and committed into another cancels a run it never saw
    // (#7505).
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
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

        // The best state known so far. Every construction site below reports
        // the freshest state it actually has rather than the literal `Running`
        // it used to hardcode — which made this report untrustworthy at
        // exactly the point an operator relies on it, because the `reconciled`
        // branch claimed `Running` for a run it had just cancelled.
        let mut authoritative_state = run.state;

        // A runner-backed record is only a local projection. Refresh it from
        // the daemon before treating a dead controller owner as authority: the
        // daemon may still be active or may have already published the terminal
        // aggregate and artifacts while the controller caller was gone.
        if run.runner_id.is_some() && run.runner_job_id.is_some() {
            match rooted_status(&lifecycle_store, &run.run_id) {
                Ok(refreshed) if refreshed.state.is_terminal() => continue,
                Ok(refreshed) if !refreshed.is_stale_running() => continue,
                Ok(refreshed) => authoritative_state = refreshed.state,
                Err(error) => {
                    failed += 1;
                    runs.push(AgentTaskReconcileRun {
                        run_id: run.run_id.clone(),
                        liveness,
                        source: run.source,
                        // The refresh is what failed, so the discovery
                        // snapshot is the most authoritative state available.
                        authoritative_state,
                        stale_reason: run.stale_reason,
                        action: "failed",
                        error: Some(error.message),
                    });
                    continue;
                }
            }
        }

        let expired_handoff = agent_task_lifecycle::has_expired_unaccepted_lab_handoff_in_store(
            &lifecycle_store,
            &run.run_id,
        )?;
        let record = agent_task_lifecycle::exact_record_in_store(&lifecycle_store, &run.run_id)?;
        let expired_detached_admission =
            agent_task_lifecycle::has_expired_detached_cook_admission(&record, chrono::Utc::now());
        if dry_run {
            runs.push(AgentTaskReconcileRun {
                run_id: run.run_id,
                liveness,
                source: run.source,
                // Preview mutates nothing, so the observed state is the state.
                authoritative_state,
                stale_reason: run.stale_reason,
                action: "would-reconcile",
                error: None,
            });
            continue;
        }
        if expired_detached_admission {
            match agent_task_lifecycle::expire_detached_cook_admission_in_store(
                &lifecycle_store,
                &run.run_id,
            ) {
                Ok(true) => {
                    reconciled += 1;
                    runs.push(AgentTaskReconcileRun {
                        run_id: run.run_id.clone(),
                        liveness,
                        source: run.source,
                        authoritative_state: rooted_status(&lifecycle_store, &run.run_id)?.state,
                        stale_reason: run.stale_reason,
                        action: "reconciled",
                        error: None,
                    });
                }
                Ok(false) => runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id.clone(),
                    liveness,
                    source: run.source,
                    authoritative_state: rooted_status(&lifecycle_store, &run.run_id)?.state,
                    stale_reason: run.stale_reason,
                    action: "no-op",
                    error: None,
                }),
                Err(error) => {
                    failed += 1;
                    runs.push(AgentTaskReconcileRun {
                        run_id: run.run_id,
                        liveness,
                        source: run.source,
                        authoritative_state,
                        stale_reason: run.stale_reason,
                        action: "failed",
                        error: Some(error.message),
                    });
                }
            }
            continue;
        }

        let reason = run
            .stale_reason
            .clone()
            .unwrap_or_else(|| format!("reconciled stale-{} run", liveness.as_str()));
        let result = if expired_handoff {
            // Handoff expiry answers whether it expired, not what state that
            // left behind; re-read the record for the post-mutation state.
            agent_task_lifecycle::expire_unaccepted_lab_handoff_in_store(
                &lifecycle_store,
                &run.run_id,
            )
            .map(|_| {
                rooted_status(&lifecycle_store, &run.run_id)
                    .map(|record| record.state)
                    .unwrap_or(authoritative_state)
            })
        } else {
            agent_task_lifecycle::cancel_run_in_store(&lifecycle_store, &run.run_id, Some(&reason))
                .map(|record| record.state)
        };
        match result {
            Ok(state) => {
                reconciled += 1;
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    // The state the reconcile produced — `Cancelled` for a
                    // cancel, the expiry's terminal state for a handoff — not
                    // the `Running` it no longer is.
                    authoritative_state: state,
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
                    authoritative_state,
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
        requested_run_id: None,
        resolved_run_ids: Vec::new(),
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
    // One store for the whole reconciliation: the scope resolution, every
    // authoritative read, and the expiry or cancellation that follows are one
    // answer about one run.
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let requested_run_id = run_id.to_string();
    let resolved_run_ids =
        agent_task_lifecycle::reconcile_scope_run_ids_in_store(&lifecycle_store, run_id)?;
    let mut runs = Vec::new();
    let mut reconciled = 0;
    let mut failed = 0;
    for resolved_run_id in &resolved_run_ids {
        let authoritative = rooted_exact_status(&lifecycle_store, resolved_run_id)?;
        let source = authoritative
            .runner_id()
            .map(|runner_id| format!("runner:{runner_id}"))
            .unwrap_or_else(|| "local".to_string());
        let candidate = discover_runs(AgentTaskDiscoveryFilter::Active)?
            .runs
            .into_iter()
            .find(|run| run.run_id == *resolved_run_id);
        match candidate {
            Some(run) if run.liveness.is_some_and(AgentTaskLiveness::is_reconcilable) => {
                let liveness = run.liveness.expect("checked reconcilable liveness");
                // Re-read immediately before apply. A newly-live runner or a changed
                // owner is authoritative and turns this scoped request into a no-op.
                let refreshed = rooted_exact_status(&lifecycle_store, resolved_run_id)?;
                let still_reconcilable = discover_runs(AgentTaskDiscoveryFilter::Active)?
                    .runs
                    .into_iter()
                    .find(|run| run.run_id == *resolved_run_id)
                    .and_then(|run| run.liveness)
                    .is_some_and(AgentTaskLiveness::is_reconcilable);
                if refreshed.state.is_terminal() || !still_reconcilable {
                    runs.push(AgentTaskReconcileRun {
                        run_id: resolved_run_id.clone(),
                        liveness,
                        source: run.source,
                        authoritative_state: refreshed.state,
                        stale_reason: run.stale_reason,
                        action: "no-op",
                        error: None,
                    });
                } else if refreshed.runner_id().is_some()
                    && refreshed.runner_job_id().is_some()
                    && refreshed.runner_id().is_some_and(|runner_id| {
                        agent_task_lifecycle::runner_authority(runner_id)
                            != agent_task_lifecycle::RunnerAuthority::Removed
                    })
                {
                    // An accepted remote handoff remains runner-owned unless its
                    // provider authoritatively confirms the runner was removed.
                    runs.push(AgentTaskReconcileRun {
                        run_id: resolved_run_id.clone(),
                        liveness,
                        source: run.source,
                        authoritative_state: refreshed.state,
                        stale_reason: run.stale_reason,
                        action: "no-op",
                        error: None,
                    });
                } else if dry_run {
                    runs.push(AgentTaskReconcileRun {
                        run_id: resolved_run_id.clone(),
                        liveness,
                        source: run.source,
                        authoritative_state: refreshed.state,
                        stale_reason: run.stale_reason,
                        action: "would-reconcile",
                        error: None,
                    });
                } else {
                    if let Some(runner_id) = refreshed.runner_id() {
                        if agent_task_lifecycle::runner_authority(runner_id)
                            != agent_task_lifecycle::RunnerAuthority::Removed
                        {
                            failed += 1;
                            runs.push(AgentTaskReconcileRun {
                                run_id: resolved_run_id.clone(),
                                liveness,
                                source: run.source,
                                authoritative_state: refreshed.state,
                                stale_reason: run.stale_reason,
                                action: "failed",
                                error: Some(format!(
                                    "runner `{runner_id}` ownership is not authoritatively removed; reconcile it through homeboy runner reconcile {runner_id}"
                                )),
                            });
                            continue;
                        }
                    }
                    let expired_detached_admission =
                        agent_task_lifecycle::has_expired_detached_cook_admission(
                            &refreshed,
                            chrono::Utc::now(),
                        );
                    if expired_detached_admission {
                        match agent_task_lifecycle::expire_detached_cook_admission_in_store(
                            &lifecycle_store,
                            resolved_run_id,
                        ) {
                            Ok(true) => {
                                reconciled += 1;
                                runs.push(AgentTaskReconcileRun {
                                    run_id: resolved_run_id.clone(),
                                    liveness,
                                    source: run.source,
                                    authoritative_state: rooted_status(
                                        &lifecycle_store,
                                        resolved_run_id,
                                    )?
                                    .state,
                                    stale_reason: run.stale_reason,
                                    action: "reconciled",
                                    error: None,
                                });
                            }
                            Ok(false) => runs.push(AgentTaskReconcileRun {
                                run_id: resolved_run_id.clone(),
                                liveness,
                                source: run.source,
                                authoritative_state: rooted_status(
                                    &lifecycle_store,
                                    resolved_run_id,
                                )?
                                .state,
                                stale_reason: run.stale_reason,
                                action: "no-op",
                                error: None,
                            }),
                            Err(error) => {
                                failed += 1;
                                runs.push(AgentTaskReconcileRun {
                                    run_id: resolved_run_id.clone(),
                                    liveness,
                                    source: run.source,
                                    authoritative_state: refreshed.state,
                                    stale_reason: run.stale_reason,
                                    action: "failed",
                                    error: Some(error.message),
                                });
                            }
                        }
                        continue;
                    }
                    let reason = run
                        .stale_reason
                        .clone()
                        .unwrap_or_else(|| format!("reconciled stale-{} run", liveness.as_str()));
                    match agent_task_lifecycle::cancel_exact_run_in_store(
                        &lifecycle_store,
                        resolved_run_id,
                        Some(&reason),
                    ) {
                        Ok(record) => {
                            reconciled += 1;
                            runs.push(AgentTaskReconcileRun {
                                run_id: resolved_run_id.clone(),
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
                                run_id: resolved_run_id.clone(),
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
                run_id: resolved_run_id.clone(),
                liveness: run.liveness.unwrap_or(AgentTaskLiveness::Active),
                source: run.source,
                authoritative_state: authoritative.state,
                stale_reason: run.stale_reason,
                action: "no-op",
                error: None,
            }),
            None => runs.push(AgentTaskReconcileRun {
                run_id: resolved_run_id.clone(),
                liveness: AgentTaskLiveness::Active,
                source,
                authoritative_state: authoritative.state,
                stale_reason: None,
                action: "no-op",
                error: None,
            }),
        }
    }

    Ok(AgentTaskReconcileReport {
        schema: "homeboy/agent-task-reconcile/v1",
        scope: format!("run:{requested_run_id}"),
        requested_run_id: Some(requested_run_id),
        resolved_run_ids,
        authorization: if dry_run { "preview" } else { "explicit-apply" },
        dry_run,
        considered: runs.iter().filter(|run| run.action != "no-op").count(),
        reconciled,
        failed,
        runs,
    })
}

// ---------------------------------------------------------------------------
// Daemon orchestration driver (W3-10)
// ---------------------------------------------------------------------------

/// The agent-task half of the daemon's orchestration tick.
///
/// `reconcile_stale_active_runs` used to have exactly two callers — `cleanup`
/// and `agent-task active --reconcile --apply` — both of which require a human
/// to type them. A detached cook whose owner died therefore stayed `running`
/// forever. Controller wait resolution had no automatic caller at all.
struct AgentTaskOrchestrationDriver;

impl homeboy_core::daemon::orchestration::OrchestrationDriver for AgentTaskOrchestrationDriver {
    fn reconcile_stale_active_runs(&self) -> Result<serde_json::Value> {
        let report = reconcile_stale_active_runs(false)?;
        // Announce what actually changed. A record that moved from `running`
        // to a terminal state without its owner noticing is the operator's
        // problem, and until the daemon drove this it had no producer at all.
        for run in &report.runs {
            if run.action != "reconciled" {
                continue;
            }
            crate::agent_task_notify::run_reconciled(
                &run.run_id,
                run_state_label(run.authoritative_state),
                run.liveness.as_str(),
                run.stale_reason.as_deref(),
            );
        }
        serde_json::to_value(&report)
            .map_err(|error| homeboy_core::Error::internal_json(error.to_string(), None))
    }

    fn reconcile_controller_waits(&self) -> Result<serde_json::Value> {
        let report = crate::agent_task_controller_service::reconcile_waiting_controllers()?;
        serde_json::to_value(&report)
            .map_err(|error| homeboy_core::Error::internal_json(error.to_string(), None))
    }
}

/// Snake_case labels for the run-state vocabulary, matching the enum's own
/// serde renaming so a notification reads the same as the JSON report.
fn run_state_label(state: agent_task_lifecycle::AgentTaskRunState) -> &'static str {
    use agent_task_lifecycle::AgentTaskRunState;
    match state {
        AgentTaskRunState::Queued => "queued",
        AgentTaskRunState::Running => "running",
        AgentTaskRunState::Succeeded => "succeeded",
        AgentTaskRunState::CandidateRecoverable => "candidate_recoverable",
        AgentTaskRunState::PartialRecoverable => "partial_recoverable",
        AgentTaskRunState::PartialFailure => "partial_failure",
        AgentTaskRunState::Failed => "failed",
        AgentTaskRunState::Cancelled => "cancelled",
    }
}

/// Register the agent-task orchestration driver with the daemon. Called once
/// at CLI startup; with no driver registered the daemon's orchestration tick
/// is inert rather than broken.
pub fn register_orchestration_driver() {
    homeboy_core::daemon::orchestration::register_orchestration_driver(std::sync::Arc::new(
        AgentTaskOrchestrationDriver,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_scheduler::AgentTaskPlan;
    use homeboy_core::test_support::with_isolated_home;

    /// A `running` record whose recorded owner pid does not exist, which is
    /// exactly the orphan shape a detached cook leaves when its owner dies.
    fn orphaned_running_run(run_id: &str) {
        let plan = AgentTaskPlan::new("reconcile-tick-plan", Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
        agent_task_lifecycle::mark_running(run_id).expect("running");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["runner_pid"] = serde_json::json!(999_999u32);
        })
        .expect("orphaned owner");
    }

    #[test]
    fn the_daemon_tick_reconciles_an_orphaned_running_record() {
        // Before the tick existed, `reconcile_stale_active_runs` had two
        // callers and both required a human: a detached cook whose owner died
        // stayed `running` forever.
        with_isolated_home(|_| {
            register_orchestration_driver();
            orphaned_running_run("reconcile-tick-orphan");

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("tick pass");

            assert_eq!(report["reconciled"], 1, "{report}");
            let refreshed =
                agent_task_lifecycle::status("reconcile-tick-orphan").expect("refreshed");
            assert!(
                refreshed.state.is_terminal(),
                "orphan must not stay running: {:?}",
                refreshed.state
            );
        });
    }

    #[test]
    fn the_daemon_tick_preserves_a_fresh_planned_runner_submission() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_id = "reconcile-tick-planned-runner";
            let plan = AgentTaskPlan::new("reconcile-tick-plan", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
            agent_task_lifecycle::record_lab_offload_phase(
                run_id,
                "homeboy-lab",
                "materializing",
                None,
                None,
                None,
                Some(&plan),
            )
            .expect("planned runner submission recorded");

            // The production daemon tick runs before the Lab can return the
            // accepted job identity. It must observe the materializing proxy's
            // fresh planned-execution transition, not cancel it as ownerless.
            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("tick pass");

            assert_eq!(report["reconciled"], 0, "{report}");
            let refreshed = agent_task_lifecycle::status(run_id).expect("refreshed");
            assert_eq!(
                refreshed.state,
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
            assert!(refreshed.metadata.get("stale_running").is_none());
            assert!(refreshed.metadata.get("cancel_reason").is_none());

            let accepted = agent_task_lifecycle::record_detached_lab_run(
                agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "homeboy-lab",
                    runner_job_id: "delayed-daemon-job",
                    remote_workspace: "/runner/workspace/homeboy",
                    remote_command: &[],
                },
            )
            .expect("delayed daemon identity bound");
            assert_eq!(accepted.runner_job_id(), Some("delayed-daemon-job"));

            let orphan_run_id = "reconcile-tick-ownerless-runner";
            orphaned_running_run(orphan_run_id);
            agent_task_lifecycle::rewrite_record_for_test(orphan_run_id, |record| {
                record
                    .metadata
                    .as_object_mut()
                    .expect("metadata object")
                    .remove("runner_pid");
            })
            .expect("owner identity removed");
            let stale = agent_task_lifecycle::status(orphan_run_id)
                .expect("PID-less owner is projected stale before daemon reconciliation");
            assert_eq!(stale.metadata["stale_running_reason"], "missing_runner_pid");

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("ownerless tick pass");
            assert_eq!(report["reconciled"], 1, "{report}");
            let terminal = agent_task_lifecycle::status(orphan_run_id).expect("ownerless run");
            assert_eq!(
                terminal.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(terminal.metadata["cancel_reason"], "missing_runner_pid");
        });
    }

    #[test]
    fn a_reconciled_run_reports_the_state_it_was_left_in() {
        // The `reconciled` branch used to hardcode `Running` — reporting the
        // state of a run it had just cancelled, which made the whole report
        // untrustworthy at the one moment an operator reads it.
        with_isolated_home(|_| {
            orphaned_running_run("reconcile-state-orphan");

            let report = reconcile_stale_active_runs(false).expect("reconcile");

            let run = report
                .runs
                .iter()
                .find(|run| run.run_id == "reconcile-state-orphan")
                .expect("reconciled run");
            assert_eq!(run.action, "reconciled");
            assert_ne!(
                run.authoritative_state,
                agent_task_lifecycle::AgentTaskRunState::Running,
                "a cancelled run must not report itself as running",
            );
            assert!(run.authoritative_state.is_terminal());
        });
    }

    #[test]
    fn pre_supervisor_detached_cook_admission_is_not_reconciled_as_stale_work() {
        with_isolated_home(|_| {
            let cook_id = "reconcile-pre-supervisor-cook";
            agent_task_lifecycle::record_detached_cook_handoff_parent(cook_id)
                .expect("persist detached admission");

            let report = reconcile_stale_active_runs(false).expect("reconcile admission state");

            assert!(
                report.runs.is_empty(),
                "a pre-supervisor admission is not an abandoned executor: {report:#?}"
            );
            let parent = agent_task_lifecycle::status(cook_id).expect("read durable admission");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["admission_state"],
                "pre_supervisor"
            );
        });
    }

    #[test]
    fn expired_unattached_and_legacy_detached_admissions_terminalize() {
        with_isolated_home(|_| {
            for (cook_id, legacy) in [
                ("expired-pre-supervisor", false),
                ("expired-legacy-pending", true),
            ] {
                agent_task_lifecycle::record_detached_cook_handoff_parent(cook_id)
                    .expect("persist detached admission");
                agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                    if legacy {
                        record.metadata["detached_cook_handoff"]
                            .as_object_mut()
                            .expect("handoff metadata")
                            .remove("admission_state");
                        record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
                        record.metadata["detached_cook_handoff"]
                            .as_object_mut()
                            .expect("handoff metadata")
                            .remove("admission_deadline_at");
                    } else {
                        record.metadata["detached_cook_handoff"]["admission_deadline_at"] =
                            serde_json::json!("2000-01-01T00:00:00+00:00");
                    }
                })
                .expect("expire admission lease");
            }

            let report = reconcile_stale_active_runs(false).expect("reconcile expired admissions");

            assert_eq!(report.reconciled, 2, "{report:#?}");
            for cook_id in ["expired-pre-supervisor", "expired-legacy-pending"] {
                let parent = agent_task_lifecycle::status(cook_id).expect("read terminal parent");
                assert_eq!(
                    parent.state,
                    agent_task_lifecycle::AgentTaskRunState::Failed
                );
                assert_eq!(
                    parent.metadata["detached_cook_handoff"]["admission_state"],
                    "failed"
                );
            }
        });
    }

    #[test]
    fn attached_detached_admission_outlives_its_pre_supervisor_deadline() {
        with_isolated_home(|_| {
            let cook_id = "attached-detached-admission";
            agent_task_lifecycle::record_detached_cook_handoff_parent(cook_id)
                .expect("persist detached admission");
            agent_task_lifecycle::record_detached_cook_handoff_child(
                cook_id,
                1,
                homeboy_core::process::ProcessStartIdentity::Macos {
                    start_seconds: 1,
                    start_microseconds: 1,
                },
            )
            .expect("attach child identity");
            agent_task_lifecycle::record_detached_cook_supervisor(cook_id, "supervisor-1")
                .expect("attach supervisor");
            agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                record.metadata["detached_cook_handoff"]["admission_deadline_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
            })
            .expect("expire old admission lease");

            let report = reconcile_stale_active_runs(false).expect("reconcile attached admission");

            assert!(report.runs.is_empty(), "{report:#?}");
            assert_eq!(
                agent_task_lifecycle::status(cook_id)
                    .expect("read protected parent")
                    .metadata["detached_cook_handoff"]["admission_state"],
                "supervising"
            );
        });
    }

    #[test]
    fn a_preview_reports_the_observed_state_and_mutates_nothing() {
        with_isolated_home(|_| {
            orphaned_running_run("reconcile-preview-orphan");

            let report = reconcile_stale_active_runs(true).expect("preview");

            let run = report
                .runs
                .iter()
                .find(|run| run.run_id == "reconcile-preview-orphan")
                .expect("candidate");
            assert_eq!(run.action, "would-reconcile");
            assert_eq!(
                run.authoritative_state,
                agent_task_lifecycle::AgentTaskRunState::Running,
                "preview mutates nothing, so the observed state is the state",
            );
            assert!(!agent_task_lifecycle::status("reconcile-preview-orphan")
                .expect("unchanged")
                .state
                .is_terminal());
        });
    }

    #[test]
    fn a_tick_pass_over_an_empty_fleet_is_inert() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let runs = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("run pass");
            let waits = homeboy_core::daemon::orchestration::reconcile_controller_waits()
                .expect("wait pass");
            assert_eq!(runs["reconciled"], 0, "{runs}");
            assert_eq!(waits["changed"], 0, "{waits}");
        });
    }
}

//! Safe reconciliation of stale/suspect/unreconciled active agent-task runs.
//! Pure move out of the former `agent_task_service.rs` god-file.
//!
//! Also hosts the agent-task side of the daemon's orchestration tick. The
//! daemon lives in `homeboy-core`, which must not depend on this subsystem, so
//! the tick dispatches through a registered driver — see
//! [`register_orchestration_driver`].

use crate::agent_task_lifecycle;
use homeboy_control_plane_contract::{
    ControlPlaneAction, ControlPlaneActionOutcome, ControlPlaneActionPayload,
    ControlPlaneActionRequest, CONTROL_PLANE_ACTION_REQUEST_SCHEMA,
};
use homeboy_core::{Error, Result};

use super::discovery::{
    discover_runs, discover_runs_in_store, AgentTaskDiscoveryFilter, AgentTaskLiveness,
};

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
/// [`agent_task_lifecycle::reconcile_status`] against an explicitly injected root.
fn rooted_status(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    Ok(agent_task_lifecycle::reconcile_status_in_store(
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
    Ok(agent_task_lifecycle::reconcile_status_in_store(
        lifecycle_store,
        run_id,
        agent_task_lifecycle::AgentTaskStatusOptions::default(),
        true,
    )?
    .record)
}

/// The record a reconcile decision is taken from.
///
/// Apply refreshes through [`rooted_exact_status`], which persists the runner,
/// candidate, and admission projections it observes. A preview must not write
/// (#13697: reads never mutate; reconciliation is the explicit action), so it
/// reads the durable record as stored. Apply still re-reads authoritatively
/// immediately before acting, so a preview can only over-report candidates,
/// never cancel one.
fn decision_status(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    dry_run: bool,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    if dry_run {
        agent_task_lifecycle::exact_record_in_store(lifecycle_store, run_id)
    } else {
        rooted_exact_status(lifecycle_store, run_id)
    }
}

/// Read back the durable record and its active discovery projection after an
/// apply. A successful reconcile must mean the selected record is no longer a
/// blocking projection, not merely that its cancellation write returned.
fn verified_reconciled_status(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    let record = rooted_exact_status(lifecycle_store, run_id)?;
    let remains_active = discover_runs_in_store(
        lifecycle_store,
        AgentTaskDiscoveryFilter::Active,
        Default::default(),
    )?
    .runs
    .into_iter()
    .any(|run| run.run_id == run_id);
    // Only an accepted runner job leaves a projection behind. A submission that
    // never obtained a job identity has nothing for the runner to resolve, so
    // requiring an Idle report would make it permanently unreconcilable.
    let runner_projection_resolved = record.runner_job_id().is_none()
        || record.runner_id().is_none_or(|runner_id| {
            agent_task_lifecycle::runner_live_job_authority(runner_id)
                == agent_task_lifecycle::RunnerLiveJobAuthority::Idle
        });
    verify_reconciled_postcondition(&record, remains_active, runner_projection_resolved)?;
    Ok(record)
}

#[cfg(test)]
fn fenced_record_is_live(record: &agent_task_lifecycle::AgentTaskRunRecord) -> bool {
    record.state().is_terminal()
        || record.has_fresh_controller_pre_provider_heartbeat()
        || (record.has_planned_runner_execution() && record.has_fresh_update())
        || record.has_live_execution_step_owner()
        || record.owner_process_is_running()
}

pub(super) fn verify_reconciled_postcondition(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    remains_active: bool,
    runner_projection_resolved: bool,
) -> Result<()> {
    if record.state().is_terminal() && !remains_active && runner_projection_resolved {
        return Ok(());
    }

    Err(Error::internal_unexpected(format!(
        "agent-task reconciliation did not satisfy its postcondition for `{}`: durable state is {}, unresolved projection remains {}",
        record.run_id,
        run_state_label(record.state()),
        if remains_active {
            "in active discovery"
        } else if !runner_projection_resolved {
            "on the runner"
        } else {
            "non-terminal"
        },
    )))
}

/// Actor recorded on every cancellation reconciliation commits.
pub(crate) const RECONCILER_ACTOR: &str = "fleet-reconciler";
/// Cancellation cause for reconciliation; operator commands keep
/// `operator_requested`. A reconcile cancellation is retryable work loss, not
/// an operator's decision to end the mission.
pub(crate) const RECONCILE_CANCELLATION_CAUSE: &str = "reconcile";

/// The provenance a reconciliation cancel writes, naming the reconciler and the
/// evidence it acted on.
fn reconciler_cancellation_provenance(
    reason: &str,
    liveness: AgentTaskLiveness,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    runner_evidence: Option<&agent_task_lifecycle::RunnerJobEvidence>,
) -> serde_json::Value {
    let mut evidence = serde_json::json!({
        "liveness": liveness.as_str(),
        "record_updated_at": record.updated_at,
        "stale_running_reason": record.stale_running_reason(),
        "owner_pid": record.owner_pid(),
    });
    if let Some(runner_evidence) = runner_evidence {
        evidence["runner_job"] = runner_evidence.to_value();
    }
    serde_json::json!({
        "actor": RECONCILER_ACTOR,
        "cause": RECONCILE_CANCELLATION_CAUSE,
        "reason": reason,
        "evidence": evidence,
        "retryable": true,
        "recovery_action": format!("homeboy agent-task retry {} --run", record.run_id),
    })
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
        // Preview leaves the runner-backed projection unrefreshed: refreshing
        // persists what it observes, and a preview must not write.
        if !dry_run && run.runner_id.is_some() && run.runner_job_id.is_some() {
            match rooted_status(&lifecycle_store, &run.run_id) {
                Ok(refreshed) if refreshed.state().is_terminal() => continue,
                Ok(refreshed) if !refreshed.is_stale_running() => continue,
                Ok(refreshed) => authoritative_state = refreshed.state(),
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

        let record = agent_task_lifecycle::exact_record_in_store(&lifecycle_store, &run.run_id)?;
        // A runner-backed run is cancelled only once its exact runner job is
        // verifiably gone or terminal. Decide that before claiming the action:
        // a claimed no-op would otherwise be replayed under the same key on
        // every tick, long after the job has actually ended.
        let runner_evidence =
            agent_task_lifecycle::observe_runner_job_in_store(&lifecycle_store, &record);
        if let Some(evidence) = runner_evidence
            .as_ref()
            .filter(|evidence| !evidence.permits_cancellation())
        {
            runs.push(AgentTaskReconcileRun {
                run_id: run.run_id,
                liveness,
                source: run.source,
                authoritative_state,
                stale_reason: Some(evidence.reason.to_string()),
                action: "no-op",
                error: None,
            });
            continue;
        }
        let stale_reason = runner_evidence
            .as_ref()
            .map(|evidence| evidence.reason.to_string())
            .or(run.stale_reason);
        if dry_run {
            runs.push(AgentTaskReconcileRun {
                run_id: run.run_id,
                liveness,
                source: run.source,
                // Preview mutates nothing, so the observed state is the state.
                authoritative_state,
                stale_reason,
                action: "would-reconcile",
                error: None,
            });
            continue;
        }
        // Freeze the discovered target identity into a deterministic per-run
        // action key. The canonical action claim serializes concurrent daemon,
        // cleanup, and CLI replays, and its immutable acknowledgement is the
        // audit record for this fleet member.
        // The observed runner evidence is part of the decision identity, so a
        // changed observation is a new decision rather than a replay.
        let idempotency_key = match runner_evidence.as_ref() {
            Some(evidence) => format!(
                "fleet-reconcile:{}:{}:{}",
                run.run_id,
                record.updated_at.as_deref().unwrap_or(&record.submitted_at),
                evidence.reason
            ),
            None => format!(
                "fleet-reconcile:{}:{}",
                run.run_id,
                record.updated_at.as_deref().unwrap_or(&record.submitted_at)
            ),
        };
        let request = ControlPlaneActionRequest {
            schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            effect_id: homeboy_control_plane_contract::EffectId(format!(
                "reconcile:{}",
                run.run_id
            )),
            action: ControlPlaneAction::Reconcile,
            idempotency_key,
            actor: "homeboy-fleet-reconciler".to_string(),
            expected_updated_at: record.updated_at.clone(),
            parameters: ControlPlaneActionPayload::empty(),
            confirmed: true,
        };
        match crate::orchestration::execute_action_from_current_environment(&run.run_id, &request) {
            Ok(acknowledgement) if acknowledgement.outcome != ControlPlaneActionOutcome::Failed => {
                let current = rooted_status(&lifecycle_store, &run.run_id)?;
                let action =
                    if acknowledgement.outcome == ControlPlaneActionOutcome::AlreadySatisfied {
                        "no-op"
                    } else {
                        "reconciled"
                    };
                if action == "reconciled" {
                    reconciled += 1;
                }
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    authoritative_state: current.state(),
                    stale_reason,
                    action,
                    error: None,
                });
            }
            Ok(acknowledgement) => {
                failed += 1;
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    authoritative_state,
                    stale_reason,
                    action: "failed",
                    error: acknowledgement.message,
                });
            }
            Err(error) => {
                failed += 1;
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    authoritative_state,
                    stale_reason,
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
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    reconcile_run_in_store(&lifecycle_store, run_id, dry_run)
}

pub(crate) fn reconcile_run_in_store(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    dry_run: bool,
) -> Result<AgentTaskReconcileReport> {
    // One store for the whole reconciliation: the scope resolution, every
    // authoritative read, and the expiry or cancellation that follows are one
    // answer about one run.
    let requested_run_id = run_id.to_string();
    let resolved_run_ids =
        agent_task_lifecycle::reconcile_scope_run_ids_in_store(&lifecycle_store, run_id)?;
    let mut runs = Vec::new();
    let mut reconciled = 0;
    let mut failed = 0;
    for resolved_run_id in &resolved_run_ids {
        let authoritative = decision_status(lifecycle_store, resolved_run_id, dry_run)?;
        let source = authoritative
            .runner_id()
            .map(|runner_id| format!("runner:{runner_id}"))
            .unwrap_or_else(|| "local".to_string());
        let candidate = discover_runs_in_store(
            lifecycle_store,
            AgentTaskDiscoveryFilter::Active,
            Default::default(),
        )?
        .runs
        .into_iter()
        .find(|run| run.run_id == *resolved_run_id);
        match candidate {
            Some(run) if run.liveness.is_some_and(AgentTaskLiveness::is_reconcilable) => {
                let liveness = run.liveness.expect("checked reconcilable liveness");
                // Re-read immediately before apply. A newly-live runner or a changed
                // owner is authoritative and turns this scoped request into a no-op.
                let refreshed = decision_status(lifecycle_store, resolved_run_id, dry_run)?;
                let locally_reconcilable_after_runner_idle =
                    refreshed.is_locally_reconcilable_after_runner_idle();
                let still_reconcilable = discover_runs_in_store(
                    lifecycle_store,
                    AgentTaskDiscoveryFilter::Active,
                    Default::default(),
                )?
                .runs
                .into_iter()
                .find(|run| run.run_id == *resolved_run_id)
                .and_then(|run| run.liveness)
                .is_some_and(AgentTaskLiveness::is_reconcilable);
                if refreshed.state().is_terminal() || !still_reconcilable {
                    runs.push(AgentTaskReconcileRun {
                        run_id: resolved_run_id.clone(),
                        liveness,
                        source: run.source,
                        authoritative_state: refreshed.state(),
                        stale_reason: run.stale_reason,
                        action: "no-op",
                        error: None,
                    });
                } else if refreshed.runner_id().is_some()
                    && refreshed.runner_job_id().is_some()
                    && !locally_reconcilable_after_runner_idle
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
                        authoritative_state: refreshed.state(),
                        stale_reason: run.stale_reason,
                        action: "no-op",
                        error: None,
                    });
                } else if let Some(evidence) =
                    agent_task_lifecycle::observe_runner_job_in_store(lifecycle_store, &refreshed)
                        .filter(|evidence| !evidence.permits_cancellation())
                {
                    // A stale projection never proves its runner job is gone.
                    // The job is live, heartbeating, or simply unverifiable, so
                    // the run is left for its owning daemon to settle.
                    runs.push(AgentTaskReconcileRun {
                        run_id: resolved_run_id.clone(),
                        liveness,
                        source: run.source,
                        authoritative_state: refreshed.state(),
                        stale_reason: Some(evidence.reason.to_string()),
                        action: "no-op",
                        error: None,
                    });
                } else if dry_run {
                    let runner_evidence = agent_task_lifecycle::observe_runner_job_in_store(
                        lifecycle_store,
                        &refreshed,
                    );
                    runs.push(AgentTaskReconcileRun {
                        run_id: resolved_run_id.clone(),
                        liveness,
                        source: run.source,
                        authoritative_state: refreshed.state(),
                        stale_reason: runner_evidence
                            .map(|evidence| evidence.reason.to_string())
                            .or(run.stale_reason),
                        action: "would-reconcile",
                        error: None,
                    });
                } else {
                    if let Some(runner_id) = refreshed.runner_id() {
                        // This guard protects work a runner is actually
                        // executing. A submission that never produced a runner
                        // job identity has no remote owner to defer to, so
                        // refusing it would strand the record as permanently
                        // unreconcilable.
                        if refreshed.runner_job_id().is_some()
                            && agent_task_lifecycle::runner_authority(runner_id)
                                != agent_task_lifecycle::RunnerAuthority::Removed
                            && !locally_reconcilable_after_runner_idle
                        {
                            failed += 1;
                            runs.push(AgentTaskReconcileRun {
                                run_id: resolved_run_id.clone(),
                                liveness,
                                source: run.source,
                                authoritative_state: refreshed.state(),
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
                                let verified =
                                    verified_reconciled_status(&lifecycle_store, resolved_run_id);
                                let verified = match verified {
                                    Ok(record) => record,
                                    Err(error) => {
                                        failed += 1;
                                        runs.push(AgentTaskReconcileRun {
                                            run_id: resolved_run_id.clone(),
                                            liveness,
                                            source: run.source,
                                            authoritative_state: refreshed.state(),
                                            stale_reason: run.stale_reason,
                                            action: "failed",
                                            error: Some(error.message),
                                        });
                                        continue;
                                    }
                                };
                                reconciled += 1;
                                runs.push(AgentTaskReconcileRun {
                                    run_id: resolved_run_id.clone(),
                                    liveness,
                                    source: run.source,
                                    authoritative_state: verified.state(),
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
                                .state(),
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
                                    authoritative_state: refreshed.state(),
                                    stale_reason: run.stale_reason,
                                    action: "failed",
                                    error: Some(error.message),
                                });
                            }
                        }
                        continue;
                    }
                    // The runner job, when there is one, was verified gone or
                    // terminal above. Name that evidence, not the projection.
                    let runner_evidence = agent_task_lifecycle::observe_runner_job_in_store(
                        lifecycle_store,
                        &refreshed,
                    );
                    let reason = runner_evidence
                        .as_ref()
                        .map(|evidence| evidence.reason.to_string())
                        .or_else(|| run.stale_reason.clone())
                        .unwrap_or_else(|| format!("reconciled stale-{} run", liveness.as_str()));
                    let provenance = reconciler_cancellation_provenance(
                        &reason,
                        liveness,
                        &refreshed,
                        runner_evidence.as_ref(),
                    );
                    match agent_task_lifecycle::cancel_exact_run_in_store_with_provenance(
                        &lifecycle_store,
                        resolved_run_id,
                        Some(&reason),
                        Some(provenance),
                    ) {
                        Ok(record) => {
                            let verified =
                                match verified_reconciled_status(&lifecycle_store, resolved_run_id)
                                {
                                    Ok(record) => record,
                                    Err(error) => {
                                        failed += 1;
                                        runs.push(AgentTaskReconcileRun {
                                            run_id: resolved_run_id.clone(),
                                            liveness,
                                            source: run.source,
                                            authoritative_state: record.state(),
                                            stale_reason: run.stale_reason,
                                            action: "failed",
                                            error: Some(error.message),
                                        });
                                        continue;
                                    }
                                };
                            reconciled += 1;
                            runs.push(AgentTaskReconcileRun {
                                run_id: resolved_run_id.clone(),
                                liveness,
                                source: run.source,
                                authoritative_state: verified.state(),
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
                                authoritative_state: refreshed.state(),
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
                authoritative_state: authoritative.state(),
                stale_reason: run.stale_reason,
                action: "no-op",
                error: None,
            }),
            None => runs.push(AgentTaskReconcileRun {
                run_id: resolved_run_id.clone(),
                liveness: AgentTaskLiveness::Active,
                source,
                authoritative_state: authoritative.state(),
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
/// forever. Loop Work jobs now own controller wait advancement.
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

    /// Tests are the entry point for their own unit of work, so the store
    /// resolves once here (#7505).
    fn test_lifecycle_store() -> crate::agent_task_lifecycle::AgentTaskLifecycleStore {
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("lifecycle store")
    }
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
                agent_task_lifecycle::reconcile_status("reconcile-tick-orphan").expect("refreshed");
            assert!(
                refreshed.state().is_terminal(),
                "orphan must not stay running: {:?}",
                refreshed.state()
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
            let refreshed = agent_task_lifecycle::reconcile_status(run_id).expect("refreshed");
            assert_eq!(
                refreshed.state(),
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
            let stale = agent_task_lifecycle::reconcile_status(orphan_run_id)
                .expect("PID-less owner is projected stale before daemon reconciliation");
            assert_eq!(stale.metadata["stale_running_reason"], "missing_runner_pid");

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("ownerless tick pass");
            assert_eq!(report["reconciled"], 1, "{report}");
            let terminal =
                agent_task_lifecycle::reconcile_status(orphan_run_id).expect("ownerless run");
            assert_eq!(
                terminal.state(),
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(terminal.metadata["cancel_reason"], "missing_runner_pid");
        });
    }

    #[test]
    fn planned_lab_proxy_at_provider_start_survives_initiating_client_exit() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_id = "reconcile-lab-planned-provider-start";
            let plan = AgentTaskPlan::new("reconcile-tick-plan", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
            agent_task_lifecycle::record_cook_progress_in_store(
                &test_lifecycle_store(),
                run_id,
                "provider_start",
                1,
                None,
            )
            .expect("provider_start");
            let remote_command = [
                "homeboy".to_string(),
                "--placement".to_string(),
                "local".to_string(),
                "agent-task".to_string(),
                "run-plan".to_string(),
            ];
            agent_task_lifecycle::record_lab_offload_planned(
                agent_task_lifecycle::LabOffloadProxyPlan {
                    run_id,
                    runner_id: "homeboy-lab",
                    remote_workspace: "/runner/workspace/homeboy",
                    remote_command: &remote_command,
                    durable_plan: Some(&plan),
                },
            )
            .expect("planned Lab proxy");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record
                    .metadata
                    .as_object_mut()
                    .expect("metadata object")
                    .remove("runner_pid");
            })
            .expect("initiating client ended");

            let live = agent_task_lifecycle::reconcile_status(run_id).expect("planned proxy");
            assert!(
                live.has_fresh_controller_pre_provider_heartbeat()
                    || (live.has_planned_runner_execution() && live.has_fresh_update()),
                "{live:?}"
            );
            assert!(
                live.updated_at.is_some(),
                "planned proxy must stamp a heartbeat"
            );
            assert_eq!(
                live.metadata["runner_execution_record"]["status"],
                "planned"
            );
            assert!(live.metadata.get("stale_running").is_none());

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("tick after initiating client exit");
            assert_eq!(report["reconciled"], 0, "{report}");
            let retained = agent_task_lifecycle::reconcile_status(run_id).expect("retained run");
            assert!(
                !retained.state().is_terminal(),
                "Lab proxy must survive initiating-client exit: {:?}",
                retained.state()
            );
            assert!(retained.metadata.get("cancel_reason").is_none());
            assert_eq!(retained.runner_id(), Some("homeboy-lab"));
            assert!(retained.runner_job_id().is_none());
        });
    }

    #[test]
    fn controller_heartbeat_keeps_queued_pre_provider_materialization_out_of_runner_pid_watchdog() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_id = "reconcile-controller-pre-provider-heartbeat";
            let plan = AgentTaskPlan::new("reconcile-tick-plan", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record
                    .metadata
                    .as_object_mut()
                    .expect("metadata object")
                    .remove("runner_pid");
                record.metadata["runner_id"] = serde_json::json!("homeboy-lab");
            })
            .expect("runner has not been submitted yet");
            agent_task_lifecycle::record_cook_progress_in_store(
                &agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                    .expect("lifecycle store"),
                run_id,
                "worktree_provider_ensure",
                1,
                Some("provider budget is 120 seconds; ensure remains active after 20 seconds"),
            )
            .expect("controller heartbeat");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata["cook_progress"]["started_at"] = serde_json::json!(
                    (chrono::Utc::now() - chrono::Duration::seconds(21)).to_rfc3339()
                );
                record.metadata["cook_progress"]["provider_budget_ms"] =
                    serde_json::json!(120_000u64);
            })
            .expect("record slow provider evidence");

            let status =
                agent_task_lifecycle::reconcile_status(run_id).expect("status while ensure runs");
            assert!(
                status.has_fresh_controller_pre_provider_heartbeat(),
                "{status:?}"
            );
            assert!(status.metadata.get("stale_running").is_none());
            assert_eq!(
                status.metadata["cook_progress"]["phase"],
                "worktree_provider_ensure"
            );
            assert_eq!(
                status.metadata["cook_progress"]["provider_budget_ms"],
                120_000
            );

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("watchdog pass");
            assert_eq!(report["reconciled"], 0, "{report}");
            let retained = agent_task_lifecycle::reconcile_status(run_id).expect("retained run");
            assert_eq!(
                retained.state(),
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
            assert!(retained.metadata.get("cancel_reason").is_none());
        });
    }

    #[test]
    fn fenced_recheck_preserves_heartbeat_recorded_after_stale_discovery_snapshot() {
        with_isolated_home(|_| {
            let run_id = "reconcile-controller-heartbeat-after-snapshot";
            let plan = AgentTaskPlan::new("reconcile-tick-plan", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");

            let snapshot = discover_runs(AgentTaskDiscoveryFilter::Active)
                .expect("ownerless queued run discovered");
            let discovered = snapshot
                .runs
                .iter()
                .find(|run| run.run_id == run_id)
                .expect("stale snapshot contains run");
            assert_eq!(discovered.liveness, Some(AgentTaskLiveness::Stale));

            agent_task_lifecycle::record_cook_progress_in_store(
                &test_lifecycle_store(),
                run_id,
                "worktree_provider_lookup",
                1,
                Some("provider lookup started after discovery"),
            )
            .expect("controller heartbeat");

            let fenced =
                agent_task_lifecycle::exact_record_in_store(&test_lifecycle_store(), run_id)
                    .expect("fenced record");
            assert!(fenced_record_is_live(&fenced));
            assert_eq!(
                fenced.state(),
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
        });
    }

    #[test]
    fn parallel_local_supervised_submissions_survive_daemon_reconciliation() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_ids = ["reconcile-parallel-local-a", "reconcile-parallel-local-b"];
            std::thread::scope(|scope| {
                for run_id in run_ids {
                    scope.spawn(move || {
                        let plan = AgentTaskPlan::new("parallel-local-cook", Vec::new());
                        let metadata = serde_json::Map::from_iter([(
                            "local_cook_supervisor".to_string(),
                            serde_json::json!({
                                "state": "supervising",
                                "job_id": uuid::Uuid::new_v4(),
                                "job_type": homeboy_core::daemon::step_job::STEP_JOB_TYPE,
                                "pinned_run_id": run_id,
                            }),
                        )]);
                        agent_task_lifecycle::submit_plan_with_runtime_admission_in_store(
                            &test_lifecycle_store(),
                            &plan,
                            Some(run_id),
                            None,
                            Some(metadata),
                            None,
                            |_| Ok(serde_json::json!({ "build": "test" })),
                        )
                        .expect("submit supervised local Cook");
                    });
                }
            });

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("tick after parallel local submission");
            assert_eq!(report["reconciled"], 0, "{report}");
            for run_id in run_ids {
                let record = agent_task_lifecycle::reconcile_status(run_id).expect("retained");
                assert!(!record.state().is_terminal(), "{run_id} was terminalized");
                assert_eq!(record.metadata["runner_pid"], std::process::id());
                assert!(record.owner_process_is_running());
                assert!(record.metadata.get("cancel_reason").is_none());
            }
        });
    }

    #[test]
    fn projected_local_retry_supervisor_keeps_lease_ownership_before_runner_pid() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_id = "reconcile-projected-local-retry";
            let cook_id = "reconcile-projected-local-retry-cook";
            let plan = AgentTaskPlan::new("projected-local-retry", Vec::new());
            let metadata = agent_task_lifecycle::execution_step_owner_fixture(cook_id);
            agent_task_lifecycle::submit_plan_with_runtime_admission_in_store(
                &test_lifecycle_store(),
                &plan,
                Some(run_id),
                None,
                Some(metadata),
                None,
                |_| Ok(serde_json::json!({ "build": "test" })),
            )
            .expect("reserve retry");

            let supervised = agent_task_lifecycle::exact_record(run_id).expect("projected");
            assert!(supervised.has_live_execution_step_owner());
            assert!(supervised.metadata.get("runner_pid").is_none());

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("tick after supervisor projection");
            assert_eq!(report["reconciled"], 0, "{report}");
            let retained = agent_task_lifecycle::reconcile_status(run_id).expect("retained");
            assert!(!retained.state().is_terminal());
            assert!(retained.metadata.get("cancel_reason").is_none());
            assert!(retained.has_live_execution_step_owner());
        });
    }

    #[test]
    fn live_local_retry_child_survives_reconcile_when_daemon_supervisor_is_unavailable() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_id = "reconcile-live-local-retry-child";
            let plan = AgentTaskPlan::new("live-local-retry-child", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
            agent_task_lifecycle::mark_running(run_id).expect("running");
            let child_pid = std::process::id();
            let child_start_identity = homeboy_core::process::process_start_identity(child_pid)
                .expect("read process start identity")
                .expect("current process identity");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record
                    .metadata
                    .as_object_mut()
                    .expect("metadata object")
                    .remove("runner_pid");
                record
                    .metadata
                    .as_object_mut()
                    .expect("metadata object")
                    .remove("runner_process_start_identity");
                record.metadata["cook_id"] = serde_json::json!("live-local-retry-cook");
                record.metadata["local_cook_supervisor"] = serde_json::json!({
                    "state": "supervising",
                    "pinned_run_id": run_id,
                    "child_pid": child_pid,
                    "child_start_identity": child_start_identity,
                });
                record.updated_at =
                    Some((chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339());
            })
            .expect("persist retry child identity");

            let record = agent_task_lifecycle::exact_record(run_id).expect("retry record");
            assert!(!record.owner_process_is_running());
            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("reconcile after observer/daemon outage");
            assert_eq!(report["reconciled"], 0, "{report}");
            let retained = agent_task_lifecycle::exact_record(run_id).expect("retained run");
            assert!(!retained.state().is_terminal());
            assert!(retained.metadata.get("cancel_reason").is_none());
        });
    }

    #[test]
    fn fenced_recheck_preserves_local_owner_recorded_after_stale_discovery_snapshot() {
        with_isolated_home(|_| {
            let run_id = "reconcile-local-owner-after-snapshot";
            let plan = AgentTaskPlan::new("reconcile-tick-plan", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");

            let snapshot = discover_runs(AgentTaskDiscoveryFilter::Active)
                .expect("ownerless queued run discovered");
            let discovered = snapshot
                .runs
                .iter()
                .find(|run| run.run_id == run_id)
                .expect("stale snapshot contains run");
            assert_eq!(discovered.liveness, Some(AgentTaskLiveness::Stale));

            agent_task_lifecycle::mark_running(run_id).expect("publish owner pid");

            let fenced =
                agent_task_lifecycle::exact_record_in_store(&test_lifecycle_store(), run_id)
                    .expect("fenced record");
            assert!(fenced_record_is_live(&fenced));
            assert!(fenced.owner_process_is_running());
        });
    }

    #[test]
    fn fenced_recheck_preserves_supervisor_lease_recorded_after_stale_discovery_snapshot() {
        with_isolated_home(|_| {
            let run_id = "reconcile-supervisor-after-snapshot";
            let plan = AgentTaskPlan::new("reconcile-tick-plan", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");

            let snapshot = discover_runs(AgentTaskDiscoveryFilter::Active)
                .expect("ownerless queued run discovered");
            let discovered = snapshot
                .runs
                .iter()
                .find(|run| run.run_id == run_id)
                .expect("stale snapshot contains run");
            assert_eq!(discovered.liveness, Some(AgentTaskLiveness::Stale));

            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata["cook_id"] = serde_json::json!("cook");
                record
                    .metadata
                    .as_object_mut()
                    .unwrap()
                    .extend(agent_task_lifecycle::execution_step_owner_fixture("cook"));
            })
            .expect("publish supervisor lease");

            let fenced =
                agent_task_lifecycle::exact_record_in_store(&test_lifecycle_store(), run_id)
                    .expect("fenced record");
            assert!(fenced_record_is_live(&fenced));
            assert_eq!(
                fenced.state(),
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
        });
    }

    #[test]
    fn missing_runner_pid_after_runner_submission_still_cancels() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_id = "reconcile-missing-post-submission-runner-pid";
            let plan = AgentTaskPlan::new("reconcile-tick-plan", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
            agent_task_lifecycle::mark_running(run_id).expect("running");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record
                    .metadata
                    .as_object_mut()
                    .expect("metadata object")
                    .remove("runner_pid");
                record.metadata["runner_id"] = serde_json::json!("homeboy-lab");
                record.metadata["cook_progress"] = serde_json::json!({
                    "phase": "runner_submission",
                    "attempt": 1,
                });
            })
            .expect("runner submission without PID");

            let stale =
                agent_task_lifecycle::reconcile_status(run_id).expect("status after submission");
            assert_eq!(stale.metadata["stale_running_reason"], "missing_runner_pid");

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("watchdog pass");
            assert_eq!(report["reconciled"], 1, "{report}");
            let cancelled = agent_task_lifecycle::reconcile_status(run_id).expect("cancelled run");
            assert_eq!(
                cancelled.state(),
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(cancelled.metadata["cancel_reason"], "missing_runner_pid");
        });
    }

    #[test]
    fn fresh_cook_progress_prevents_fleet_cancellation_before_lab_job_binding() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_id = "cook-pre-provider-heartbeat-independent-of-run-timestamp";
            agent_task_lifecycle::submit_plan(
                &AgentTaskPlan::new("controller-owned-before-lab-binding", Vec::new()),
                Some(run_id),
            )
            .expect("submit Cook attempt");
            let now = chrono::Utc::now();
            let old = (now - chrono::Duration::minutes(31)).to_rfc3339();
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata["runner_id"] = serde_json::json!("fixture-lab");
                record.metadata["cook_progress"] = serde_json::json!({
                    "phase": "workspace_capacity_reservation",
                    "attempt": 1,
                    "updated_at": now.to_rfc3339(),
                });
                // Runner-targeted progress can be newer than the run-level
                // timestamp before a runner job id exists.
                record.updated_at = Some(old.clone());
            })
            .expect("retain controller progress before Lab accepts the job");

            let active = agent_task_lifecycle::exact_record(run_id).expect("queued Cook attempt");
            assert!(active.has_fresh_controller_pre_provider_heartbeat());
            let report = reconcile_stale_active_runs(false).expect("daemon watchdog pass");
            assert_eq!(report.reconciled, 0, "{report:?}");
            assert_eq!(
                agent_task_lifecycle::exact_record(run_id).unwrap().state(),
                agent_task_lifecycle::AgentTaskRunState::Queued,
                "a live controller heartbeat must protect its unbound Lab attempt"
            );

            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata["cook_progress"]["updated_at"] = serde_json::json!(old);
            })
            .expect("expire controller heartbeat");
            let expired = reconcile_stale_active_runs(false).expect("expired watchdog pass");
            assert_eq!(expired.reconciled, 1, "{expired:?}");
            assert_eq!(
                agent_task_lifecycle::exact_record(run_id).unwrap().state(),
                agent_task_lifecycle::AgentTaskRunState::Cancelled,
                "an expired unbound attempt remains recoverable by the stale-run watchdog"
            );
        });
    }

    #[test]
    fn concurrent_planned_submissions_survive_delayed_runner_identity_publication() {
        use std::sync::{Arc, Barrier};

        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_ids = [
                "reconcile-concurrent-planned-1",
                "reconcile-concurrent-planned-2",
                "reconcile-concurrent-planned-3",
                "reconcile-concurrent-planned-4",
            ];
            for run_id in run_ids {
                orphaned_running_run(run_id);
                agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                    record
                        .metadata
                        .as_object_mut()
                        .expect("metadata object")
                        .remove("runner_pid");
                })
                .expect("remove controller owner identity");
            }

            let published = Arc::new(Barrier::new(run_ids.len() + 1));
            let release = Arc::new(Barrier::new(run_ids.len() + 1));
            let mut planners = Vec::new();
            for run_id in run_ids {
                let published = Arc::clone(&published);
                let release = Arc::clone(&release);
                planners.push(std::thread::spawn(move || {
                    let plan = AgentTaskPlan::new("concurrent-planned-runner", Vec::new());
                    agent_task_lifecycle::record_lab_offload_phase(
                        run_id,
                        "homeboy-lab",
                        "materializing",
                        None,
                        None,
                        None,
                        Some(&plan),
                    )
                    .expect("planned runner submission");
                    published.wait();
                    release.wait();
                }));
            }
            published.wait();

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("cleanup during delayed identity publication");
            assert_eq!(report["reconciled"], 0, "{report}");
            for run_id in run_ids {
                let record =
                    agent_task_lifecycle::reconcile_status(run_id).expect("planned record");
                assert!(!record.state().is_terminal(), "{run_id} was terminalized");
                assert_eq!(
                    record.metadata["runner_execution_record"]["status"],
                    "planned"
                );
                assert!(record.metadata.get("cancel_reason").is_none());
            }

            release.wait();
            for planner in planners {
                planner.join().expect("planner thread");
            }
            for run_id in run_ids {
                let accepted = agent_task_lifecycle::record_detached_lab_run(
                    agent_task_lifecycle::DetachedLabRunRecord {
                        run_id,
                        runner_id: "homeboy-lab",
                        runner_job_id: &format!("delayed-{run_id}"),
                        remote_workspace: "/runner/workspace/homeboy",
                        remote_command: &[],
                    },
                )
                .expect("delayed identity publication");
                assert_eq!(
                    accepted.runner_job_id(),
                    Some(format!("delayed-{run_id}").as_str())
                );
            }
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
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist detached admission");

            let report = reconcile_stale_active_runs(false).expect("reconcile admission state");

            assert!(
                report.runs.is_empty(),
                "a pre-supervisor admission is not an abandoned executor: {report:#?}"
            );
            let parent =
                agent_task_lifecycle::reconcile_status(cook_id).expect("read durable admission");
            assert_eq!(
                parent.state(),
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
                agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                    &test_lifecycle_store(),
                    cook_id,
                )
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
                let parent =
                    agent_task_lifecycle::reconcile_status(cook_id).expect("read terminal parent");
                assert_eq!(
                    parent.state(),
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
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist detached admission");
            agent_task_lifecycle::record_detached_cook_handoff_child_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                homeboy_core::process::ProcessStartIdentity::Macos {
                    start_seconds: 1,
                    start_microseconds: 1,
                },
            )
            .expect("attach child identity");
            agent_task_lifecycle::record_detached_cook_supervisor_in_store(
                &test_lifecycle_store(),
                cook_id,
                "supervisor-1",
            )
            .expect("attach supervisor");
            agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                record.metadata["detached_cook_handoff"]["admission_deadline_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
            })
            .expect("expire old admission lease");

            let report = reconcile_stale_active_runs(false).expect("reconcile attached admission");

            assert!(report.runs.is_empty(), "{report:#?}");
            assert_eq!(
                agent_task_lifecycle::reconcile_status(cook_id)
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
            assert!(
                !agent_task_lifecycle::reconcile_status("reconcile-preview-orphan")
                    .expect("unchanged")
                    .state()
                    .is_terminal()
            );
        });
    }

    /// A runner-local `running` record (the shape a Lab daemon reconciles for
    /// its own runs) whose projection heartbeat aged past the liveness window
    /// and that has no local owner pid: only its runner job can say whether the
    /// work is still alive.
    fn stale_runner_backed_run(run_id: &str, runner_job_id: &str) {
        let plan = AgentTaskPlan::new("reconcile-runner-job-plan", Vec::new());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted");
        agent_task_lifecycle::mark_running(run_id).expect("running");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            let metadata = record.metadata.as_object_mut().expect("metadata object");
            metadata.remove("runner_pid");
            metadata.remove("runner_process_start_identity");
            record.metadata["runner_job_id"] = serde_json::json!(runner_job_id);
            record.updated_at = Some(
                (chrono::Utc::now() - chrono::Duration::hours(2))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            );
        })
        .expect("stale runner-backed projection");
    }

    fn local_job_store() -> homeboy_core::api_jobs::JobStore {
        homeboy_core::api_jobs::JobStore::open_without_reconciliation(
            test_lifecycle_store().observation_db_path(),
        )
        .expect("local daemon job store")
    }

    #[test]
    fn fleet_reconcile_never_cancels_a_stale_projection_of_a_live_runner_job() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let jobs = local_job_store();
            let job = jobs.create("agent-task.run-plan");
            jobs.start(job.id).expect("runner job is running");
            let run_id = "reconcile-live-runner-job";
            stale_runner_backed_run(run_id, &job.id.to_string());

            let report = reconcile_stale_active_runs(false).expect("fleet reconcile");

            assert_eq!(report.reconciled, 0, "{report:?}");
            let run = report
                .runs
                .iter()
                .find(|run| run.run_id == run_id)
                .expect("stale projection is considered");
            assert_eq!(run.action, "no-op");
            assert_eq!(run.stale_reason.as_deref(), Some("runner_job_live"));
            let retained = agent_task_lifecycle::exact_record(run_id).expect("retained run");
            assert_eq!(
                retained.state(),
                agent_task_lifecycle::AgentTaskRunState::Running
            );
            assert!(retained.metadata.get("cancel_reason").is_none());
            assert!(retained.metadata.get("cancellation_provenance").is_none());
            assert!(
                !retained.is_stale_running(),
                "a heartbeating runner job keeps its projection out of stale-running"
            );

            // The scoped reconcile the daemon action performs is equally inert.
            let scoped =
                reconcile_run_in_store(&test_lifecycle_store(), run_id, false).expect("scoped");
            assert_eq!(scoped.reconciled, 0, "{scoped:?}");
            assert!(!agent_task_lifecycle::exact_record(run_id)
                .expect("retained run")
                .state()
                .is_terminal());
        });
    }

    #[test]
    fn fleet_reconcile_cancels_a_verifiably_gone_runner_job_with_reconciler_provenance() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let run_id = "reconcile-gone-runner-job";
            stale_runner_backed_run(run_id, &uuid::Uuid::new_v4().to_string());

            let report = reconcile_stale_active_runs(false).expect("fleet reconcile");

            assert_eq!(report.reconciled, 1, "{report:?}");
            let cancelled = agent_task_lifecycle::exact_record(run_id).expect("cancelled run");
            assert_eq!(
                cancelled.state(),
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(cancelled.metadata["cancel_reason"], "runner_job_not_found");
            let provenance = &cancelled.metadata["cancellation_provenance"];
            assert_eq!(provenance["actor"], "fleet-reconciler");
            assert_eq!(provenance["cause"], "reconcile");
            assert_eq!(provenance["reason"], "runner_job_not_found");
            assert_eq!(
                provenance["evidence"]["runner_job"]["observation"],
                "not_found"
            );
            assert_eq!(
                provenance["evidence"]["runner_job"]["source"],
                "local_job_store"
            );
            assert_ne!(
                provenance["reason"], "runner_job_unverified_after_daemon_restart",
                "no daemon restart was evidenced"
            );
        });
    }

    #[test]
    fn fleet_reconcile_names_a_terminal_runner_job() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let jobs = local_job_store();
            let job = jobs.create("agent-task.run-plan");
            jobs.start(job.id).expect("runner job started");
            jobs.cancel(job.id, "runner job ended")
                .expect("runner job terminal");
            let run_id = "reconcile-terminal-runner-job";
            stale_runner_backed_run(run_id, &job.id.to_string());

            let preview = reconcile_stale_active_runs(true).expect("preview");
            let previewed = preview
                .runs
                .iter()
                .find(|run| run.run_id == run_id)
                .expect("candidate");
            assert_eq!(previewed.action, "would-reconcile");
            assert_eq!(
                previewed.stale_reason.as_deref(),
                Some("runner_job_terminal")
            );

            let report = reconcile_stale_active_runs(false).expect("fleet reconcile");
            assert_eq!(report.reconciled, 1, "{report:?}");
            let cancelled = agent_task_lifecycle::exact_record(run_id).expect("cancelled run");
            assert_eq!(cancelled.metadata["cancel_reason"], "runner_job_terminal");
            assert_eq!(
                cancelled.metadata["cancellation_provenance"]["evidence"]["runner_job"]
                    ["job_status"],
                "cancelled"
            );
        });
    }

    #[test]
    fn runner_job_observation_permits_cancellation_only_for_gone_or_terminal_jobs() {
        use agent_task_lifecycle::RunnerJobObservation as Observation;
        for observation in [
            Observation::Live,
            Observation::HeartbeatStale,
            Observation::Unverifiable,
        ] {
            assert!(!observation.permits_cancellation(), "{observation:?}");
        }
        for observation in [
            Observation::Terminal,
            Observation::LostAfterDaemonRestart,
            Observation::NotFound,
            Observation::RunnerRemoved,
        ] {
            assert!(observation.permits_cancellation(), "{observation:?}");
        }
        assert_eq!(
            Observation::LostAfterDaemonRestart.reason(),
            "runner_job_lost_after_daemon_restart"
        );
    }

    #[test]
    fn a_tick_pass_over_an_empty_fleet_is_inert() {
        with_isolated_home(|_| {
            register_orchestration_driver();
            let runs = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("run pass");
            assert_eq!(runs["reconciled"], 0, "{runs}");
        });
    }
}

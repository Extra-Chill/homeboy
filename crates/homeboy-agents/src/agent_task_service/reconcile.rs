//! Safe reconciliation of stale/suspect/unreconciled active agent-task runs.
//! Pure move out of the former `agent_task_service.rs` god-file.
//!
//! Also hosts the agent-task side of the daemon's orchestration tick. The
//! daemon lives in `homeboy-core`, which must not depend on this subsystem, so
//! the tick dispatches through a registered driver — see
//! [`register_orchestration_driver`].

use crate::agent_task_lifecycle;
use homeboy_core::Result;
use std::collections::HashMap;

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

fn fenced_record_is_live(record: &agent_task_lifecycle::AgentTaskRunRecord) -> bool {
    record.state.is_terminal()
        || record.has_fresh_controller_pre_provider_heartbeat()
        || (record.has_planned_runner_execution() && record.has_fresh_update())
        || agent_task_lifecycle::has_live_pending_runner_submission_intent(
            record,
            chrono::Utc::now(),
        )
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
            // Discovery is a fleet snapshot. A Lab planner can publish its
            // run-bound execution after that snapshot but before this cleanup
            // reaches cancellation. Share the handoff fence with that writer,
            // then decide from the fenced record so a fresh planned submission
            // remains alive until normal expiry/acceptance reconciliation.
            let _lock =
                agent_task_lifecycle::LabHandoffLock::lock_in_store(&lifecycle_store, &run.run_id)?;
            let fenced = lifecycle_store.read_record(&run.run_id)?;
            if fenced_record_is_live(&fenced) {
                continue;
            }
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

    fn reconcile_unmaterialized_cook_admissions(&self) -> Result<serde_json::Value> {
        reconcile_unmaterialized_cook_admissions()
    }
}

/// Advance reference-only Cook admissions from the daemon's serialized tick.
/// A transition to `queued` is a fenced replay signal; destination provisioning
/// remains in the established CLI Cook path after that signal is consumed.
pub fn reconcile_unmaterialized_cook_admissions() -> Result<serde_json::Value> {
    reconcile_unmaterialized_cook_admissions_with(
        None,
        homeboy_core::daemon::orchestration::select_unmaterialized_cook_runner,
        homeboy_core::daemon::orchestration::replay_unmaterialized_cook_admission,
    )
}

/// Reconcile exactly one durable admission. Used by explicit resume so one
/// operator action never probes or mutates unrelated queued Cooks.
pub fn reconcile_unmaterialized_cook_admission(run_id: &str) -> Result<serde_json::Value> {
    reconcile_unmaterialized_cook_admissions_with(
        Some(run_id),
        homeboy_core::daemon::orchestration::select_unmaterialized_cook_runner,
        homeboy_core::daemon::orchestration::replay_unmaterialized_cook_admission,
    )
}

#[doc(hidden)]
pub fn reconcile_unmaterialized_cook_admission_with(
    run_id: &str,
    select_runner: impl FnMut(&serde_json::Value) -> Result<serde_json::Value>,
    replay: impl FnMut(&serde_json::Value) -> Result<serde_json::Value>,
) -> Result<serde_json::Value> {
    reconcile_unmaterialized_cook_admissions_with(Some(run_id), select_runner, replay)
}

fn reconcile_unmaterialized_cook_admissions_with(
    run_id: Option<&str>,
    select_runner: impl FnMut(&serde_json::Value) -> Result<serde_json::Value>,
    replay: impl FnMut(&serde_json::Value) -> Result<serde_json::Value>,
) -> Result<serde_json::Value> {
    reconcile_unmaterialized_cook_admissions_with_process_identity(
        run_id,
        select_runner,
        replay,
        |pid, identity| {
            homeboy_core::process::process_identity_state_with_start_identity(
                pid,
                None,
                Some(identity),
            )
        },
    )
}

fn reconcile_unmaterialized_cook_admissions_with_process_identity(
    run_id: Option<&str>,
    mut select_runner: impl FnMut(&serde_json::Value) -> Result<serde_json::Value>,
    mut replay: impl FnMut(&serde_json::Value) -> Result<serde_json::Value>,
    mut process_identity_state: impl FnMut(
        u32,
        &homeboy_core::process::ProcessStartIdentity,
    ) -> homeboy_core::process::ProcessIdentityState,
) -> Result<serde_json::Value> {
    const MAX_ADMISSIONS_PER_PASS: usize = 32;
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let now = chrono::Utc::now();
    let mut considered = 0usize;
    let mut queued = 0usize;
    let mut blocked = 0usize;
    let mut exhausted = 0usize;
    let mut replayed = 0usize;
    let mut replay_failed = 0usize;
    let mut records = match run_id {
        Some(run_id) => vec![agent_task_lifecycle::exact_record_in_store(&store, run_id)?],
        None => store.read_records()?,
    };
    records.retain(|record| {
        !record.state.is_terminal() && record.metadata["unmaterialized_cook_admission"].is_object()
    });
    records.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    let records = if run_id.is_some() || records.len() <= MAX_ADMISSIONS_PER_PASS {
        records
    } else {
        rotate_admission_batch(&store, records, MAX_ADMISSIONS_PER_PASS)?
    };
    let mut selection_cache = HashMap::<String, serde_json::Value>::new();
    for record in records {
        let record =
            if record.metadata["unmaterialized_cook_admission"]["state"] == "preparing_inputs" {
                match agent_task_lifecycle::recover_unmaterialized_cook_input_publication_in_store(
                    &store,
                    &record.run_id,
                ) {
                    Ok(record) => record,
                    Err(_) => continue,
                }
            } else {
                record
            };
        let admission = &record.metadata["unmaterialized_cook_admission"];
        if admission["state"] == "preparing_inputs" {
            continue;
        }
        let due = admission["retry"]["next_attempt_at"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_none_or(|value| value.with_timezone(&chrono::Utc) <= now);
        let active_lease =
            unmaterialized_replay_lease_is_active(admission, now, &mut process_identity_state);
        if !due || active_lease {
            continue;
        }
        considered += 1;
        let selection_key = admission_selection_cache_key(admission);
        let selection = selection_cache
            .entry(selection_key)
            .or_insert_with(|| {
                select_runner(&serde_json::json!({
                    "schema": "homeboy/unmaterialized-cook-runner-selection-request/v1",
                    "cook_id": record.run_id,
                    "binding": admission["binding"],
                }))
                .unwrap_or_else(|error| {
                    serde_json::json!({
                        "state": "blocked_runner_unavailable",
                        "reason": homeboy_core::redaction::redact_string(&error.message),
                    })
                })
            })
            .clone();
        let selected_runner = selection["runner_id"].as_str();
        let eligible = selected_runner.is_some();
        let mut should_exhaust = false;
        let token = uuid::Uuid::new_v4().to_string();
        let mut claimed_fence = None;
        let updated = store.mutate_record(&record.run_id, |current| {
            if current.state.is_terminal() {
                return false;
            }
            let admission = &mut current.metadata["unmaterialized_cook_admission"];
            let active_lease =
                unmaterialized_replay_lease_is_active(admission, now, &mut process_identity_state);
            if active_lease {
                return false;
            }
            let attempts = admission["admission_attempts"].as_u64().unwrap_or(0) + 1;
            let max = admission["retry"]["max_attempts"].as_u64().unwrap_or(20);
            admission["admission_attempts"] = serde_json::json!(attempts);
            if attempts >= max {
                admission
                    .as_object_mut()
                    .expect("unmaterialized admission object")
                    .remove("lease");
                admission["state"] = serde_json::json!("exhausted");
                admission["reason"] =
                    serde_json::json!("bounded Lab admission retry budget exhausted");
                should_exhaust = true;
                exhausted += 1;
            } else if eligible {
                let fence = admission["fence"].as_u64().unwrap_or(0) + 1;
                admission["fence"] = serde_json::json!(fence);
                admission["state"] = serde_json::json!("queued");
                admission["lease"] = serde_json::json!({
                    "state": "claimed",
                    "fence": fence,
                    "token": token,
                    "expires_at": (now + chrono::Duration::seconds(60)).to_rfc3339(),
                });
                claimed_fence = Some(fence);
                queued += 1;
            } else {
                admission
                    .as_object_mut()
                    .expect("unmaterialized admission object")
                    .remove("lease");
                let shift = u32::try_from(attempts.min(5)).unwrap_or(5);
                let delay = 15_i64.saturating_mul(1_i64 << shift);
                admission["retry"]["next_attempt_at"] =
                    serde_json::json!((now + chrono::Duration::seconds(delay)).to_rfc3339());
                admission["state"] = serde_json::json!(selection["state"]
                    .as_str()
                    .filter(|state| matches!(
                        *state,
                        "queued" | "blocked_runner_unavailable" | "blocked_runner_stale"
                    ))
                    .unwrap_or("blocked_runner_unavailable"));
                admission["reason"] = serde_json::json!(selection["reason"]
                    .as_str()
                    .map(homeboy_core::redaction::redact_string)
                    .unwrap_or_else(|| "no currently eligible Lab runner".to_string()));
                blocked += 1;
            }
            current.updated_at = Some(now.to_rfc3339());
            true
        })?;
        if should_exhaust {
            let _ = agent_task_lifecycle::fail_detached_cook_handoff_parent_in_store(
                &store,
                &record.run_id,
                "bounded Lab admission retry budget exhausted",
            )?;
        }
        if let (Some(fence), Some(runner_id), Some(updated)) =
            (claimed_fence, selected_runner, updated)
        {
            let request = serde_json::json!({
                "schema": "homeboy/unmaterialized-cook-replay-request/v1",
                "cook_id": record.run_id,
                "fence": fence,
                "token": token,
                "runner_id": runner_id,
                "intent": updated.metadata["unmaterialized_cook_admission"]["binding"]["replay_intent"],
            });
            let replay_result = replay(&request);
            let token_for_receipt = token.clone();
            let _ = store.mutate_record(&record.run_id, |current| {
                let admission = &mut current.metadata["unmaterialized_cook_admission"];
                if admission["lease"]["fence"].as_u64() != Some(fence)
                    || admission["lease"]["token"].as_str() != Some(token_for_receipt.as_str())
                    || !matches!(
                        admission["lease"]["state"].as_str(),
                        Some("claimed" | "consumed" | "materializing")
                    )
                {
                    return false;
                }
                match &replay_result {
                    Ok(receipt) => {
                        admission["replay_receipt"] = receipt.clone();
                        admission["replay_receipt"]["recorded_at"] =
                            serde_json::json!(chrono::Utc::now().to_rfc3339());
                        replayed += 1;
                    }
                    Err(error) => {
                        admission["state"] = serde_json::json!("blocked_runner_unavailable");
                        admission["reason"] = serde_json::json!(
                            homeboy_core::redaction::redact_string(&error.message)
                        );
                        admission["lease"]["state"] = serde_json::json!("released");
                        admission["retry"]["next_attempt_at"] = serde_json::json!(
                            (chrono::Utc::now() + chrono::Duration::seconds(15)).to_rfc3339()
                        );
                        replay_failed += 1;
                    }
                }
                true
            })?;
        }
    }
    Ok(serde_json::json!({
        "schema": "homeboy/unmaterialized-cook-admission-reconcile/v1",
        "considered": considered,
        "queued": queued,
        "blocked": blocked,
        "exhausted": exhausted,
        "replayed": replayed,
        "replay_failed": replay_failed,
    }))
}

fn admission_selection_cache_key(admission: &serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "placement": admission["binding"]["placement"],
        "provider_runtime_refs": admission["binding"]["provider_runtime_refs"],
    }))
    .unwrap_or_else(|_| "configured-policy".to_string())
}

fn rotate_admission_batch(
    store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    records: Vec<agent_task_lifecycle::AgentTaskRunRecord>,
    limit: usize,
) -> Result<Vec<agent_task_lifecycle::AgentTaskRunRecord>> {
    let start = store
        .unmaterialized_admission_cursor()
        .and_then(|last| records.iter().position(|record| record.run_id > last))
        .unwrap_or(0);
    let count = limit.min(records.len());
    let mut records = records.into_iter().map(Some).collect::<Vec<_>>();
    let total = records.len();
    let selected = (0..count)
        .map(|offset| {
            records[(start + offset) % total]
                .take()
                .expect("fair admission index is unique")
        })
        .collect::<Vec<_>>();
    if let Some(last) = selected.last() {
        store.write_unmaterialized_admission_cursor(&last.run_id)?;
    }
    Ok(selected)
}

fn unmaterialized_replay_lease_is_active(
    admission: &serde_json::Value,
    now: chrono::DateTime<chrono::Utc>,
    process_identity_state: &mut impl FnMut(
        u32,
        &homeboy_core::process::ProcessStartIdentity,
    ) -> homeboy_core::process::ProcessIdentityState,
) -> bool {
    let lease = &admission["lease"];
    match lease["state"].as_str() {
        Some("claimed") => lease["expires_at"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|value| value.with_timezone(&chrono::Utc) > now),
        Some("consumed" | "materializing") => {
            let owner = &lease["owner"];
            if owner.is_null() {
                return false;
            }
            let Some(pid) = owner["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
            else {
                // Malformed identity evidence is indeterminate, not proof that
                // a potentially live materializer can be superseded safely.
                return true;
            };
            let Ok(identity) = serde_json::from_value::<homeboy_core::process::ProcessStartIdentity>(
                owner["process_start_identity"].clone(),
            ) else {
                return true;
            };
            matches!(
                process_identity_state(pid, &identity),
                homeboy_core::process::ProcessIdentityState::Live
                    | homeboy_core::process::ProcessIdentityState::Unverifiable
            )
        }
        _ => false,
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

    fn due_unmaterialized_admission(run_id: &str) {
        agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
            &test_lifecycle_store(),
            run_id,
            serde_json::json!({
                "placement": { "requested": "auto", "local_fallback": false },
                "provider_runtime_refs": { "required_capabilities": [] },
            }),
            "queued",
            "eligible",
        )
        .expect("admitted");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["unmaterialized_cook_admission"]["retry"]["next_attempt_at"] =
                serde_json::json!("2000-01-01T00:00:00+00:00");
        })
        .expect("make admission due");
    }

    #[test]
    fn ordinary_rows_before_an_admission_do_not_consume_the_pass_limit() {
        with_isolated_home(|_| {
            for index in 0..32 {
                let plan = AgentTaskPlan::new(format!("ordinary-plan-{index:02}"), Vec::new());
                agent_task_lifecycle::submit_plan(&plan, Some(&format!("a-ordinary-{index:02}")))
                    .expect("ordinary row");
            }
            due_unmaterialized_admission("z-unmaterialized");
            let replayed = std::cell::Cell::new(0usize);
            let report = reconcile_unmaterialized_cook_admissions_with(
                None,
                |_| Ok(serde_json::json!({ "state": "eligible", "runner_id": "lab" })),
                |_| {
                    replayed.set(replayed.get() + 1);
                    Ok(serde_json::json!({ "worker_id": "worker" }))
                },
            )
            .expect("reconcile admission after ordinary rows");
            assert_eq!(report["replayed"], 1);
            assert_eq!(replayed.get(), 1);
        });
    }

    #[test]
    fn persisted_admission_cursor_advances_later_rows_after_reconciler_restart() {
        with_isolated_home(|_| {
            for index in 0..40 {
                due_unmaterialized_admission(&format!("fair-admission-{index:02}"));
            }
            let replayed = std::cell::RefCell::new(std::collections::BTreeSet::new());
            let run_pass = || {
                reconcile_unmaterialized_cook_admissions_with(
                    None,
                    |_| Ok(serde_json::json!({ "state": "eligible", "runner_id": "lab" })),
                    |request| {
                        replayed
                            .borrow_mut()
                            .insert(request["cook_id"].as_str().expect("cook id").to_string());
                        Ok(serde_json::json!({ "worker_id": "worker" }))
                    },
                )
                .expect("fair reconcile pass");
            };
            run_pass();
            assert_eq!(replayed.borrow().len(), 32);
            let restarted_store =
                agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                    .expect("restarted store");
            assert_eq!(
                restarted_store.unmaterialized_admission_cursor().as_deref(),
                Some("fair-admission-31")
            );
            run_pass();
            assert_eq!(replayed.borrow().len(), 40);
        });
    }

    #[test]
    fn compatible_admissions_share_one_runner_selection_snapshot_per_pass() {
        with_isolated_home(|_| {
            for index in 0..3 {
                due_unmaterialized_admission(&format!("cached-selection-{index}"));
            }
            let selections = std::cell::Cell::new(0usize);
            let report = reconcile_unmaterialized_cook_admissions_with(
                None,
                |_| {
                    selections.set(selections.get() + 1);
                    Ok(serde_json::json!({ "state": "blocked_runner_unavailable" }))
                },
                |_| panic!("blocked admission must not replay"),
            )
            .expect("cached selection pass");
            assert_eq!(report["considered"], 3);
            assert_eq!(selections.get(), 1);
        });
    }

    #[test]
    fn preparing_inputs_admission_is_never_claimed_before_publication() {
        with_isolated_home(|_| {
            let cook_id = "preparing-inputs-not-claimable";
            let root = homeboy_core::paths::homeboy_data()
                .expect("data root")
                .join("agent-task-cook-admissions");
            agent_task_lifecycle::prepare_unmaterialized_cook_admission(
                cook_id,
                serde_json::json!({
                    "request_ref": "sha256:preparing",
                    "input_publication": {
                        "state": "staged",
                        "staging_root": root.join(".missing-stage"),
                        "published_root": root.join(cook_id).join("inputs"),
                    },
                }),
                "queued",
                "eligible",
            )
            .expect("prepared admission");
            agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                record.metadata["unmaterialized_cook_admission"]["retry"]["next_attempt_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
            })
            .expect("due");
            assert!(agent_task_lifecycle::detached_cook_admission_is_live(
                &agent_task_lifecycle::exact_record(cook_id).expect("prepared record"),
                chrono::Utc::now(),
            ));

            let report = reconcile_unmaterialized_cook_admissions_with(
                None,
                |_| panic!("preparing admission must not select a runner"),
                |_| panic!("preparing admission must not replay"),
            )
            .expect("preparing pass");
            assert_eq!(report["considered"], 0);
            assert_eq!(
                agent_task_lifecycle::exact_record(cook_id)
                    .expect("still preparing")
                    .metadata["unmaterialized_cook_admission"]["state"],
                "preparing_inputs"
            );
        });
    }

    #[test]
    fn explicit_same_runner_with_different_requirements_does_not_share_verdict() {
        with_isolated_home(|_| {
            for (run_id, capability) in [
                ("explicit-capability-compatible", "supported"),
                ("explicit-capability-incompatible", "missing"),
            ] {
                agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                    &test_lifecycle_store(),
                    run_id,
                    serde_json::json!({
                        "placement": {
                            "requested": "lab",
                            "runner_ref": "same-runner",
                            "local_fallback": false,
                        },
                        "provider_runtime_refs": {
                            "backend": "fixture",
                            "required_capabilities": [capability],
                            "runtime_generation": "test",
                        },
                    }),
                    "queued",
                    "eligible",
                )
                .expect("admitted");
                agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                    record.metadata["unmaterialized_cook_admission"]["retry"]["next_attempt_at"] =
                        serde_json::json!("2000-01-01T00:00:00+00:00");
                })
                .expect("due");
            }
            let selections = std::cell::Cell::new(0usize);
            let replays = std::cell::Cell::new(0usize);
            let report = reconcile_unmaterialized_cook_admissions_with(
                None,
                |request| {
                    selections.set(selections.get() + 1);
                    let required = request["binding"]["provider_runtime_refs"]
                        ["required_capabilities"][0]
                        .as_str();
                    Ok(if required == Some("supported") {
                        serde_json::json!({ "state": "eligible", "runner_id": "same-runner" })
                    } else {
                        serde_json::json!({
                            "state": "blocked_runner_unavailable",
                            "reason": "required capability unavailable",
                        })
                    })
                },
                |_| {
                    replays.set(replays.get() + 1);
                    Ok(serde_json::json!({ "worker_id": "worker" }))
                },
            )
            .expect("capability-aware selection");
            assert_eq!(selections.get(), 2);
            assert_eq!(replays.get(), 1);
            assert_eq!(report["replayed"], 1);
            assert_eq!(report["blocked"], 1);
            assert!(
                agent_task_lifecycle::exact_record("explicit-capability-incompatible")
                    .expect("blocked record")
                    .tasks
                    .is_empty()
            );
        });
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

            let status = agent_task_lifecycle::status(run_id).expect("status while ensure runs");
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
            let retained = agent_task_lifecycle::status(run_id).expect("retained run");
            assert_eq!(
                retained.state,
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
                fenced.state,
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

            let stale = agent_task_lifecycle::status(run_id).expect("status after submission");
            assert_eq!(stale.metadata["stale_running_reason"], "missing_runner_pid");

            let report = homeboy_core::daemon::orchestration::reconcile_stale_active_runs()
                .expect("watchdog pass");
            assert_eq!(report["reconciled"], 1, "{report}");
            let cancelled = agent_task_lifecycle::status(run_id).expect("cancelled run");
            assert_eq!(
                cancelled.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(cancelled.metadata["cancel_reason"], "missing_runner_pid");
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
                let record = agent_task_lifecycle::status(run_id).expect("planned record");
                assert!(!record.state.is_terminal(), "{run_id} was terminalized");
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
            assert_eq!(runs["reconciled"], 0, "{runs}");
        });
    }

    #[test]
    fn unmaterialized_admission_retries_reconnect_once_under_a_fenced_lease() {
        with_isolated_home(|_| {
            let cook_id = "reconcile-unmaterialized-reconnect";
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &test_lifecycle_store(),
                cook_id,
                serde_json::json!({
                    "placement": {
                        "local_fallback": false,
                    }
                }),
                "blocked_runner_unavailable",
                "runner disconnected",
            )
            .expect("admitted");
            let make_due = || {
                agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                    record.metadata["unmaterialized_cook_admission"]["retry"]["next_attempt_at"] =
                        serde_json::json!("2000-01-01T00:00:00+00:00");
                })
                .expect("make retry due");
            };

            make_due();
            let blocked = reconcile_unmaterialized_cook_admissions_with(
                None,
                |_| Ok(serde_json::json!({ "state": "blocked_runner_unavailable" })),
                |_| panic!("blocked admission must not replay"),
            )
            .expect("blocked pass");
            assert_eq!(blocked["blocked"], 1);
            let blocked_record = agent_task_lifecycle::exact_record(cook_id).expect("blocked");
            assert_eq!(
                blocked_record.metadata["unmaterialized_cook_admission"]["state"],
                "blocked_runner_unavailable"
            );
            assert!(blocked_record.tasks.is_empty());

            make_due();
            let replay_calls = std::cell::Cell::new(0usize);
            let connected = reconcile_unmaterialized_cook_admissions_with(
                None,
                |request| {
                    assert!(request["binding"]["placement"]["candidate_runner_refs"].is_null());
                    Ok(serde_json::json!({ "state": "eligible", "runner_id": "configured-later" }))
                },
                |_| {
                    replay_calls.set(replay_calls.get() + 1);
                    Ok(serde_json::json!({ "worker_id": "worker-1" }))
                },
            )
            .expect("reconnect pass");
            assert_eq!(connected["queued"], 1);
            assert_eq!(connected["replayed"], 1);
            assert_eq!(replay_calls.get(), 1);
            let claimed = agent_task_lifecycle::exact_record(cook_id).expect("claimed");
            let lease = claimed.metadata["unmaterialized_cook_admission"]["lease"].clone();
            assert_eq!(lease["state"], "claimed");
            assert_eq!(
                claimed.metadata["unmaterialized_cook_admission"]["fence"],
                1
            );

            let duplicate = reconcile_unmaterialized_cook_admissions_with(
                None,
                |_| Ok(serde_json::json!({ "state": "eligible", "runner_id": "lab" })),
                |_| panic!("active claim must not replay twice"),
            )
            .expect("duplicate reconnect pass");
            assert_eq!(duplicate["queued"], 0);
            assert_eq!(
                agent_task_lifecycle::exact_record(cook_id)
                    .expect("same claim")
                    .metadata["unmaterialized_cook_admission"]["lease"],
                lease
            );

            agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                record.metadata["unmaterialized_cook_admission"]["lease"]["expires_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
                record.metadata["unmaterialized_cook_admission"]["retry"]["next_attempt_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
            })
            .expect("expire abandoned replay claim");
            let recovered_calls = std::cell::Cell::new(0usize);
            let recovered = reconcile_unmaterialized_cook_admissions_with(
                None,
                |_| Ok(serde_json::json!({ "state": "eligible", "runner_id": "lab" })),
                |_| {
                    recovered_calls.set(recovered_calls.get() + 1);
                    Ok(serde_json::json!({ "worker_id": "worker-2" }))
                },
            )
            .expect("restart recovery pass");
            assert_eq!(recovered["replayed"], 1);
            assert_eq!(recovered_calls.get(), 1);
            assert_eq!(
                agent_task_lifecycle::exact_record(cook_id)
                    .expect("reclaimed")
                    .metadata["unmaterialized_cook_admission"]["fence"],
                2
            );
        });
    }

    #[test]
    fn cancelled_unmaterialized_admission_never_invokes_replay() {
        with_isolated_home(|_| {
            let cook_id = "reconcile-unmaterialized-cancelled";
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &test_lifecycle_store(),
                cook_id,
                serde_json::json!({
                    "placement": { "candidate_runner_refs": ["lab"], "local_fallback": false },
                    "replay_intent": { "schema": "fixture" }
                }),
                "queued",
                "waiting",
            )
            .expect("admitted");
            agent_task_lifecycle::cancel_run(cook_id, Some("operator cancellation"))
                .expect("cancelled");
            let report = reconcile_unmaterialized_cook_admissions_with(
                None,
                |_| Ok(serde_json::json!({ "state": "eligible", "runner_id": "lab" })),
                |_| panic!("cancelled admission must not replay"),
            )
            .expect("cancelled pass");
            assert_eq!(report["considered"], 0);
            assert_eq!(report["replayed"], 0);
        });
    }

    #[test]
    fn live_materializer_identity_blocks_duplicate_replay() {
        with_isolated_home(|_| {
            let cook_id = "reconcile-materializing-fence";
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &test_lifecycle_store(),
                cook_id,
                serde_json::json!({ "placement": { "local_fallback": false } }),
                "queued",
                "eligible",
            )
            .expect("admitted");
            agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                record.metadata["unmaterialized_cook_admission"]["state"] =
                    serde_json::json!("materializing");
                record.metadata["unmaterialized_cook_admission"]["fence"] = serde_json::json!(3);
                record.metadata["unmaterialized_cook_admission"]["lease"] = serde_json::json!({
                    "state": "materializing",
                    "fence": 3,
                    "token": "winner",
                    "materialization_fence_at": "2000-01-01T00:00:00+00:00",
                    "owner": {
                        "pid": 42,
                        "process_start_identity": {
                            "platform": "linux",
                            "starttime_ticks": 100,
                        },
                    },
                });
                record.metadata["unmaterialized_cook_admission"]["retry"]["next_attempt_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
            })
            .expect("materializing owner");
            let report = reconcile_unmaterialized_cook_admissions_with_process_identity(
                None,
                |_| panic!("materializing admission must not probe another runner"),
                |_| panic!("materializing admission must not replay"),
                |pid, identity| {
                    assert_eq!(pid, 42);
                    assert_eq!(
                        identity,
                        &homeboy_core::process::ProcessStartIdentity::Linux {
                            starttime_ticks: 100,
                        }
                    );
                    homeboy_core::process::ProcessIdentityState::Live
                },
            )
            .expect("race pass");
            assert_eq!(report["considered"], 0);
            let record = agent_task_lifecycle::exact_record(cook_id).expect("winner retained");
            assert_eq!(record.metadata["unmaterialized_cook_admission"]["fence"], 3);
            assert_eq!(
                record.metadata["unmaterialized_cook_admission"]["lease"]["token"],
                "winner"
            );
            assert!(unmaterialized_replay_lease_is_active(
                &record.metadata["unmaterialized_cook_admission"],
                chrono::Utc::now(),
                &mut |_, _| homeboy_core::process::ProcessIdentityState::Unverifiable,
            ));
        });
    }

    #[test]
    fn dead_reused_and_absent_materializer_identities_are_reclaimed() {
        with_isolated_home(|_| {
            for (cook_id, has_owner, identity_state) in [
                (
                    "reconcile-dead-materializer",
                    true,
                    Some(homeboy_core::process::ProcessIdentityState::Dead),
                ),
                (
                    "reconcile-reused-materializer",
                    true,
                    Some(homeboy_core::process::ProcessIdentityState::IdentityMismatch),
                ),
                ("reconcile-absent-materializer", false, None),
            ] {
                agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                    &test_lifecycle_store(),
                    cook_id,
                    serde_json::json!({ "placement": { "local_fallback": false } }),
                    "queued",
                    "eligible",
                )
                .expect("admitted");
                agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                    let admission = &mut record.metadata["unmaterialized_cook_admission"];
                    admission["state"] = serde_json::json!("materializing");
                    admission["fence"] = serde_json::json!(3);
                    admission["lease"] = serde_json::json!({
                        "state": "materializing",
                        "fence": 3,
                        "token": "abandoned",
                        "owner": {
                            "pid": 42,
                            "process_start_identity": {
                                "platform": "linux",
                                "starttime_ticks": 100,
                            },
                        },
                    });
                    if !has_owner {
                        admission["lease"]
                            .as_object_mut()
                            .expect("lease object")
                            .remove("owner");
                    }
                    admission["retry"]["next_attempt_at"] =
                        serde_json::json!("2000-01-01T00:00:00+00:00");
                })
                .expect("abandoned materializer");

                let report = reconcile_unmaterialized_cook_admissions_with_process_identity(
                    Some(cook_id),
                    |_| Ok(serde_json::json!({ "state": "eligible", "runner_id": "lab" })),
                    |_| Ok(serde_json::json!({ "worker_id": "replacement" })),
                    |_, _| identity_state.expect("absent owner is not inspected"),
                )
                .expect("recover materializer");
                assert_eq!(report["replayed"], 1, "{report}");
                let record = agent_task_lifecycle::exact_record(cook_id).expect("reclaimed");
                assert_eq!(
                    record.metadata["unmaterialized_cook_admission"]["lease"]["state"],
                    "claimed"
                );
                assert_eq!(record.metadata["unmaterialized_cook_admission"]["fence"], 4);
            }
        });
    }

    #[test]
    fn restart_replay_converges_to_the_new_live_materializer() {
        with_isolated_home(|_| {
            let cook_id = "reconcile-restart-materializer";
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &test_lifecycle_store(),
                cook_id,
                serde_json::json!({ "placement": { "local_fallback": false } }),
                "queued",
                "eligible",
            )
            .expect("admitted");
            agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                let admission = &mut record.metadata["unmaterialized_cook_admission"];
                admission["state"] = serde_json::json!("materializing");
                admission["fence"] = serde_json::json!(1);
                admission["lease"] = serde_json::json!({
                    "state": "materializing",
                    "fence": 1,
                    "token": "crashed",
                    "owner": {
                        "pid": 42,
                        "process_start_identity": {
                            "platform": "linux",
                            "starttime_ticks": 100,
                        },
                    },
                });
                admission["retry"]["next_attempt_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
            })
            .expect("crashed materializer");

            let replay_calls = std::cell::Cell::new(0usize);
            let recovered = reconcile_unmaterialized_cook_admissions_with_process_identity(
                Some(cook_id),
                |_| Ok(serde_json::json!({ "state": "eligible", "runner_id": "lab" })),
                |request| {
                    replay_calls.set(replay_calls.get() + 1);
                    let fence = request["fence"].as_u64().expect("fence");
                    let token = request["token"].as_str().expect("token");
                    assert!(
                        agent_task_lifecycle::consume_unmaterialized_cook_replay_claim(
                            cook_id, fence, token
                        )?
                    );
                    assert!(
                        agent_task_lifecycle::renew_unmaterialized_cook_replay_claim(
                            cook_id, fence, token
                        )?
                    );
                    Ok(serde_json::json!({ "worker_id": "replacement" }))
                },
                |_, _| homeboy_core::process::ProcessIdentityState::Dead,
            )
            .expect("restart recovery");
            assert_eq!(recovered["replayed"], 1);
            assert_eq!(replay_calls.get(), 1);

            let duplicate = reconcile_unmaterialized_cook_admissions_with(
                Some(cook_id),
                |_| panic!("live replacement must not select again"),
                |_| panic!("live replacement must not replay again"),
            )
            .expect("converged pass");
            assert_eq!(duplicate["considered"], 0);
            assert_eq!(replay_calls.get(), 1);
        });
    }

    #[test]
    fn scoped_reconcile_never_scans_or_mutates_a_sibling_admission() {
        with_isolated_home(|_| {
            for cook_id in ["scoped-admission-target", "scoped-admission-sibling"] {
                agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                    &test_lifecycle_store(),
                    cook_id,
                    serde_json::json!({ "placement": { "local_fallback": false } }),
                    "blocked_runner_unavailable",
                    "waiting",
                )
                .expect("admitted");
                agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                    record.metadata["unmaterialized_cook_admission"]["retry"]["next_attempt_at"] =
                        serde_json::json!("2000-01-01T00:00:00+00:00");
                })
                .unwrap();
            }
            let selections = std::cell::Cell::new(0usize);
            let report = reconcile_unmaterialized_cook_admissions_with(
                Some("scoped-admission-target"),
                |_| {
                    selections.set(selections.get() + 1);
                    Ok(serde_json::json!({ "state": "blocked_runner_unavailable" }))
                },
                |_| panic!("blocked target does not replay"),
            )
            .expect("scoped pass");
            assert_eq!(report["considered"], 1);
            assert_eq!(selections.get(), 1);
            assert_eq!(
                agent_task_lifecycle::exact_record("scoped-admission-sibling")
                    .unwrap()
                    .metadata["unmaterialized_cook_admission"]["admission_attempts"],
                0
            );
        });
    }

    #[test]
    fn unmaterialized_admission_exhaustion_terminalizes_without_materialization() {
        with_isolated_home(|_| {
            let cook_id = "reconcile-unmaterialized-exhausted";
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &test_lifecycle_store(),
                cook_id,
                serde_json::json!({
                    "placement": { "candidate_runner_refs": ["lab"], "local_fallback": false }
                }),
                "blocked_runner_stale",
                "runner stale",
            )
            .expect("admitted");
            agent_task_lifecycle::rewrite_record_for_test(cook_id, |record| {
                record.metadata["unmaterialized_cook_admission"]["retry"]["max_attempts"] =
                    serde_json::json!(1);
                record.metadata["unmaterialized_cook_admission"]["retry"]["next_attempt_at"] =
                    serde_json::json!("2000-01-01T00:00:00+00:00");
            })
            .expect("exhaust next pass");

            let report = reconcile_unmaterialized_cook_admissions_with(
                None,
                |_| Ok(serde_json::json!({ "state": "blocked_runner_stale" })),
                |_| panic!("exhausted admission must not replay"),
            )
            .expect("exhausted");
            assert_eq!(report["exhausted"], 1);
            let terminal = agent_task_lifecycle::exact_record(cook_id).expect("terminal");
            assert_eq!(
                terminal.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert!(terminal.tasks.is_empty());
            assert!(terminal.metadata.get("worktree_provision").is_none());
            assert!(terminal.metadata.get("provider_execution").is_none());
        });
    }
}

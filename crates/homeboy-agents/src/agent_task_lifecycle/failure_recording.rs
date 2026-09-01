use super::store::read_plan_path;
use super::*;
use homeboy_engine_primitives::content_hash;
use sha2::Digest;
use std::path::{Path, PathBuf};

const LAB_PRE_EXECUTION_CIRCUIT_SCHEMA: &str = "homeboy/lab-pre-execution-circuit/v1";
const LAB_PRE_EXECUTION_REPAIR_ACTION: &str = "repair_lab_pre_execution_and_retry";

pub fn record_pre_execution_failure(
    run_id: &str,
    plan: &AgentTaskPlan,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_pre_execution_failure_in_store(&lifecycle_store, run_id, plan, phase, error)
}

pub fn record_pre_execution_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    // Cancellation and aggregate projection both own a terminal transition.
    // Re-read and arbitrate while holding the record/aggregate transaction lock
    // so a retry launcher failure cannot overwrite a cancellation winner.
    let (mut record, aggregate) = lifecycle_store.with_config_lock(|| {
        let record = lifecycle_store.read_record(&run_id)?;
        // A completed provider candidate still needs its non-terminal transport
        // follow-up marker. Bare cancellation and other terminal winners are
        // returned unchanged rather than being overwritten by this aggregate.
        if record.state.is_terminal() && !record.has_recorded_provider_progress() {
            return Ok((record, None));
        }
        record_pre_execution_failure_locked(lifecycle_store, record, plan, phase, error)
    })?;

    // Projection acquires its own authority locks. It follows the locked
    // arbitration/commit so a cancellation winner is never overwritten.
    if let Some(aggregate) = aggregate {
        record_terminal_artifact_projection_in_store(lifecycle_store, &mut record, &aggregate)?;
        update_cook_candidate_after_completion_in_store(
            lifecycle_store,
            &record,
            &aggregate,
            None,
        )?;
    } else if record.state.is_terminal() {
        lifecycle_store.project_terminal_record_after_unlock(&record.run_id)?;
    }
    Ok(record)
}

/// Persist interrupted-owner evidence before a local Cook observer loss becomes
/// terminal. Aggregate, attempt diagnostics, candidate harvest (or explicit
/// unavailability), stop reason, and retry duplication facts are committed first
/// so later status/diagnose reads are not an empty Failed tombstone.
pub fn record_interrupted_local_owner_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let (mut record, aggregate) = lifecycle_store.with_config_lock(|| {
        let mut record = lifecycle_store.read_record(&run_id)?;
        if record.state.is_terminal() {
            return Ok((record, None));
        }
        let decision = annotate_local_provider_ownership(&mut record);
        record_interrupted_local_owner_locked(lifecycle_store, record, decision)
    })?;
    if let Some(aggregate) = aggregate {
        record_terminal_artifact_projection_in_store(lifecycle_store, &mut record, &aggregate)?;
        update_cook_candidate_after_completion_in_store(
            lifecycle_store,
            &record,
            &aggregate,
            None,
        )?;
    } else if record.state.is_terminal() {
        lifecycle_store.project_terminal_record_after_unlock(&record.run_id)?;
    }
    Ok(record)
}

fn record_interrupted_local_owner_locked(
    lifecycle_store: &AgentTaskLifecycleStore,
    mut record: AgentTaskRunRecord,
    decision: LocalProviderOwnerDecision,
) -> Result<(AgentTaskRunRecord, Option<AgentTaskAggregate>)> {
    let plan = lifecycle_store.read_controller_plan(&record.run_id)?;
    let now = now_timestamp();
    let (has_succeeded, has_failed, has_cancelled, recovery_identity) = match decision {
        LocalProviderOwnerDecision::Interrupted {
            has_succeeded,
            has_failed,
            has_cancelled,
            recovery_identity,
        } => (has_succeeded, has_failed, has_cancelled, recovery_identity),
        LocalProviderOwnerDecision::StayRunning | LocalProviderOwnerDecision::NotApplicable => {
            let identity = record
                .metadata
                .get("provider_executions")
                .and_then(Value::as_array)
                .map(|executions| {
                    executions
                        .iter()
                        .map(|execution| execution["owner_identity"].clone())
                        .collect()
                })
                .unwrap_or_default();
            (false, false, false, identity)
        }
    };
    let consumed = record
        .metadata
        .get("provider_executions")
        .and_then(Value::as_array)
        .map(|executions| executions.len())
        .unwrap_or_default();
    let in_flight = record
        .metadata
        .get("provider_executions")
        .and_then(Value::as_array)
        .is_some_and(|executions| {
            executions
                .iter()
                .any(|execution| execution["state"] == json!("running"))
        });
    if in_flight {
        if let Some(executions) = record
            .ensure_metadata_object()
            .get_mut("provider_executions")
            .and_then(Value::as_array_mut)
        {
            for execution in executions {
                if execution["state"] == json!("running") {
                    execution["state"] = json!("cancelled");
                    execution["finished_at"] = json!(now.clone());
                }
            }
        }
    }
    let stop_reason = "local Cook observer was interrupted during provider execution".to_string();
    let outcomes = plan
        .tasks
        .iter()
        .map(|task| {
            build_interrupted_owner_outcome(
                &record.run_id,
                task,
                has_succeeded,
                has_failed,
                consumed,
                in_flight,
                &stop_reason,
            )
        })
        .collect::<Vec<_>>();
    let harvested = outcomes.iter().any(|outcome| {
        outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable
            || outcome
                .artifacts
                .iter()
                .any(crate::agent_task_timeout_artifacts::is_actionable_patch_artifact)
    });
    let (aggregate_status, ownership_state, run_cancelled) = if has_succeeded || harvested {
        (
            crate::agent_task_scheduler::AgentTaskAggregateStatus::CandidateRecoverable,
            "owner_dead",
            false,
        )
    } else if has_failed {
        (
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Failed,
            "provider_failed",
            false,
        )
    } else if has_cancelled {
        (
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Cancelled,
            "provider_cancelled",
            true,
        )
    } else {
        (
            crate::agent_task_scheduler::AgentTaskAggregateStatus::Cancelled,
            "owner_dead",
            true,
        )
    };
    let failed = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Failed | AgentTaskOutcomeStatus::Cancelled
            )
        })
        .count();
    let candidate_recoverable = outcomes
        .iter()
        .filter(|outcome| outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable)
        .count();
    let cancelled = outcomes
        .iter()
        .filter(|outcome| outcome.status == AgentTaskOutcomeStatus::Cancelled)
        .count();
    let aggregate = AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: aggregate_status,
        totals: AgentTaskAggregateTotals {
            failed,
            cancelled,
            candidate_recoverable,
            recoverable_candidates: candidate_recoverable,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes,
        events: plan
            .tasks
            .iter()
            .map(|task| AgentTaskProgressEvent {
                task_id: task.task_id.clone(),
                state: if harvested || has_succeeded {
                    AgentTaskState::CandidateRecoverable
                } else if run_cancelled {
                    AgentTaskState::Cancelled
                } else {
                    AgentTaskState::Failed
                },
                attempt: 1,
                message: Some(stop_reason.clone()),
            })
            .collect(),
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: AgentTaskQueueStatus {
            max_concurrency: plan.options.max_concurrency,
            completed: plan.tasks.len(),
            ..AgentTaskQueueStatus::default()
        },
    };
    let aggregate_path = lifecycle_store
        .aggregate_path(&record.run_id)
        .display()
        .to_string();
    apply_aggregate_to_record(&mut record, &plan, &aggregate, aggregate_path);
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "local_provider_ownership".to_string(),
        json!({
            "state": ownership_state,
            "recovery_identity": recovery_identity,
            "reconciled_at": now,
        }),
    );
    metadata.insert("stop_reason".to_string(), json!(stop_reason));
    metadata.insert(
        "terminal_failure_classification".to_string(),
        json!("interrupted_owner"),
    );
    metadata.insert("terminal_phase".to_string(), json!("interrupted_owner"));
    metadata.insert("provider_executions_consumed".to_string(), json!(consumed));
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    metadata.insert(
        "interrupted_owner".to_string(),
        json!({
            "schema": "homeboy/agent-task-interrupted-owner/v1",
            "cause": "observer_interrupted_during_provider_execution",
            "stop_reason": stop_reason,
            "provider_executions_consumed": consumed,
            "provider_budget_consumed": consumed > 0,
            "in_flight_work_may_be_duplicated": in_flight || consumed > 0,
            "candidate_status": if harvested || has_succeeded {
                "harvested_or_recoverable"
            } else {
                "unavailable"
            },
        }),
    );
    if run_cancelled {
        metadata.insert(
            "cancel_reason".to_string(),
            json!("local provider owner process is not running"),
        );
    }
    metadata.insert(
        "cook_progress".to_string(),
        json!({
            "phase": "terminal",
            "attempt": 1,
            "detail": "interrupted_owner",
            "terminal_success": harvested || has_succeeded,
            "exit_code": if harvested || has_succeeded { 0 } else { 1 },
            "updated_at": now,
        }),
    );
    let record = lifecycle_store
        .write_aggregate_and_record_locked_without_terminal_projection(&record, &aggregate)?;
    Ok((record, Some(aggregate)))
}

fn build_interrupted_owner_outcome(
    run_id: &str,
    task: &AgentTaskRequest,
    has_succeeded: bool,
    has_failed: bool,
    consumed: usize,
    in_flight: bool,
    stop_reason: &str,
) -> AgentTaskOutcome {
    let mut outcome = AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: task.task_id.clone(),
        status: if has_succeeded {
            AgentTaskOutcomeStatus::CandidateRecoverable
        } else if has_failed {
            AgentTaskOutcomeStatus::Failed
        } else {
            AgentTaskOutcomeStatus::Cancelled
        },
        summary: Some(stop_reason.to_string()),
        failure_classification: if has_succeeded {
            None
        } else {
            Some(AgentTaskFailureClassification::ExecutionFailed)
        },
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "interrupted-owner".to_string(),
            uri: format!("homeboy://agent-task/run/{run_id}/status#interrupted-owner"),
            label: Some("Interrupted local Cook observer".to_string()),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: "interrupted_owner".to_string(),
            message: stop_reason.to_string(),
            data: json!({
                "phase": "interrupted_owner",
                "provider_executions_consumed": consumed,
                "provider_budget_consumed": consumed > 0,
                "in_flight_work_may_be_duplicated": in_flight || consumed > 0,
            }),
        }],
        outputs: json!({
            "schema": "homeboy/agent-task-interrupted-owner/v1",
            "phase": "interrupted_owner",
            "stop_reason": stop_reason,
            "provider_executions_consumed": consumed,
            "provider_budget_consumed": consumed > 0,
            "in_flight_work_may_be_duplicated": in_flight || consumed > 0,
        }),
        metadata: json!({
            "kind": "interrupted_owner",
            "phase": "interrupted_owner",
            "provider_executions_consumed": consumed,
            "provider_budget_consumed": consumed > 0,
            "in_flight_work_may_be_duplicated": in_flight || consumed > 0,
        }),
        ..Default::default()
    };
    harvest_interrupted_owner_candidate(run_id, task, &mut outcome);
    outcome
}

fn harvest_interrupted_owner_candidate(
    run_id: &str,
    task: &AgentTaskRequest,
    outcome: &mut AgentTaskOutcome,
) {
    let discovery = crate::agent_task_timeout_artifacts::TimeoutArtifactDiscovery::discover(task);
    crate::agent_task_timeout_artifacts::append_unique_artifacts(
        &mut outcome.artifacts,
        discovery.artifacts,
    );
    crate::agent_task_timeout_artifacts::append_unique_evidence_refs(
        &mut outcome.evidence_refs,
        discovery.evidence_refs,
    );
    outcome.diagnostics.extend(discovery.diagnostics);
    if let Some(discovered) = discovery.outcome {
        crate::agent_task_timeout_artifacts::append_unique_artifacts(
            &mut outcome.artifacts,
            discovered.artifacts,
        );
        crate::agent_task_timeout_artifacts::append_unique_evidence_refs(
            &mut outcome.evidence_refs,
            discovered.evidence_refs,
        );
        outcome.diagnostics.extend(discovered.diagnostics);
    }
    let harvested = outcome
        .artifacts
        .iter()
        .any(crate::agent_task_timeout_artifacts::is_actionable_patch_artifact);
    if harvested {
        outcome.status = AgentTaskOutcomeStatus::CandidateRecoverable;
        outcome.failure_classification = None;
        outcome.summary =
            Some("interrupted local Cook observer left a recoverable candidate".to_string());
        outcome.metadata["candidate_status"] = json!("harvested");
        return;
    }
    crate::agent_task_timeout_artifacts::append_unique_evidence_refs(
        &mut outcome.evidence_refs,
        vec![AgentTaskEvidenceRef {
            kind: "interrupted-owner-candidate".to_string(),
            uri: format!("homeboy://agent-task/run/{run_id}/status#interrupted-owner-candidate"),
            label: Some("Candidate unavailable after interrupted owner".to_string()),
        }],
    );
    outcome.diagnostics.push(AgentTaskDiagnostic {
        class: "interrupted_owner.candidate_unavailable".to_string(),
        message:
            "no candidate could be harvested after the local Cook observer was interrupted during provider execution"
                .to_string(),
        data: json!({ "status": "unavailable" }),
    });
    outcome.metadata["candidate_status"] = json!("unavailable");
}

/// Replace only a scheduler-terminal snapshot fence failure with Cook's normal
/// retryable pre-execution record. A provider ledger entry is authoritative: it
/// permanently fences this path from rewriting real provider failures.
pub fn record_workspace_snapshot_fence_invalidation_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let (mut record, aggregate) = lifecycle_store.with_config_lock(|| {
        let record = lifecycle_store.read_record(&run_id)?;
        let is_snapshot_invalidated =
            lifecycle_store
                .read_aggregate(&run_id)
                .ok()
                .is_some_and(|aggregate| {
                    aggregate.outcomes.iter().any(|outcome| {
                        outcome.status == AgentTaskOutcomeStatus::Failed
                            && outcome.diagnostics.iter().any(|diagnostic| {
                                diagnostic.class == "agent_task.workspace_snapshot_invalidated"
                                    && diagnostic.data["pre_provider"]
                                        == serde_json::Value::Bool(true)
                            })
                    })
                });
        let consumed = record.metadata["provider_executions_consumed"]
            .as_u64()
            .unwrap_or(u64::MAX);
        if record.state != AgentTaskRunState::Failed
            || !is_snapshot_invalidated
            || record.has_recorded_provider_progress()
            || consumed != 0
        {
            return Ok((record, None));
        }
        record_pre_execution_failure_locked(
            lifecycle_store,
            record,
            plan,
            "workspace_snapshot_fence",
            error,
        )
    })?;
    if let Some(aggregate) = aggregate {
        record_terminal_artifact_projection_in_store(lifecycle_store, &mut record, &aggregate)?;
        update_cook_candidate_after_completion_in_store(
            lifecycle_store,
            &record,
            &aggregate,
            None,
        )?;
    } else if record.state.is_terminal() {
        lifecycle_store.project_terminal_record_after_unlock(&record.run_id)?;
    }
    Ok(record)
}

fn record_pre_execution_failure_locked(
    lifecycle_store: &AgentTaskLifecycleStore,
    mut record: AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    phase: &str,
    error: &Error,
) -> Result<(AgentTaskRunRecord, Option<AgentTaskAggregate>)> {
    // A transport/handoff error that arrives AFTER a provider attempt was
    // already dispatched (or completed) on a runner is not a pre-execution
    // failure. Terminalizing it here would overwrite a succeeded candidate with
    // a `Failed`, zero-artifact, `provider_executions_consumed: 0` record and
    // strand the work — exactly the loss #9377 describes. Preserve the candidate
    // and record the follow-up failure as a non-terminal, recoverable marker so
    // controller reconciliation can adopt the completed candidate without
    // rerunning the provider.
    if record.has_recorded_provider_progress() {
        return record_transport_follow_up_failure_in_store(lifecycle_store, record, phase, error)
            .map(|record| (record, None));
    }

    let task_count = plan.tasks.len();
    let failed = task_count;
    let retryable = error.retryable == Some(true);
    let failure_classification = pre_execution_failure_classification(error);
    let candidate_adoption_recovery = candidate_adoption_recovery(phase, error);
    let error_code = reported_error_code(error);
    let outcomes = plan
        .tasks
        .iter()
        .map(|task| build_pre_execution_failure_outcome(&record.run_id, task, phase, error))
        .collect();
    let aggregate = AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Failed,
        totals: AgentTaskAggregateTotals {
            failed,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes,
        events: plan
            .tasks
            .iter()
            .map(|task| AgentTaskProgressEvent {
                task_id: task.task_id.clone(),
                state: AgentTaskState::Failed,
                attempt: 1,
                message: Some(format!(
                    "agent-task pre-execution {phase} failed: {}",
                    error.message
                )),
            })
            .collect(),
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: AgentTaskQueueStatus {
            max_concurrency: plan.options.max_concurrency,
            completed: failed,
            ..AgentTaskQueueStatus::default()
        },
    };
    let aggregate_path = lifecycle_store
        .aggregate_path(&record.run_id)
        .display()
        .to_string();
    apply_aggregate_to_record(&mut record, plan, &aggregate, aggregate_path);
    let mut failed_record = record;
    let runner_id = failed_record.runner_id().map(str::to_string);
    let circuit = lab_pre_execution_circuit(&failed_record, plan, phase, error);
    let metadata = failed_record.ensure_metadata_object();
    if retryable {
        metadata.insert("retryable".to_string(), json!(true));
    }
    metadata.insert(
        "pre_execution_failure".to_string(),
        json!({
            "phase": phase,
            "error_code": error_code,
            "failure_classification": failure_classification,
            "retryable": retryable,
            "failure_code": error.details.get("field").cloned().unwrap_or_else(|| json!(error_code)),
            "message": error.message,
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
            "provider_executions_consumed": 0,
            "candidate_adoption_recovery": candidate_adoption_recovery,
            "controller_identity": homeboy_core::build_identity::current().display,
            "runner_id": runner_id,
            "task_linkage": plan.tasks.iter().map(|task| json!({
                "task_id": task.task_id,
                "workspace": task.workspace,
                "source_refs": task.source_refs,
            })).collect::<Vec<_>>(),
        }),
    );
    if let Some(circuit) = circuit {
        metadata.insert("lab_pre_execution_circuit".to_string(), circuit);
    }
    let failed_record = lifecycle_store
        .write_aggregate_and_record_locked_without_terminal_projection(
            &failed_record,
            &aggregate,
        )?;
    Ok((failed_record, Some(aggregate)))
}

/// Refuse a Cook replacement only when its own prior Lab failure has the same
/// bounded failure and execution identity. This is deliberately per-run
/// lineage: unrelated workloads never share a circuit state.
pub fn admit_lab_pre_execution_replay(
    record: &AgentTaskRunRecord,
    plan: &AgentTaskPlan,
) -> Result<()> {
    let Some(previous) = record.metadata.get("lab_pre_execution_circuit") else {
        return Ok(());
    };
    let Some(current) = lab_pre_execution_circuit_from_failure(record, plan) else {
        return Ok(());
    };
    if previous["fingerprint"] != current["fingerprint"]
        || previous["identity"] != current["identity"]
    {
        return Ok(());
    }
    let mut error = Error::validation_invalid_argument(
        "lab_pre_execution_circuit_breaker",
        "identical Lab pre-provider failure is still open; repair the Lab snapshot or SSH cleanup before retrying",
        Some(record.run_id.clone()),
        Some(vec![format!(
            "Repair the owning Lab failure, then retry: homeboy agent-task retry {} --run",
            record.run_id
        )]),
    );
    error.details = json!({
        "field": "lab_pre_execution_circuit_breaker",
        "schema": LAB_PRE_EXECUTION_CIRCUIT_SCHEMA,
        "action": LAB_PRE_EXECUTION_REPAIR_ACTION,
        "fingerprint": previous["fingerprint"],
        "provider_executions_consumed": 0,
    });
    Err(error)
}

fn lab_pre_execution_circuit(
    record: &AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    phase: &str,
    error: &Error,
) -> Option<Value> {
    if !is_circuit_breaking_lab_failure(phase, &error.message, &error.details)
        || error.retryable != Some(true)
        || record.metadata["provider_executions_consumed"]
            .as_u64()
            .unwrap_or_default()
            != 0
    {
        return None;
    }
    let identity = lab_pre_execution_identity(record, plan);
    let failure = json!({
        "phase": bounded(phase, 128),
        "error_code": error.code.as_str(),
        "failure_code": error.details["field"]
            .as_str()
            .map(|value| bounded(value, 128))
            .unwrap_or_else(|| error.code.as_str().to_string()),
        "message": bounded(failure_message(&error.message, &error.details), 512),
    });
    let fingerprint = content_hash::sha256_hex(
        serde_json::to_vec(&json!({ "identity": identity, "failure": failure }))
            .expect("circuit fingerprint is serializable")
            .as_slice(),
    );
    Some(json!({
        "schema": LAB_PRE_EXECUTION_CIRCUIT_SCHEMA,
        "state": "open",
        "action": LAB_PRE_EXECUTION_REPAIR_ACTION,
        "fingerprint": fingerprint,
        "identity": identity,
        "provider_executions_consumed": 0,
    }))
}

fn lab_pre_execution_circuit_from_failure(
    record: &AgentTaskRunRecord,
    plan: &AgentTaskPlan,
) -> Option<Value> {
    let failure = record.metadata.get("pre_execution_failure")?;
    let phase = failure.get("phase")?.as_str()?;
    if !is_circuit_breaking_lab_failure(
        phase,
        failure["message"].as_str().unwrap_or_default(),
        &failure["details"],
    ) || failure["retryable"] != Value::Bool(true)
    {
        return None;
    }
    let identity = lab_pre_execution_identity(record, plan);
    let signature = json!({
        "phase": bounded(phase, 128),
        "error_code": failure["error_code"].as_str().unwrap_or_default(),
        "failure_code": failure["failure_code"].as_str().map(|value| bounded(value, 128)),
        "message": bounded(
            failure_message(failure["message"].as_str().unwrap_or_default(), &failure["details"]),
            512,
        ),
    });
    let fingerprint = content_hash::sha256_hex(
        serde_json::to_vec(&json!({ "identity": identity, "failure": signature }))
            .expect("circuit fingerprint is serializable")
            .as_slice(),
    );
    Some(json!({ "fingerprint": fingerprint, "identity": identity }))
}

fn lab_pre_execution_identity(record: &AgentTaskRunRecord, plan: &AgentTaskPlan) -> Value {
    let source = json!({
        "source_checkout": record.metadata["source_checkout"],
        "tasks": plan.tasks.iter().map(|task| json!({
            "workspace": task.workspace,
            "source_refs": task.source_refs,
        })).collect::<Vec<_>>(),
    });
    let current_generation = homeboy_core::build_identity::current().display;
    let runner_generation = record.metadata["runner_generation"]
        .as_str()
        .or_else(|| {
            record
                .metadata
                .pointer("/runner_execution_record/generation")
                .and_then(Value::as_str)
        })
        .unwrap_or(&current_generation);
    json!({
        "runner_id": record.runner_id().map(|value| bounded(value, 256)),
        "runner_generation": bounded(runner_generation, 256),
        "source_identity": content_hash::sha256_hex(
            serde_json::to_vec(&source).expect("source identity is serializable").as_slice(),
        ),
    })
}

fn bounded(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn failure_message<'a>(message: &'a str, details: &'a Value) -> &'a str {
    details["error"].as_str().unwrap_or(message)
}

fn is_circuit_breaking_lab_failure(phase: &str, message: &str, details: &Value) -> bool {
    if phase != "lab_workspace_stage" {
        return false;
    }
    let message = failure_message(message, details);
    message.contains("snapshot manifests differ")
        || message.contains("SSH stream drain exceeded its cleanup deadline")
}

fn record_transport_follow_up_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    mut record: AgentTaskRunRecord,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "transport_follow_up_failure".to_string(),
        json!({
            "schema": super::CANDIDATE_ADOPTION_RECOVERY_SCHEMA,
            "phase": phase,
            "reason": "post_provider_transport_failure",
            "error_code": error.code.as_str(),
            "message": error.message,
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
            "retryable": true,
            "candidate_preserved": true,
            "recorded_at": now_timestamp(),
        }),
    );
    // Preserve the candidate/workspace: a run with recorded provider progress
    // must not be reaped as a clean success when its follow-up failed.
    metadata.insert("candidate_preserved".to_string(), json!(true));
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    lifecycle_store.write_record_locked_without_terminal_projection(&record)
}

pub(crate) fn build_pre_execution_failure_outcome(
    run_id: &str,
    task: &AgentTaskRequest,
    phase: &str,
    error: &Error,
) -> AgentTaskOutcome {
    let retryable = error.retryable == Some(true);
    let failure_classification = pre_execution_failure_classification(error);
    let candidate_adoption_recovery = candidate_adoption_recovery(phase, error);
    let error_code = reported_error_code(error);
    let diagnostic = AgentTaskDiagnostic {
        class: "pre_execution_failure".to_string(),
        message: error.message.clone(),
        data: json!({
            "phase": phase,
            "error_code": error_code,
            "retryable": retryable,
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
        }),
    };
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: task.task_id.clone(),
        status: AgentTaskOutcomeStatus::Failed,
        summary: Some(format!(
            "agent-task pre-execution {phase} failed: {}",
            error.message
        )),
        failure_classification: Some(failure_classification),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: std::iter::once(AgentTaskEvidenceRef {
            kind: "agent-task-pre-execution-failure".to_string(),
            uri: format!("homeboy://agent-task/run/{run_id}/status"),
            label: Some("Agent-task pre-execution failure".to_string()),
        })
        .chain(
            error
                .details
                .pointer("/child_command_result/child_result_evidence/evidence_uri")
                .and_then(Value::as_str)
                .map(|uri| AgentTaskEvidenceRef {
                    kind: "detached-child-command-result".to_string(),
                    uri: uri.to_string(),
                    label: Some("Bounded detached child command result".to_string()),
                }),
        )
        .collect(),
        diagnostics: vec![diagnostic],
        outputs: json!({
            "schema": "homeboy/agent-task-pre-execution-failure/v1",
            "phase": phase,
            "error_code": error_code,
            "retryable": retryable,
            "message": error.message,
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
        }),
        workflow: None,
        follow_up: None,
        metadata: json!({
            "kind": "pre_execution_failure",
            "phase": phase,
            "error_code": error_code,
            "retryable": retryable,
            "provider_executions_consumed": 0,
            "candidate_adoption_recovery": candidate_adoption_recovery,
        }),
    }
}

fn reported_error_code(error: &Error) -> &str {
    error
        .details
        .get("child_command_result")
        .and_then(Value::as_object)
        .and_then(|_| error.details["child_reported_error_code"].as_str())
        .unwrap_or_else(|| error.code.as_str())
}

fn candidate_adoption_recovery(phase: &str, error: &Error) -> Option<serde_json::Value> {
    let reason = if matches!(
        phase,
        "lab_handoff_preacceptance" | "transport_dispatcher_prepare"
    ) {
        "pre_provider_transport_failure"
    } else if error.details["dirty_candidate_adoption"]["reason"] == "first_provider_admission" {
        "dirty_destination_first_provider_admission"
    } else {
        return None;
    };
    Some(json!({
        "schema": super::CANDIDATE_ADOPTION_RECOVERY_SCHEMA,
        "reason": reason,
        "provider_executions_consumed": 0,
    }))
}

fn pre_execution_failure_classification(error: &Error) -> AgentTaskFailureClassification {
    if error.details["pre_execution_phase"] == "gate_toolchain_preflight" {
        return AgentTaskFailureClassification::CapabilityMissing;
    }
    if error.retryable == Some(true) {
        AgentTaskFailureClassification::Transient
    } else {
        AgentTaskFailureClassification::InvalidInput
    }
}

/// Shared `(run_id, runner_id)` identity borrowed by the Lab offload dispatch
/// failure/record builders. Embedded as a named field so each builder stops
/// repeating the same two borrows without changing any serialized shape (these
/// builders are internal and not serialized).
#[derive(Debug, Clone, Copy)]
pub struct RunDispatchIdentity<'a> {
    pub run_id: &'a str,
    pub runner_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct AgentTaskPreDispatchFailure<'a> {
    pub identity: RunDispatchIdentity<'a>,
    pub local_command: Vec<String>,
    pub remote_command: Vec<String>,
    pub remote_workspace: &'a str,
    pub failure_message: &'a str,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: i32,
}

pub fn record_pre_dispatch_failure(
    failure: AgentTaskPreDispatchFailure<'_>,
) -> Result<AgentTaskRunRecord> {
    record_pre_dispatch_failure_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        failure,
    )
}

/// [`record_pre_dispatch_failure`] against an explicitly injected root.
///
/// The prior record and the plan this failure submits describe the same run, so
/// they have to come from and land in the same installation (#7505).
pub fn record_pre_dispatch_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    failure: AgentTaskPreDispatchFailure<'_>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(failure.identity.run_id);
    if let Ok(record) = reconcile_status_in_store(
        lifecycle_store,
        &run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .map(|outcome| outcome.record)
    {
        return Ok(record);
    }

    let task_id = "agent-task-predispatch".to_string();
    let metadata = json!({
        "kind": "lab_offload_pre_dispatch_failure",
        "runner_id": failure.identity.runner_id,
        "remote_workspace": failure.remote_workspace,
        "local_command": failure.local_command,
        "remote_command": failure.remote_command,
        "exit_code": failure.exit_code,
        "failure_message": failure.failure_message,
    });
    let plan = AgentTaskPlan::new(
        format!("{run_id}.predispatch"),
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.clone(),
            group_key: Some("lab-offload".to_string()),
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "homeboy-lab".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "Persist Lab offload pre-dispatch validation failure evidence."
                .to_string(),
            inputs: json!({
                "local_command": failure.local_command,
                "remote_command": failure.remote_command,
                "runner_id": failure.identity.runner_id,
                "remote_workspace": failure.remote_workspace,
                "failure": {
                    "message": failure.failure_message,
                    "exit_code": failure.exit_code,
                    "stdout": failure.stdout,
                    "stderr": failure.stderr,
                }
            }),
            source_refs: vec![AgentTaskSourceRef {
                kind: "lab-offload-run".to_string(),
                uri: format!("homeboy://agent-task/run/{run_id}/lab-offload"),
                revision: None,
            }],
            workspace: AgentTaskWorkspace {
                mode: AgentTaskWorkspaceMode::Existing,
                root: Some(failure.remote_workspace.to_string()),
                kind: Some("lab-offload".to_string()),
                cleanup: Some("preserve".to_string()),
                materialization: metadata.clone(),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: metadata.clone(),
        }],
    );
    submit_plan_in_store(lifecycle_store, &plan, Some(&run_id))?;
    let aggregate = AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Failed,
        totals: AgentTaskAggregateTotals {
            failed: 1,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes: vec![AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(failure.failure_message.to_string()),
            failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "lab-offload-pre-dispatch-failure".to_string(),
                uri: format!("homeboy://agent-task/run/{run_id}/logs"),
                label: Some("Lab offload pre-dispatch failure".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: json!({
                "schema": "homeboy/agent-task-predispatch-failure/v1",
                "runner_id": failure.identity.runner_id,
                "remote_workspace": failure.remote_workspace,
                "local_command": failure.local_command,
                "remote_command": failure.remote_command,
                "exit_code": failure.exit_code,
                "stdout": failure.stdout,
                "stderr": failure.stderr,
            }),
            workflow: None,
            follow_up: None,
            metadata,
        }],
        events: vec![
            AgentTaskProgressEvent {
                task_id: task_id.clone(),
                state: AgentTaskState::Queued,
                attempt: 1,
                message: Some("Lab offload selected and remote command prepared".to_string()),
            },
            AgentTaskProgressEvent {
                task_id,
                state: AgentTaskState::Failed,
                attempt: 1,
                message: Some(failure.failure_message.to_string()),
            },
        ],
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: AgentTaskQueueStatus {
            max_concurrency: 1,
            completed: 1,
            ..AgentTaskQueueStatus::default()
        },
    };
    record_run_aggregate_in_store(lifecycle_store, &run_id, &plan, &aggregate)
}

#[derive(Debug, Clone)]
pub struct AgentTaskRemoteDispatchFailure<'a> {
    pub identity: RunDispatchIdentity<'a>,
    pub local_command: Vec<String>,
    pub remote_command: Vec<String>,
    pub remote_workspace: &'a str,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: i32,
}

pub fn record_remote_dispatch_failure(
    failure: AgentTaskRemoteDispatchFailure<'_>,
    envelope: &Value,
) -> Result<Option<AgentTaskRunRecord>> {
    record_remote_dispatch_failure_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        failure,
        envelope,
    )
}

/// [`record_remote_dispatch_failure`] against an explicitly injected root.
///
/// The plan rewrite, the aggregate, and the record commit are one durable
/// failure. The body used to resolve a root for the submit/aggregate pair and
/// let four `store::` shims resolve their own for everything around it (#7505).
pub fn record_remote_dispatch_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    failure: AgentTaskRemoteDispatchFailure<'_>,
    envelope: &Value,
) -> Result<Option<AgentTaskRunRecord>> {
    if envelope.get("schema").and_then(Value::as_str) != Some("homeboy/agent-task-dispatch/v1") {
        return Ok(None);
    }

    let Some(aggregate_value) = envelope.get("aggregate") else {
        return Ok(None);
    };

    let run_id = sanitize_run_id(failure.identity.run_id);
    let mut aggregate: AgentTaskAggregate = serde_json::from_value(aggregate_value.clone())
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse offloaded agent-task dispatch aggregate".to_string()),
            )
        })?;
    enrich_remote_dispatch_aggregate(envelope, &mut aggregate);
    if aggregate.events.is_empty() {
        aggregate.events = events_for_outcomes(&aggregate.outcomes);
    }

    let (
        mut record,
        remote_run_id,
        remote_plan_path,
        remote_aggregate_path,
        needs_atomic_terminal_commit,
    ) = if let Some(record_value) = envelope.get("record") {
        let mut record: AgentTaskRunRecord =
            serde_json::from_value(record_value.clone()).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("parse offloaded agent-task dispatch record".to_string()),
                )
            })?;
        let remote_run_id = record.run_id.clone();
        let remote_plan_path = record.plan_path.clone();
        let remote_aggregate_path = record.aggregate_path.clone();
        let plan = if std::path::Path::new(&record.plan_path).is_file() {
            read_plan_path(&record.plan_path)?
        } else {
            synthetic_remote_dispatch_plan(&run_id, &failure, envelope, &aggregate)
        };
        record.run_id = run_id.clone();
        record.plan_path = lifecycle_store
            .write_controller_plan(&run_id, &plan)?
            .display()
            .to_string();
        apply_aggregate_to_record(
            &mut record,
            &plan,
            &aggregate,
            lifecycle_store
                .aggregate_path(&run_id)
                .display()
                .to_string(),
        );
        (
            record,
            remote_run_id,
            remote_plan_path,
            remote_aggregate_path,
            true,
        )
    } else {
        let remote_run_id = envelope
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or(failure.identity.run_id)
            .to_string();
        let remote_plan_path = envelope
            .get("plan_path")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                envelope
                    .get("plan_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&aggregate.plan_id)
                    .to_string()
            });
        let remote_aggregate_path = envelope
            .get("aggregate_path")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let plan = synthetic_remote_dispatch_plan(&run_id, &failure, envelope, &aggregate);
        // One store for both: the record this submits and the aggregate written
        // against it are one durable outcome (#7505).
        let mut record = submit_plan_in_store(lifecycle_store, &plan, Some(&run_id))?;
        record_aggregate_in_store(lifecycle_store, &mut record, &plan, &aggregate)?;
        (
            record,
            remote_run_id,
            remote_plan_path,
            remote_aggregate_path,
            false,
        )
    };

    let provider_run_ids: Vec<String> = record
        .provider_handles
        .iter()
        .map(|handle| handle.provider_run_id.clone())
        .collect();
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "kind".to_string(),
        json!("lab_offload_remote_dispatch_failure"),
    );
    metadata.insert("runner_id".to_string(), json!(failure.identity.runner_id));
    metadata.insert(
        "remote_workspace".to_string(),
        json!(failure.remote_workspace),
    );
    metadata.insert("local_command".to_string(), json!(failure.local_command));
    metadata.insert("remote_command".to_string(), json!(failure.remote_command));
    metadata.insert("exit_code".to_string(), json!(failure.exit_code));
    metadata.insert("stdout".to_string(), json!(failure.stdout));
    metadata.insert("stderr".to_string(), json!(failure.stderr));
    metadata.insert("remote_run_id".to_string(), json!(remote_run_id));
    metadata.insert("remote_plan_path".to_string(), json!(remote_plan_path));
    metadata.insert(
        "remote_aggregate_path".to_string(),
        json!(remote_aggregate_path),
    );
    metadata.insert("provider_run_ids".to_string(), json!(provider_run_ids));

    if needs_atomic_terminal_commit {
        lifecycle_store.write_aggregate_and_record(&record, &aggregate)?;
    } else {
        lifecycle_store.write_record(&record)?;
    }
    Ok(Some(record))
}

fn enrich_remote_dispatch_aggregate(envelope: &Value, aggregate: &mut AgentTaskAggregate) {
    let remote_run_id = envelope.get("run_id").and_then(Value::as_str);
    for outcome in &mut aggregate.outcomes {
        normalize_provider_run_result(outcome);

        if outcome.evidence_refs.is_empty() {
            if let Some(remote_run_id) = remote_run_id {
                outcome.evidence_refs.extend([
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-logs".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/logs"),
                        label: Some("Remote agent-task logs".to_string()),
                    },
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-review".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/review"),
                        label: Some("Remote agent-task review".to_string()),
                    },
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-artifacts".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/artifacts"),
                        label: Some("Remote agent-task artifacts".to_string()),
                    },
                ]);
            }
        }
    }
}

fn synthetic_remote_dispatch_plan(
    run_id: &str,
    failure: &AgentTaskRemoteDispatchFailure<'_>,
    envelope: &Value,
    aggregate: &AgentTaskAggregate,
) -> AgentTaskPlan {
    let tasks = aggregate
        .outcomes
        .iter()
        .map(|outcome| {
            let provider = outcome
                .metadata
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("homeboy-lab");
            AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: outcome.task_id.clone(),
                group_key: Some("lab-offload".to_string()),
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: provider.to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: outcome.summary.clone().unwrap_or_else(|| {
                    "Persist remote Lab agent-task dispatch outcome.".to_string()
                }),
                inputs: json!({
                    "remote_dispatch_envelope": envelope,
                    "remote_command": failure.remote_command,
                }),
                source_refs: vec![AgentTaskSourceRef {
                    kind: "lab-offload-remote-dispatch".to_string(),
                    uri: envelope
                        .get("run_id")
                        .and_then(Value::as_str)
                        .map(|remote_run_id| format!("homeboy://agent-task/run/{remote_run_id}"))
                        .unwrap_or_else(|| {
                            format!("homeboy://agent-task/run/{run_id}/lab-offload")
                        }),
                    revision: envelope
                        .get("plan_id")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                }],
                workspace: AgentTaskWorkspace {
                    mode: AgentTaskWorkspaceMode::Existing,
                    root: Some(failure.remote_workspace.to_string()),
                    slug: None,
                    kind: Some("lab-offload".to_string()),
                    component_id: None,
                    branch: None,
                    base_ref: None,
                    task_url: None,
                    cleanup: Some("preserve".to_string()),
                    attempt: None,
                    materialization: json!({
                        "runner_id": failure.identity.runner_id,
                        "remote_workspace": failure.remote_workspace,
                    }),
                },
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                output_declarations: Vec::new(),
                runtime_tools: Vec::new(),
                metadata: outcome.metadata.clone(),
            }
        })
        .collect();

    let mut plan = AgentTaskPlan::new(
        envelope
            .get("plan_id")
            .and_then(Value::as_str)
            .unwrap_or(&aggregate.plan_id),
        tasks,
    );
    plan.group_key = Some("lab-offload".to_string());
    plan.metadata = json!({
        "kind": "lab_offload_remote_dispatch_failure",
        "runner_id": failure.identity.runner_id,
        "remote_workspace": failure.remote_workspace,
        "remote_run_id": envelope.get("run_id").and_then(Value::as_str),
    });
    plan
}

// The ambient `record_aggregate()` shim that used to sit here is gone; the
// remote-dispatch failure recorder was its only caller and now writes the
// aggregate into the same store it submitted the record to (#7505).

pub(crate) fn record_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let aggregate_path = lifecycle_store.aggregate_path(&record.run_id);
    apply_aggregate_to_record(
        record,
        plan,
        aggregate,
        aggregate_path.display().to_string(),
    );
    let mut retained_roots = Vec::new();
    let roots: Vec<PathBuf> = plan
        .tasks
        .iter()
        .filter_map(|task| task.workspace.root.as_ref())
        .filter_map(|root| {
            let path = PathBuf::from(root);
            if path.is_dir() {
                Some(path)
            } else {
                retained_roots.push(serde_json::json!({
                    "path": root,
                    "reason": "workspace root is inaccessible from this controller",
                }));
                None
            }
        })
        .collect();
    record.ensure_metadata_object();
    if !roots.is_empty() {
        record.metadata["automatic_artifact_retention"] =
            match homeboy_core::cleanup::try_run_automatic_artifact_retention(roots) {
                Ok(Some(output)) => serde_json::to_value(output).unwrap_or_else(|error| {
                    serde_json::json!({ "status": "retained", "reason": error.to_string() })
                }),
                Ok(None) => serde_json::json!({
                    "status": "busy",
                    "reason": "automatic artifact retention is already running",
                }),
                Err(error) => serde_json::json!({ "status": "retained", "reason": error.message }),
            };
    }
    if !retained_roots.is_empty() {
        record.metadata["automatic_artifact_retention_inaccessible_roots"] =
            serde_json::Value::Array(retained_roots);
    }
    crate::controller_scratch::register_outcome_resources_at(
        &lifecycle_store.data_root(),
        &record.run_id,
        &aggregate.outcomes,
    )?;
    crate::controller_scratch::finalize_run_at(&lifecycle_store.data_root(), &record.run_id)?;
    lifecycle_store.write_aggregate_and_record(record, aggregate)?;
    record_terminal_artifact_projection_in_store(lifecycle_store, record, aggregate)?;
    // The Cook index this completion updates belongs to the same store the
    // aggregate was just committed into. Resolving it ambiently made this
    // rooted function write its substantive-candidate pointer into whatever
    // home the environment pointed at, which no positive assertion here can
    // see: every record and aggregate read back from the injected store is
    // already correct at that point (#7505).
    update_cook_candidate_after_completion_in_store(lifecycle_store, record, aggregate, None)?;
    Ok(record.clone())
}

/// Register a terminal run's artifacts into its own store's projection root.
///
/// There is no ambient wrapper: the last caller that resolved its own root was
/// `project_terminal_runner_lifecycle_event`, and it now takes a store (#7505).
/// Leaving an unused ambient form behind would only be a new way to reach the
/// process artifact root by accident.
pub(crate) fn record_terminal_artifact_projection_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let recovery_command = format!("homeboy agent-task status {}", record.run_id);
    if record.runner_id().is_none() && aggregate_has_runner_backed_actionable_patch(aggregate) {
        match runner_id_from_artifact_provenance(aggregate) {
            Ok(runner_id) => {
                record
                    .ensure_metadata_object()
                    .insert("runner_id".to_string(), json!(runner_id));
            }
            Err(error) => {
                let status = if error.retryable == Some(true) {
                    "pending"
                } else {
                    "failed"
                };
                record.ensure_metadata_object().insert(
                    "artifact_projection".to_string(),
                    json!({
                        "status": status,
                        "error": error.message,
                        "recovery_action": {
                            "kind": "fetch_and_reconcile",
                            "command": recovery_command,
                        },
                    }),
                );
                return lifecycle_store.write_record(record);
            }
        }
    }
    let observation_store = lifecycle_store.open_observation_initialized()?;
    match project_terminal_artifacts_in_store(&observation_store, record, aggregate) {
        Ok(()) => {
            record.ensure_metadata_object().insert(
                "artifact_projection".to_string(),
                json!({ "status": "complete" }),
            );
        }
        Err(error) => {
            let status = if error.retryable == Some(true) {
                "pending"
            } else {
                "failed"
            };
            record.ensure_metadata_object().insert(
                "artifact_projection".to_string(),
                json!({
                    "status": status,
                    "error": error.message,
                    "recovery_action": {
                        "kind": "fetch_and_reconcile",
                        "command": recovery_command,
                    },
                }),
            );
        }
    }
    lifecycle_store.write_record(record)
}

/// Replace runner-local file references with controller-resolvable aggregate
/// references before persisting a terminal runner result. The original location
/// is deliberately not retained as a durable URI: it is meaningful only on the
/// producing runner and would make a controller attempt local IO against it.
pub(crate) fn project_runner_evidence_refs(
    record: &AgentTaskRunRecord,
    aggregate: &mut AgentTaskAggregate,
) {
    let Some(runner_id) = record
        .runner_id()
        .filter(|runner_id| !runner_id.trim().is_empty())
    else {
        return;
    };
    let runner_job_id = record
        .runner_job_id()
        .filter(|job_id| !job_id.trim().is_empty());
    let encoded_run_id = homeboy_core::execution_contract::encode_uri_component(&record.run_id);

    for outcome in &mut aggregate.outcomes {
        let mut projected = Vec::new();
        for evidence in &mut outcome.evidence_refs {
            if !evidence.uri.starts_with("file://") {
                continue;
            }
            let reference_digest = content_hash::sha256_hex(evidence.uri.as_bytes());
            let encoded_task_id =
                homeboy_core::execution_contract::encode_uri_component(&outcome.task_id);
            evidence.uri = format!(
                "homeboy://agent-task/run/{encoded_run_id}/aggregate#outcome={encoded_task_id}&evidence={reference_digest}"
            );
            projected.push((reference_digest, evidence.kind.clone()));
        }
        if projected.is_empty() {
            continue;
        }
        if !outcome.metadata.is_object() {
            outcome.metadata = json!({});
        }
        if !outcome.metadata["runner_evidence_projection"].is_object() {
            outcome.metadata["runner_evidence_projection"] = json!({});
        }
        let projections = outcome.metadata["runner_evidence_projection"]
            .as_object_mut()
            .expect("runner evidence projection is an object");
        for (reference_digest, kind) in projected {
            projections.insert(
                reference_digest,
                json!({
                    "kind": kind,
                    "source_runner_id": runner_id,
                    "source_runner_job_id": runner_job_id,
                    "retention": "controller_aggregate",
                    "redaction": "producer_redacted",
                }),
            );
        }
    }
}

/// The authoritative model recorded on an aggregate outcome, if any.
///
/// Locates the outcome for `task_id` and reads its concrete model through the
/// canonical [`AgentTaskOutcome::selected_model`] reader, so aggregate → task
/// model reconciliation uses the same present/non-blank definition as every
/// other model-resolution site.
fn aggregate_selected_model(aggregate: &AgentTaskAggregate, task_id: &str) -> Option<String> {
    aggregate
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == task_id)
        .and_then(|outcome| outcome.selected_model())
        .map(str::to_string)
}

/// True when a terminal record's durable lifecycle provider model is missing but
/// the authoritative aggregate outcome recorded a concrete model — the exact
/// stale-terminal state that blocks `finalize-pr` after the #9404/#9405 repair
/// (#9411). The nonterminal `status()` path already normalizes this via
/// `apply_aggregate_to_record`, but a record that went terminal *before* that
/// repair never gets reprojected on read.
pub(crate) fn terminal_provider_model_reconciliation_needed(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> bool {
    record.tasks.iter().any(|task| {
        let durable_missing = task
            .model
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty);
        durable_missing && aggregate_selected_model(aggregate, &task.task_id).is_some()
    })
}

/// Reproject authoritative aggregate model evidence onto a terminal record when
/// the durable lifecycle model is stale-null (#9411).
///
/// Fills only the previously-null provider model — on the run tasks, provider
/// handles, and the rebuilt lifecycle projection — and persists. Terminal state,
/// promotion evidence, gates, totals, and artifact projections are preserved:
/// `set_run_state` re-asserts the existing terminal state and only the
/// model-bearing lifecycle projection is rebuilt from the record's own tasks.
///
/// The persist at the end is the whole point of taking a store: it is the only
/// durable effect, so a reconciliation driven from injected roots must land its
/// repaired model in the same installation the record and aggregate were read
/// from. There is no ambient wrapper — `reconcile_status_in_store` is the only caller,
/// and the store it hands down is the one its own caller injected.
pub(crate) fn reconcile_terminal_provider_model_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let mut changed = false;
    for task in &mut record.tasks {
        if task
            .model
            .as_deref()
            .map(str::trim)
            .is_some_and(|model| !model.is_empty())
        {
            continue;
        }
        if let Some(model) = aggregate_selected_model(aggregate, &task.task_id) {
            task.model = Some(model);
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    persist_provider_handle_models(&mut record.provider_handles, plan);
    update_lifecycle_from_record(record, plan);
    record.updated_at = Some(now_timestamp());
    lifecycle_store.write_record(record)
}

/// Recover the runner identity for canonical legacy patch artifacts. Diagnostic
/// artifacts can share an aggregate without participating in promotion.
fn runner_id_from_artifact_provenance(aggregate: &AgentTaskAggregate) -> Result<String> {
    let runner_ids = aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.artifacts)
        .filter(|artifact| {
            crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
                && artifact.size_bytes.is_some()
                && artifact.sha256.is_some()
        })
        .map(|artifact| {
            artifact_runner_id(artifact)
                .map(str::to_string)
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "artifact.metadata.source_provenance.runner_id",
                        "cannot recover a controller artifact projection without unambiguous runner provenance",
                        Some(artifact.id.clone()),
                        None,
                    )
                })
        })
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    match runner_ids.into_iter().collect::<Vec<_>>().as_slice() {
        [runner_id] => Ok(runner_id.clone()),
        _ => Err(Error::validation_invalid_argument(
            "artifact.metadata.source_provenance.runner_id",
            "cannot recover a controller artifact projection without unambiguous runner provenance",
            None,
            None,
        )),
    }
}

fn artifact_runner_id(artifact: &AgentTaskArtifact) -> Option<&str> {
    artifact
        .metadata
        .pointer("/source_provenance/runner_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|runner_id| !runner_id.is_empty())
}

fn validate_artifact_runner_binding(
    artifact: &AgentTaskArtifact,
    record_runner_id: &str,
) -> Result<()> {
    if artifact_runner_id(artifact).is_some_and(|runner_id| runner_id != record_runner_id) {
        return Err(Error::validation_invalid_argument(
            "artifact.metadata.source_provenance.runner_id",
            format!(
                "artifact '{}' runner provenance conflicts with lifecycle runner binding '{}'",
                artifact.id, record_runner_id
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    Ok(())
}

fn aggregate_has_runner_backed_actionable_patch(aggregate: &AgentTaskAggregate) -> bool {
    aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.artifacts)
        .any(|artifact| {
            crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
                && artifact.path.as_deref().is_none_or(|path| {
                    let path = Path::new(path);
                    path.is_absolute() && !path.is_file()
                })
                && artifact.size_bytes.is_some()
                && artifact.sha256.is_some()
        })
}

pub(crate) fn terminal_artifact_projection_is_verified(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<bool> {
    terminal_artifact_projection_is_verified_with(record, aggregate, || {
        homeboy_core::observation::ObservationStore::open_initialized()
    })
}

/// [`terminal_artifact_projection_is_verified`] against an explicitly injected
/// durable lifecycle root.
///
/// `open_observation_initialized` binds this store's observation database *and*
/// the artifact root it carries, and the second binding is the reason this
/// sibling has to exist. `PathRoots` keeps `artifacts` separate from `data`, and
/// `ObservationStore::open_initialized` resolves both from the process — so a
/// projection verified ambiently looks for controller-owned bytes under
/// whichever artifact root the environment names, and reports an
/// otherwise-complete candidate as unprojected purely because it was asked from
/// the wrong home (#7505, #12618, #12619).
pub(crate) fn terminal_artifact_projection_is_verified_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<bool> {
    terminal_artifact_projection_is_verified_with(record, aggregate, || {
        lifecycle_store.open_observation_initialized()
    })
}

/// The shared body of both forms above.
///
/// `open_observations` is invoked at exactly the point the ambient form used to
/// open its store — lazily, inside the loop, only once an artifact actually
/// requires a durable projection — so neither form initializes an observation
/// database it would not have touched before.
fn terminal_artifact_projection_is_verified_with(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
    mut open_observations: impl FnMut() -> Result<homeboy_core::observation::ObservationStore>,
) -> Result<bool> {
    for outcome in &aggregate.outcomes {
        for artifact in &outcome.artifacts {
            if requires_durable_lab_projection(artifact) {
                if artifact.size_bytes.is_none()
                    || artifact.sha256.is_none()
                    || (artifact.path.is_none() && record.runner_id().is_none())
                {
                    return Ok(false);
                }
                if verified_controller_artifact_projection_path_in_store(
                    &open_observations()?,
                    &record.run_id,
                    &outcome.task_id,
                    artifact,
                )?
                .is_none()
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

/// Return a repairable diagnostic when an actionable patch is not yet readable
/// from a verified controller-owned projection. Missing aggregates remain
/// outside this check so historical terminal records keep their existing
/// recovery behavior.
pub(crate) fn terminal_artifact_projection_readiness(run_id: &str) -> Result<Option<String>> {
    let record = store::read_record(&super::sanitize_run_id(run_id))?;
    terminal_artifact_projection_readiness_for_record(
        &record,
        store::read_aggregate(&record.run_id),
    )
}

/// [`terminal_artifact_projection_readiness`] against an explicitly injected
/// durable lifecycle root.
///
/// All three reads follow the injected store, and the third is the one that
/// matters: the projection check opens an observation database *and* resolves
/// an artifact root, which `PathRoots` carries separately from `data`. Answered
/// ambiently, an otherwise-complete candidate is reported as unprojected purely
/// because the controller-owned bytes were looked for under the wrong home
/// (#7505, #12618, #12619) — and here that verdict is not merely cosmetic: the
/// caller turns it into a `cook_continuation_scheduler` status that suppresses
/// the terminal continuation enqueue.
///
/// This is the initializing counterpart of
/// [`terminal_artifact_projection_readiness_bounded_in_store`]; it opens the
/// same stores the ambient form opens rather than the read-only ones.
pub fn terminal_artifact_projection_readiness_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<String>> {
    let record = lifecycle_store.read_record(&super::sanitize_run_id(run_id))?;
    terminal_artifact_projection_readiness_for_record_with(
        &record,
        lifecycle_store.read_aggregate(&record.run_id),
        |record, aggregate| {
            terminal_artifact_projection_is_verified_in_store(lifecycle_store, record, aggregate)
        },
    )
}

/// Bounded read-only counterpart used by fanout status while coordinators are
/// writing observations. It intentionally leaves reconciliation to `resume`.
pub(crate) fn terminal_artifact_projection_readiness_bounded(
    run_id: &str,
) -> Result<Option<String>> {
    let record = store::read_record_bounded(&super::sanitize_run_id(run_id))?;
    terminal_artifact_projection_readiness_for_record(
        &record,
        store::read_aggregate_bounded(&record.run_id),
    )
}

/// [`terminal_artifact_projection_readiness_bounded`] against an explicitly
/// injected durable lifecycle root.
///
/// All three reads follow the injected store. The `store::` shims above are
/// exactly `default_store()?.read_record_bounded` and
/// `read_aggregate_bounded_in_store(&default_store()?, ..)`, and
/// `default_store()` is `AgentTaskLifecycleStore::from_current_environment()`,
/// so an ambient caller reaches byte-identical state through either form. The
/// third is the projection check itself: it opens an observation store and
/// resolves an artifact root, which `PathRoots` carries separately from `data` —
/// see `terminal_artifact_projection_is_verified_in_store`.
pub fn terminal_artifact_projection_readiness_bounded_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<String>> {
    let record = lifecycle_store.read_record_bounded(&super::sanitize_run_id(run_id))?;
    terminal_artifact_projection_readiness_for_record_with(
        &record,
        lifecycle_store.read_aggregate_bounded(&record.run_id),
        |record, aggregate| {
            terminal_artifact_projection_is_verified_in_store(lifecycle_store, record, aggregate)
        },
    )
}

fn terminal_artifact_projection_readiness_for_record(
    record: &AgentTaskRunRecord,
    aggregate: Result<AgentTaskAggregate>,
) -> Result<Option<String>> {
    terminal_artifact_projection_readiness_for_record_with(
        record,
        aggregate,
        terminal_artifact_projection_is_verified,
    )
}

/// The shared body of both readiness forms. `verified` decides which durable
/// roots the projection check itself is answered against; everything else here
/// is derived from the record and aggregate already in hand.
fn terminal_artifact_projection_readiness_for_record_with(
    record: &AgentTaskRunRecord,
    aggregate: Result<AgentTaskAggregate>,
    verified: impl FnOnce(&AgentTaskRunRecord, &AgentTaskAggregate) -> Result<bool>,
) -> Result<Option<String>> {
    let Ok(aggregate) = aggregate else {
        return Ok(None);
    };
    if verified(record, &aggregate)? {
        return Ok(None);
    }
    Ok(Some(
        record
            .metadata
            .get("artifact_projection")
            .and_then(|projection| projection.get("error"))
            .and_then(Value::as_str)
            .unwrap_or(
                "an actionable patch is missing a readable controller projection or required path, size, and SHA-256 integrity metadata",
            )
            .to_string(),
    ))
}

fn project_terminal_artifacts_in_store(
    store: &homeboy_core::observation::ObservationStore,
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let status = match record.state {
        AgentTaskRunState::Succeeded => "pass",
        AgentTaskRunState::CandidateRecoverable => "fail",
        AgentTaskRunState::PartialRecoverable => "fail",
        AgentTaskRunState::PartialFailure => "fail",
        AgentTaskRunState::Failed => "fail",
        AgentTaskRunState::Cancelled => "fail",
        _ => return Ok(()),
    };
    let existing = store.get_run(&record.run_id)?;
    let homeboy_version = existing
        .as_ref()
        .map(|run| run.homeboy_version.clone())
        .unwrap_or_else(|| Some(homeboy_core::build_identity::current().version));
    let mut existing_metadata = existing
        .map(|run| run.metadata_json)
        .unwrap_or_else(|| json!({ "agent_task_run": record.run_id }));
    if !existing_metadata.is_object() {
        existing_metadata = json!({});
    }
    existing_metadata
        .as_object_mut()
        .expect("object checked above")
        .insert("agent_task_terminal_state".to_string(), json!(record.state));
    store.upsert_imported_run_preserving_terminal(&homeboy_core::observation::RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: None,
        started_at: record.submitted_at.clone(),
        finished_at: record.updated_at.clone(),
        status: status.to_string(),
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version,
        git_sha: None,
        rig_id: None,
        metadata_json: existing_metadata,
    })?;

    validate_unique_terminal_artifact_ids(aggregate)?;
    let mut projection_error = None;
    for outcome in &aggregate.outcomes {
        for artifact in &outcome.artifacts {
            let actionable =
                crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact);
            let remote_runner = record.runner_id().filter(|runner_id| {
                super::lifecycle_ops::execution_runner_id().as_deref() != Some(*runner_id)
            });
            // Only artifacts consumed after terminalization need controller
            // bytes. Other declarations remain review metadata and can name a
            // producer-local path that no longer exists.
            if !requires_durable_lab_projection(artifact) {
                continue;
            }
            if actionable
                && (artifact.size_bytes.is_none()
                    || artifact.sha256.is_none()
                    || (artifact.path.is_none() && remote_runner.is_none()))
            {
                return Err(Error::validation_invalid_argument(
                    "artifact_projection",
                    format!(
                        "actionable patch for run '{}', task '{}', and artifact '{}' requires a local path or authenticated runner binding, plus size and SHA-256, before controller projection",
                        record.run_id, outcome.task_id, artifact.id
                    ),
                    Some(artifact.id.clone()),
                    None,
                ));
            }
            if artifact.path.is_none() && remote_runner.is_none() {
                continue;
            }
            if artifact.size_bytes.is_none() || artifact.sha256.is_none() {
                // Unreadable/remote declarations remain visible to review only.
                continue;
            }
            validate_projection_token("artifact.id", &artifact.id)?;
            validate_projection_token("artifact.kind", &artifact.kind)?;
            let base_id = artifact.id.trim();
            let logical_id = base_id;
            // Observation artifact ids are globally unique. Keep the lifecycle
            // logical id as the per-run lookup token exposed by runs artifact.
            let mut id_hash = sha2::Sha256::new();
            sha2::Digest::update(&mut id_hash, record.run_id.as_bytes());
            sha2::Digest::update(&mut id_hash, [0]);
            sha2::Digest::update(&mut id_hash, outcome.task_id.as_bytes());
            sha2::Digest::update(&mut id_hash, [0]);
            sha2::Digest::update(&mut id_hash, logical_id.as_bytes());
            let artifact_id = format!("agent-task-{:x}", id_hash.finalize());
            let mut metadata = json!({
                "name": logical_id,
                "agent_task": {
                    "task_id": outcome.task_id,
                    "logical_artifact_id": logical_id,
                    "runner_provenance": artifact.metadata,
                }
            });
            if remote_runner.is_none() {
                // A local artifact record is copied into the observation store
                // before it can be reused, so tag that retained controller copy
                // before the idempotency check below.
                metadata["agent_task"]["projection"] = json!("controller_local");
            }
            // Prefer pre-existing executor-finalized bytes over a legacy
            // direct import. The import remains evidence, while the finalized
            // projection keeps its established derived controller identity.
            let finalized_path = remote_runner
                .map(|_| controller_finalized_artifact_path(store, artifact))
                .transpose()?
                .flatten();
            if finalized_path.is_some() {
                stamp_legacy_artifact_provenance(
                    store,
                    &record.run_id,
                    &outcome.task_id,
                    artifact,
                    &artifact_id,
                )?;
            }
            if finalized_path.is_none()
                && reusable_terminal_artifact(
                    &store,
                    &record.run_id,
                    &outcome.task_id,
                    artifact,
                    &artifact_id,
                    &metadata,
                )?
            {
                continue;
            }
            if let Some(runner_id) = remote_runner {
                if runner_id.trim().is_empty() {
                    return Err(Error::validation_invalid_argument(
                        "runner_id",
                        "runner id cannot be empty when creating a runner artifact reference",
                        None,
                        None,
                    ));
                }
                validate_artifact_runner_binding(artifact, runner_id)?;
                if let Some(path) = finalized_path {
                    let mut controller_hash = sha2::Sha256::new();
                    sha2::Digest::update(&mut controller_hash, b"controller");
                    sha2::Digest::update(&mut controller_hash, [0]);
                    sha2::Digest::update(&mut controller_hash, artifact_id.as_bytes());
                    let controller_artifact_id =
                        format!("agent-task-{:x}", controller_hash.finalize());
                    let mut metadata = metadata;
                    metadata["agent_task"]["projection"] = json!("controller_finalized");
                    store.record_verified_artifact_with_id(
                        &record.run_id,
                        &artifact.kind,
                        path,
                        &controller_artifact_id,
                        artifact
                            .size_bytes
                            .and_then(|size| i64::try_from(size).ok()),
                        artifact.sha256.as_deref(),
                        metadata,
                    )?;
                } else {
                    let remote_ref = homeboy_core::execution_contract::EXECUTION_CONTRACT
                        .artifacts
                        .runner_artifact_ref(runner_id, &record.run_id, logical_id);
                    let mirror_result = if requires_durable_lab_projection(artifact) {
                        (|| -> Result<()> {
                            let mirror = tempfile::NamedTempFile::new().map_err(|error| {
                                Error::internal_io(
                                    error.to_string(),
                                    Some("create controller artifact mirror".to_string()),
                                )
                            })?;
                            let download =
                                homeboy_core::observation::runs_service::runner_evidence::with_runner_evidence(
                                    |p| {
                                        p.download_remote_artifact(
                                            &remote_ref,
                                            Some(mirror.path().to_path_buf()),
                                        )
                                    },
                                )
                                .map_err(|error| error.with_retryable(true))?;
                            let expected_size = artifact.size_bytes.expect("checked above");
                            let expected_sha256 =
                                artifact.sha256.as_deref().expect("checked above");
                            let actual_size = std::fs::metadata(&download.output_path)
                                .map_err(|error| {
                                    Error::internal_io(
                                        error.to_string(),
                                        Some("inspect controller artifact mirror".to_string()),
                                    )
                                })?
                                .len();
                            let actual_sha256 = homeboy_core::artifact_metadata::sha256_file(
                                &download.output_path,
                            )?;
                            if actual_size != expected_size || actual_sha256 != expected_sha256 {
                                return Err(Error::validation_invalid_argument(
                                    "artifact_id",
                                    format!(
                                        "runner artifact mirror for run '{}', task '{}', and artifact '{}' does not match the aggregate SHA-256 and size",
                                        record.run_id, outcome.task_id, artifact.id
                                    ),
                                    Some(artifact.id.clone()),
                                    None,
                                ));
                            }
                            let mut controller_hash = sha2::Sha256::new();
                            sha2::Digest::update(&mut controller_hash, b"controller");
                            sha2::Digest::update(&mut controller_hash, [0]);
                            sha2::Digest::update(&mut controller_hash, artifact_id.as_bytes());
                            let controller_artifact_id =
                                format!("agent-task-{:x}", controller_hash.finalize());
                            let mut controller_metadata = metadata.clone();
                            controller_metadata["agent_task"]["projection"] =
                                json!("runner_mirrored");
                            store.record_verified_artifact_with_id(
                                &record.run_id,
                                &artifact.kind,
                                &download.output_path,
                                &controller_artifact_id,
                                Some(expected_size as i64),
                                Some(expected_sha256),
                                controller_metadata,
                            )?;
                            Ok(())
                        })()
                    } else {
                        Ok(())
                    };

                    // Preserve the canonical runner retrieval alias even when
                    // the controller also materializes verified bytes.
                    store.import_artifact(&homeboy_core::observation::ArtifactRecord {
                        id: artifact_id,
                        run_id: record.run_id.clone(),
                        kind: artifact.kind.clone(),
                        artifact_type: "remote_file".to_string(),
                        path: remote_ref,
                        url: None,
                        public_url: None,
                        viewer_url: None,
                        viewer_links: Vec::new(),
                        sha256: artifact.sha256.clone(),
                        size_bytes: artifact
                            .size_bytes
                            .and_then(|value| i64::try_from(value).ok()),
                        mime: artifact.mime.clone(),
                        metadata_json: metadata,
                        created_at: chrono::Utc::now().to_rfc3339(),
                    })?;
                    if let Err(error) = mirror_result {
                        projection_error.get_or_insert(error);
                    }
                }
            } else {
                let path = artifact.path.as_deref().expect("local path checked above");
                let path = controller_projected_local_artifact_path(
                    store,
                    &record.run_id,
                    &outcome.task_id,
                    &artifact.id,
                    path,
                    artifact.size_bytes.expect("checked above"),
                    artifact.sha256.as_deref().expect("checked above"),
                )?;
                store.record_verified_artifact_with_id(
                    &record.run_id,
                    &artifact.kind,
                    &path,
                    &artifact_id,
                    artifact
                        .size_bytes
                        .and_then(|size| i64::try_from(size).ok()),
                    artifact.sha256.as_deref(),
                    metadata,
                )?;
            }
        }
    }
    projection_error.map_or(Ok(()), Err)
}

/// Copy local producer output under controller ownership before publishing its
/// projection. The aggregate path remains provenance only and may disappear
/// when the producer workspace is cleaned up.
fn controller_projected_local_artifact_path(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact_id: &str,
    source: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<std::path::PathBuf> {
    let bytes = std::fs::read(source).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read local artifact {source}")),
        )
    })?;
    if bytes.len() as u64 != expected_size
        || homeboy_engine_primitives::content_hash::sha256_hex(&bytes) != expected_sha256
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "local artifact does not match its declared SHA-256 and size",
            Some(artifact_id.to_string()),
            None,
        ));
    }
    let path = store
        .artifact_root()?
        .join("controller-projected-agent-task-artifacts")
        .join(homeboy_core::paths::sanitize_path_segment(run_id))
        .join(homeboy_core::paths::sanitize_path_segment(task_id))
        .join(homeboy_core::paths::sanitize_path_segment(artifact_id));
    std::fs::create_dir_all(path.parent().expect("projection path parent")).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller artifact projection".to_string()),
        )
    })?;
    std::fs::write(&path, bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "write controller artifact projection {}",
                path.display()
            )),
        )
    })?;
    Ok(path)
}

fn requires_durable_lab_projection(artifact: &AgentTaskArtifact) -> bool {
    crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
        || matches!(
            artifact.kind.as_str(),
            "transcript" | "result" | "agent-result" | "agent_result"
        )
}

/// A direct artifact import can retain the same deterministic lifecycle id
/// before terminal reconciliation. Reuse it only when its controller-local
/// bytes prove it belongs to this artifact projection.
fn reusable_terminal_artifact(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
    artifact_id: &str,
    metadata: &Value,
) -> Result<bool> {
    let Some(existing) = store.get_artifact(artifact_id)? else {
        return Ok(false);
    };
    if existing.artifact_type != "file" {
        return Ok(false);
    }

    let expected_size = i64::try_from(artifact.size_bytes.expect("checked above")).ok();
    let expected_sha256 = artifact.sha256.as_deref().expect("checked above");
    let matches = existing.run_id == run_id
        && existing.size_bytes == expected_size
        && existing.sha256.as_deref() == Some(expected_sha256)
        && std::fs::metadata(&existing.path)
            .map(|metadata| {
                metadata.is_file() && i64::try_from(metadata.len()).ok() == expected_size
            })
            .unwrap_or(false)
        && homeboy_core::artifact_metadata::sha256_file(Path::new(&existing.path))
            .ok()
            .as_deref()
            == Some(expected_sha256);
    if matches {
        let existing_task_id = existing
            .metadata_json
            .pointer("/agent_task/task_id")
            .and_then(Value::as_str);
        let existing_logical_id = existing
            .metadata_json
            .pointer("/agent_task/logical_artifact_id")
            .and_then(Value::as_str);
        if existing_task_id.is_some_and(|value| value != task_id)
            || existing_logical_id.is_some_and(|value| value != artifact.id)
        {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!(
                    "existing artifact record conflicts with terminal artifact projection: {artifact_id}"
                ),
                Some(artifact_id.to_string()),
                None,
            ));
        }
        if controller_owned_artifact_path(store, Path::new(&existing.path)) {
            // Pre-marker controller projections already have durable storage;
            // retain their record and stamp the current lookup metadata.
            store.update_artifact_metadata(artifact_id, metadata.clone())?;
            return Ok(true);
        }

        // A directly imported observation artifact is durable controller
        // evidence even when its original path was outside the artifact root.
        // Preserve that record and derive a separate controller-local copy
        // before allowing terminal recovery to use its verified bytes.
        let path = controller_projected_local_artifact_path(
            store,
            run_id,
            task_id,
            &artifact.id,
            &existing.path,
            artifact.size_bytes.expect("checked above"),
            expected_sha256,
        )?;
        let controller_artifact_id = controller_projection_artifact_id(artifact_id);
        store.record_verified_artifact_with_id(
            run_id,
            &artifact.kind,
            path,
            &controller_artifact_id,
            expected_size,
            Some(expected_sha256),
            metadata.clone(),
        )?;
        return Ok(true);
    }

    Err(Error::validation_invalid_argument(
        "artifact_id",
        format!(
            "existing artifact record conflicts with terminal artifact projection: {artifact_id}"
        ),
        Some(artifact_id.to_string()),
        None,
    ))
}

/// Declared authority of one controller-side projection.
///
/// Terminal projection deliberately keeps a legacy import as evidence while the
/// finalized record carries the derived controller identity, so both records can
/// legitimately share a task and logical artifact id. Resolution therefore
/// selects by declared authority rather than by discovery order.
fn controller_projection_precedence(
    record: &homeboy_core::observation::ArtifactRecord,
) -> (u8, &str) {
    let rank = match record
        .metadata_json
        .pointer("/agent_task/projection")
        .and_then(Value::as_str)
    {
        Some("controller_finalized") => 0,
        Some("controller_local") => 1,
        Some("runner_mirrored") => 2,
        _ => 3,
    };
    (rank, record.id.as_str())
}

/// Reduce equivalent controller projections of one logical artifact to the
/// single authoritative record. Candidates that disagree on content identity
/// are a real conflict and still fail closed, naming the records involved.
fn select_controller_artifact_projection(
    candidates: Vec<homeboy_core::observation::ArtifactRecord>,
) -> std::result::Result<homeboy_core::observation::ArtifactRecord, Vec<String>> {
    let identity = |record: &homeboy_core::observation::ArtifactRecord| {
        (record.sha256.clone(), record.size_bytes)
    };
    if let Some(first) = candidates.first().map(&identity) {
        if candidates.iter().any(|record| identity(record) != first) {
            let mut conflicting: Vec<String> =
                candidates.iter().map(|record| record.id.clone()).collect();
            conflicting.sort();
            return Err(conflicting);
        }
    }
    candidates
        .into_iter()
        .min_by(|left, right| {
            controller_projection_precedence(left).cmp(&controller_projection_precedence(right))
        })
        .ok_or_else(Vec::new)
}

fn controller_projection_artifact_id(artifact_id: &str) -> String {
    let mut controller_hash = sha2::Sha256::new();
    sha2::Digest::update(&mut controller_hash, b"controller");
    sha2::Digest::update(&mut controller_hash, [0]);
    sha2::Digest::update(&mut controller_hash, artifact_id.as_bytes());
    format!("agent-task-{:x}", controller_hash.finalize())
}

/// Retain legacy imported evidence while making its lifecycle association
/// explicit. This never grants projection authority to its external path.
fn stamp_legacy_artifact_provenance(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
    artifact_id: &str,
) -> Result<()> {
    let Some(existing) = store.get_artifact(artifact_id)? else {
        return Ok(());
    };
    let expected_size = i64::try_from(artifact.size_bytes.expect("checked above")).ok();
    let expected_sha256 = artifact.sha256.as_deref().expect("checked above");
    if existing.artifact_type != "file"
        || existing.run_id != run_id
        || existing.kind != artifact.kind
        || existing.size_bytes != expected_size
        || existing.sha256.as_deref() != Some(expected_sha256)
        || std::fs::metadata(&existing.path)
            .map(|metadata| {
                metadata.is_file() && i64::try_from(metadata.len()).ok() == expected_size
            })
            .unwrap_or(false)
            == false
        || homeboy_core::artifact_metadata::sha256_file(Path::new(&existing.path))
            .ok()
            .as_deref()
            != Some(expected_sha256)
    {
        return Ok(());
    }
    let mut metadata = existing.metadata_json;
    if !metadata.is_object() {
        metadata = json!({});
    }
    let agent_task = metadata["agent_task"].as_object_mut();
    if agent_task.is_none() {
        metadata["agent_task"] = json!({});
    }
    let agent_task = metadata["agent_task"]
        .as_object_mut()
        .expect("agent task metadata object");
    let existing_task_id = agent_task.get("task_id").and_then(Value::as_str);
    let existing_logical_id = agent_task
        .get("logical_artifact_id")
        .and_then(Value::as_str);
    if existing_task_id.is_some_and(|value| value != task_id)
        || existing_logical_id.is_some_and(|value| value != artifact.id)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!("existing artifact record conflicts with terminal artifact projection: {artifact_id}"),
            Some(artifact_id.to_string()),
            None,
        ));
    }
    agent_task.insert("task_id".to_string(), json!(task_id));
    agent_task.insert("logical_artifact_id".to_string(), json!(artifact.id));
    store.update_artifact_metadata(artifact_id, metadata)?;
    Ok(())
}

/// Observation artifacts are controller-owned only when their resolved file is
/// beneath the controller artifact root. A matching digest alone must not turn
/// an ephemeral producer path into durable lifecycle input.
fn controller_owned_artifact_path(
    store: &homeboy_core::observation::ObservationStore,
    path: &Path,
) -> bool {
    let Ok(root) = store.artifact_root().and_then(|root| {
        std::fs::canonicalize(root).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("resolve controller artifact root".to_string()),
            )
        })
    }) else {
        return false;
    };
    controller_owned_artifact_path_at(path, &root)
}

fn controller_owned_artifact_path_at(path: &Path, root: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(root) else {
        return false;
    };
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    path.starts_with(root)
}

/// Find finalized bytes already copied into the controller artifact root. Lab
/// aggregate paths describe runner provenance and are never read after recovery.
fn controller_finalized_artifact_path(
    store: &homeboy_core::observation::ObservationStore,
    artifact: &AgentTaskArtifact,
) -> Result<Option<PathBuf>> {
    let Some(expected_sha256) = artifact.sha256.as_deref() else {
        return Ok(None);
    };
    let Some(expected_size) = artifact.size_bytes else {
        return Ok(None);
    };
    let root = store.artifact_root()?.join("executor-finalized");
    if !root.is_dir() {
        return Ok(None);
    }
    let mut matches = Vec::new();
    collect_matching_finalized_artifacts(&root, expected_sha256, expected_size, &mut matches)?;
    matches.sort();
    Ok(matches.into_iter().next())
}

fn collect_matching_finalized_artifacts(
    directory: &Path,
    expected_sha256: &str,
    expected_size: u64,
    matches: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in std::fs::read_dir(directory).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "read controller finalized artifact directory {}",
                directory.display()
            )),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read controller finalized artifact entry".to_string()),
            )
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "inspect controller finalized artifact {}",
                    path.display()
                )),
            )
        })?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_matching_finalized_artifacts(&path, expected_sha256, expected_size, matches)?;
        } else if metadata.is_file()
            && metadata.len() == expected_size
            && homeboy_core::artifact_metadata::sha256_file(&path)? == expected_sha256
        {
            matches.push(path);
        }
    }
    Ok(())
}

/// Locate the controller-owned copy of a lifecycle artifact. Aggregate paths
/// describe producer provenance and can point at a runner after reconciliation;
/// promotion must consume the controller projection instead.
pub fn verified_controller_artifact_projection_path(
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
) -> Result<Option<PathBuf>> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    verified_controller_artifact_projection_path_in_store(&store, run_id, task_id, artifact)
}

pub fn verified_controller_artifact_projection_path_in_store(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
) -> Result<Option<PathBuf>> {
    let artifact_root = store.artifact_root()?;
    let Some(expected_sha256) = artifact.sha256.as_deref() else {
        return Ok(None);
    };
    let Some(expected_size) = artifact
        .size_bytes
        .and_then(|size| i64::try_from(size).ok())
    else {
        return Ok(None);
    };
    let candidates: Vec<_> = store
        .list_artifacts(run_id)?
        .into_iter()
        .filter(|candidate| {
            candidate.artifact_type == "file"
                && candidate.kind == artifact.kind
                && (matches!(
                    candidate
                        .metadata_json
                        .pointer("/agent_task/projection")
                        .and_then(serde_json::Value::as_str),
                    Some("controller_local" | "controller_finalized" | "runner_mirrored")
                ) || controller_owned_artifact_path_at(
                    Path::new(&candidate.path),
                    &artifact_root,
                ))
                && candidate
                    .metadata_json
                    .pointer("/agent_task/task_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(task_id)
                && candidate
                    .metadata_json
                    .pointer("/agent_task/logical_artifact_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(artifact.id.as_str())
        })
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }
    let mut candidate = select_controller_artifact_projection(candidates).map_err(|conflicting| {
        Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "conflicting controller-side artifact projections match run '{run_id}', task '{task_id}', and artifact '{}': {}",
                artifact.id,
                conflicting.join(", ")
            ),
            Some(artifact.id.clone()),
            None,
        )
    })?;
    let path = PathBuf::from(&candidate.path);
    let actual_size = std::fs::metadata(&path)
        .ok()
        .and_then(|metadata| i64::try_from(metadata.len()).ok());
    let actual_sha256 = homeboy_core::artifact_metadata::sha256_file(&path).ok();
    if candidate.sha256.as_deref() != Some(expected_sha256)
        || candidate.size_bytes != Some(expected_size)
        || actual_size != Some(expected_size)
        || actual_sha256.as_deref() != Some(expected_sha256)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "controller-side artifact projection for run '{run_id}', task '{task_id}', and artifact '{}' does not match the aggregate SHA-256 and size",
                artifact.id
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    if !controller_owned_artifact_path_at(&path, &artifact_root) {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "controller-side artifact projection for run '{run_id}', task '{task_id}', and artifact '{}' is not stored under controller ownership",
                artifact.id
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    if !matches!(
        candidate
            .metadata_json
            .pointer("/agent_task/projection")
            .and_then(serde_json::Value::as_str),
        Some("controller_local" | "controller_finalized" | "runner_mirrored")
    ) {
        candidate.metadata_json["agent_task"]["projection"] = json!("controller_local");
        store.update_artifact_metadata(&candidate.id, candidate.metadata_json)?;
    }
    Ok(Some(path))
}

/// Resolve a controller-retained artifact by its durable logical identity. This
/// is intentionally independent of the runner-reported path: Lab workspaces
/// are disposable after their aggregate has been mirrored.
pub(crate) fn verified_controller_artifact_projection(
    run_id: &str,
    task_id: &str,
    logical_artifact_id: &str,
    kind: &str,
    expected_sha256: &str,
    expected_record_id: Option<&str>,
) -> Result<Option<(String, Vec<u8>)>> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    verified_controller_artifact_projection_in_store(
        &store,
        run_id,
        task_id,
        logical_artifact_id,
        kind,
        expected_sha256,
        expected_record_id,
    )
}

pub fn verified_controller_artifact_projection_in_store(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    logical_artifact_id: &str,
    kind: &str,
    expected_sha256: &str,
    expected_record_id: Option<&str>,
) -> Result<Option<(String, Vec<u8>)>> {
    let artifact_root = store.artifact_root()?;
    let candidates: Vec<_> = store
        .list_artifacts(run_id)?
        .into_iter()
        .filter(|candidate| {
            candidate.artifact_type == "file"
                && candidate.kind == kind
                && candidate
                    .metadata_json
                    .pointer("/agent_task/task_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(task_id)
                && candidate
                    .metadata_json
                    .pointer("/agent_task/logical_artifact_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(logical_artifact_id)
        })
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }
    // An explicitly pinned record stays authoritative; selection only resolves
    // equivalent projections the caller has not already chosen between.
    let selected = match expected_record_id
        .and_then(|id| candidates.iter().position(|candidate| candidate.id == id))
    {
        Some(index) => candidates.into_iter().nth(index).expect("located candidate"),
        None => select_controller_artifact_projection(candidates).map_err(|conflicting| {
            Error::validation_invalid_argument(
                "artifact_id",
                format!(
                    "conflicting controller-side artifact projections match run '{run_id}', task '{task_id}', and artifact '{logical_artifact_id}': {}",
                    conflicting.join(", ")
                ),
                Some(logical_artifact_id.to_string()),
                None,
            )
        })?,
    };
    let candidate = &selected;
    if expected_record_id.is_some_and(|id| id != candidate.id) {
        return Err(Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            format!(
                "controller artifact identity mismatch for run '{run_id}', task '{task_id}', and artifact '{logical_artifact_id}': expected record '{}', found '{}'",
                expected_record_id.unwrap_or_default(), candidate.id
            ),
            Some(logical_artifact_id.to_string()),
            None,
        ));
    }
    let path = PathBuf::from(&candidate.path);
    if !controller_owned_artifact_path_at(&path, &artifact_root) {
        return Err(Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            "controller artifact mirror is outside the owning artifact root",
            Some(path.display().to_string()),
            None,
        ));
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            format!(
                "controller artifact mirror is unavailable for run '{run_id}', task '{task_id}', and artifact '{logical_artifact_id}': {}",
                error
            ),
            Some(logical_artifact_id.to_string()),
            None,
        )
    })?;
    let actual_sha256 = content_hash::sha256_hex(&bytes);
    if candidate.sha256.as_deref() != Some(expected_sha256)
        || candidate.size_bytes != Some(bytes.len() as i64)
        || actual_sha256 != expected_sha256
    {
        return Err(Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            format!(
                "controller artifact mirror hash mismatch for run '{run_id}', task '{task_id}', and artifact '{logical_artifact_id}': expected '{expected_sha256}', record '{:?}', bytes '{actual_sha256}'",
                candidate.sha256
            ),
            Some(logical_artifact_id.to_string()),
            None,
        ));
    }
    Ok(Some((candidate.id.clone(), bytes)))
}

/// Runner artifact retrieval is keyed only by artifact id. Reject duplicates
/// rather than inventing a controller-only alias that cannot be retried safely.
fn validate_unique_terminal_artifact_ids(aggregate: &AgentTaskAggregate) -> Result<()> {
    let mut identities = std::collections::BTreeMap::new();
    for outcome in &aggregate.outcomes {
        for artifact in &outcome.artifacts {
            validate_projection_token("artifact.id", &artifact.id)?;
            let Some(previous_task_id) = identities.insert(&artifact.id, &outcome.task_id) else {
                continue;
            };
            return Err(Error::validation_invalid_argument(
                "artifact.id",
                format!(
                    "terminal aggregate reuses artifact id '{}' for tasks '{}' and '{}'; runner artifact ids must be unique",
                    artifact.id, previous_task_id, outcome.task_id
                ),
                Some(artifact.id.clone()),
                None,
            ));
        }
    }
    Ok(())
}

fn validate_projection_token(field: &str, value: &str) -> Result<()> {
    crate::agent_task_provider::artifact_finalization::validate_token(field, value)
}

pub(crate) fn apply_aggregate_to_record(
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    aggregate_path: String,
) {
    record.updated_at = Some(now_timestamp());
    set_run_state(record, run_state_for_aggregate(aggregate));
    record.aggregate_path = Some(aggregate_path);
    record.totals = Some(aggregate.totals.clone());
    record.tasks = tasks_for_aggregate(plan, aggregate);
    record.artifact_refs = artifact_refs_for_outcomes(&aggregate.outcomes);
    record.provider_handles = provider_handles_for_outcomes(&aggregate.outcomes);
    persist_provider_handle_models(&mut record.provider_handles, plan);
    record.latest_executor_evidence = latest_executor_evidence(&record.run_id, plan, aggregate);
    update_lifecycle_from_record(record, plan);
    let provider_run_ids: Vec<String> = record
        .provider_handles
        .iter()
        .map(|handle| handle.provider_run_id.clone())
        .collect();
    let latest_executor_evidence_value = record
        .latest_executor_evidence
        .as_ref()
        .map(|evidence| serde_json::to_value(evidence).unwrap_or(Value::Null));
    let metadata = record.ensure_metadata_object();
    metadata.insert("provider_run_ids".to_string(), json!(provider_run_ids));
    if let Some(evidence) = latest_executor_evidence_value {
        metadata.insert("latest_executor_evidence".to_string(), evidence);
    }
}

fn persist_provider_handle_models(
    handles: &mut [AgentTaskRunProviderHandle],
    plan: &AgentTaskPlan,
) {
    for handle in handles {
        if handle
            .metadata
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| !model.trim().is_empty())
        {
            continue;
        }
        let Some(model) = plan
            .tasks
            .iter()
            .find(|task| task.task_id == handle.task_id)
            .and_then(|task| task.executor.model())
            .filter(|model| !model.trim().is_empty())
        else {
            continue;
        };
        if !handle.metadata.is_object() {
            handle.metadata = json!({});
        }
        handle
            .metadata
            .as_object_mut()
            .expect("provider handle metadata object")
            .insert("model".to_string(), json!(model));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: &str, kind: &str, runner_id: Option<&str>) -> AgentTaskArtifact {
        AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: id.to_string(),
            kind: kind.to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/runner/patch.diff".to_string()),
            url: None,
            mime: None,
            size_bytes: Some(1),
            sha256: Some("a".repeat(64)),
            metadata: runner_id.map_or_else(
                || json!({}),
                |runner_id| json!({ "source_provenance": { "runner_id": runner_id } }),
            ),
        }
    }

    #[test]
    fn legacy_runner_provenance_uses_only_actionable_patch_artifacts() {
        let mut aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: "plan".to_string(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals::default(),
            outcomes: Vec::new(),
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus::default(),
        };
        aggregate.outcomes.push(AgentTaskOutcome {
            task_id: "task".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            artifacts: vec![
                artifact("patch", "patch", Some("runner-a")),
                artifact("transcript", "transcript", None),
                artifact("result", "result", None),
                artifact("runtime-log", "runtime-log", None),
            ],
            ..Default::default()
        });

        assert_eq!(
            runner_id_from_artifact_provenance(&aggregate).expect("consistent provenance"),
            "runner-a"
        );
        aggregate.outcomes[0]
            .artifacts
            .push(artifact("second-patch", "patch", Some("runner-b")));
        assert!(runner_id_from_artifact_provenance(&aggregate).is_err());
    }

    #[test]
    fn artifact_runner_provenance_must_match_the_lifecycle_binding() {
        let artifact = artifact("patch", "patch", Some("runner-a"));

        validate_artifact_runner_binding(&artifact, "runner-a").expect("matching runner binding");
        let error = validate_artifact_runner_binding(&artifact, "runner-b")
            .expect_err("conflicting runner identity must fail closed");

        assert!(error
            .message
            .contains("conflicts with lifecycle runner binding"));
    }

    #[test]
    fn reusable_artifact_rejects_conflicting_persisted_logical_identity() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let store = homeboy_core::observation::ObservationStore::open_initialized()
                .expect("observation store");
            let run = store
                .start_run(
                    homeboy_core::observation::NewRunRecord::builder("agent-task")
                        .cwd_path(home.path())
                        .build(),
                )
                .expect("run");
            let path = home.path().join("patch.diff");
            let bytes = b"patch";
            std::fs::write(&path, bytes).expect("patch bytes");
            let mut artifact = artifact("patch", "patch", None);
            artifact.size_bytes = Some(bytes.len() as u64);
            artifact.sha256 = Some(format!("{:x}", sha2::Sha256::digest(bytes)));
            store
                .record_artifact_with_id(
                    &run.id,
                    "patch",
                    &path,
                    "stable-patch",
                    json!({
                        "agent_task": {
                            "task_id": "other-task",
                            "logical_artifact_id": "patch",
                        }
                    }),
                )
                .expect("imported artifact");

            let error = reusable_terminal_artifact(
                &store,
                &run.id,
                "task",
                &artifact,
                "stable-patch",
                &json!({
                    "agent_task": {
                        "task_id": "task",
                        "logical_artifact_id": "patch",
                    }
                }),
            )
            .expect_err("conflicting logical identity must fail closed");

            assert!(error
                .message
                .contains("conflicts with terminal artifact projection"));
        });
    }

    #[test]
    fn controller_local_projection_isolates_identical_ids_across_artifact_roots() {
        let left_context = homeboy_core::test_support::HermeticTestContext::new();
        let right_context = homeboy_core::test_support::HermeticTestContext::new();
        let left_store = AgentTaskLifecycleStore::new(left_context.path_roots())
            .open_observation_initialized()
            .expect("left observation store");
        let right_store = AgentTaskLifecycleStore::new(right_context.path_roots())
            .open_observation_initialized()
            .expect("right observation store");
        let left_source = left_context.root().join("source.patch");
        let right_source = right_context.root().join("source.patch");
        std::fs::write(&left_source, b"left").expect("left source");
        std::fs::write(&right_source, b"right").expect("right source");

        let left = controller_projected_local_artifact_path(
            &left_store,
            "same-run",
            "same-task",
            "same-artifact",
            left_source.to_str().unwrap(),
            4,
            &format!("{:x}", sha2::Sha256::digest(b"left")),
        )
        .expect("left projection");
        let right = controller_projected_local_artifact_path(
            &right_store,
            "same-run",
            "same-task",
            "same-artifact",
            right_source.to_str().unwrap(),
            5,
            &format!("{:x}", sha2::Sha256::digest(b"right")),
        )
        .expect("right projection");

        assert!(left.starts_with(left_store.artifact_root().unwrap()));
        assert!(right.starts_with(right_store.artifact_root().unwrap()));
        assert_ne!(left, right);
        assert_eq!(std::fs::read(left).unwrap(), b"left");
        assert_eq!(std::fs::read(right).unwrap(), b"right");
    }

    /// Register one logical artifact twice the way terminal projection does for
    /// a Lab run: a legacy import stamped with the lifecycle identity, plus the
    /// finalized controller projection that carries projection authority.
    fn record_duplicate_projections(
        store: &homeboy_core::observation::ObservationStore,
        run_id: &str,
        legacy_bytes: &[u8],
        finalized_bytes: &[u8],
    ) {
        let root = store.artifact_root().expect("artifact root");
        std::fs::create_dir_all(&root).expect("artifact root directory");
        let legacy_path = root.join("legacy-transcript.txt");
        let finalized_path = root.join("finalized-transcript.txt");
        std::fs::write(&legacy_path, legacy_bytes).expect("legacy bytes");
        std::fs::write(&finalized_path, finalized_bytes).expect("finalized bytes");
        store
            .record_verified_artifact_with_id(
                run_id,
                "transcript",
                &legacy_path,
                "agent-task-legacy",
                Some(legacy_bytes.len() as i64),
                Some(&format!("{:x}", sha2::Sha256::digest(legacy_bytes))),
                json!({
                    "agent_task": { "task_id": "task", "logical_artifact_id": "transcript" }
                }),
            )
            .expect("legacy projection");
        store
            .record_verified_artifact_with_id(
                run_id,
                "transcript",
                &finalized_path,
                "agent-task-finalized",
                Some(finalized_bytes.len() as i64),
                Some(&format!("{:x}", sha2::Sha256::digest(finalized_bytes))),
                json!({
                    "agent_task": {
                        "task_id": "task",
                        "logical_artifact_id": "transcript",
                        "projection": "controller_finalized",
                    }
                }),
            )
            .expect("finalized projection");
    }

    fn transcript_artifact(bytes: &[u8]) -> AgentTaskArtifact {
        let mut artifact = artifact("transcript", "transcript", None);
        artifact.size_bytes = Some(bytes.len() as u64);
        artifact.sha256 = Some(format!("{:x}", sha2::Sha256::digest(bytes)));
        artifact
    }

    #[test]
    fn equivalent_duplicate_projections_resolve_to_the_finalized_record() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let store = homeboy_core::observation::ObservationStore::open_initialized()
                .expect("observation store");
            let run = store
                .start_run(
                    homeboy_core::observation::NewRunRecord::builder("agent-task")
                        .cwd_path(home.path())
                        .build(),
                )
                .expect("run");
            let bytes = b"transcript bytes";
            record_duplicate_projections(&store, &run.id, bytes, bytes);

            // A Lab run legitimately retains its legacy import alongside the
            // finalized projection, so this must resolve rather than abort the
            // owning Cook (#14164).
            let resolved = verified_controller_artifact_projection_path_in_store(
                &store,
                &run.id,
                "task",
                &transcript_artifact(bytes),
            )
            .expect("equivalent duplicates resolve")
            .expect("a controller projection is available");

            // Projection authority, not discovery order, selects the record.
            assert!(
                resolved
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("agent-task-finalized")),
                "expected the finalized projection, got {}",
                resolved.display()
            );
            assert_eq!(std::fs::read(&resolved).expect("resolved bytes"), bytes);

            let (_, bytes_read) = verified_controller_artifact_projection_in_store(
                &store,
                &run.id,
                "task",
                "transcript",
                "transcript",
                &format!("{:x}", sha2::Sha256::digest(bytes)),
                None,
            )
            .expect("equivalent duplicates resolve for byte reads")
            .expect("a controller projection is available");

            assert_eq!(bytes_read, bytes);
        });
    }

    #[test]
    fn conflicting_duplicate_projections_still_fail_closed() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let store = homeboy_core::observation::ObservationStore::open_initialized()
                .expect("observation store");
            let run = store
                .start_run(
                    homeboy_core::observation::NewRunRecord::builder("agent-task")
                        .cwd_path(home.path())
                        .build(),
                )
                .expect("run");
            let bytes = b"transcript bytes";
            record_duplicate_projections(&store, &run.id, b"different transcript", bytes);

            let error = verified_controller_artifact_projection_path_in_store(
                &store,
                &run.id,
                "task",
                &transcript_artifact(bytes),
            )
            .expect_err("projections that disagree on content must fail closed");

            assert!(
                error.message.contains("conflicting"),
                "got {}",
                error.message
            );
            assert!(
                error.message.contains("agent-task-finalized")
                    && error.message.contains("agent-task-legacy"),
                "the conflicting records must be named: {}",
                error.message
            );
        });
    }
}

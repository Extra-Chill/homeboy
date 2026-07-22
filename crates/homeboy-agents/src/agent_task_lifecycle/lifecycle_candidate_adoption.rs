//! Candidate-adoption lifecycle for agent-task Cook runs.
//!
//! Adopting a candidate is the entry point of a Cook attempt: it opens the
//! adoption gate, heartbeats and checkpoints gate progress, honors cancellation,
//! finishes the adoption, and reconciles adopted state back onto the run record.
//! It also reconstructs the authenticated pre-provider recovery outcome that lets
//! an externally prepared immutable candidate be admitted after a transport
//! failure. Extracted from `lifecycle_ops` so the adoption state machine stays
//! reviewable in isolation.

use super::*;

pub(crate) fn project_cook_alias_adoption(
    record: &mut AgentTaskRunRecord,
    index: &AgentTaskCookIndex,
) -> Result<()> {
    let mut selected: Option<(
        bool,
        String,
        usize,
        String,
        AgentTaskCandidateAdoptionAttempt,
    )> = None;

    for (index_order, indexed_attempt) in index.attempts.iter().enumerate() {
        let Ok(mut attempt_record) = store::read_record(&indexed_attempt.run_id) else {
            continue;
        };
        if reconcile_candidate_adoption(&mut attempt_record) {
            store::write_record(&attempt_record)?;
        }
        let Some(adoption) = attempt_record.candidate_adoption else {
            continue;
        };
        let candidate = (
            adoption.is_active(),
            adoption.updated_at.clone(),
            index_order,
            indexed_attempt.run_id.clone(),
            adoption,
        );
        let replace = selected.as_ref().is_none_or(|current| {
            candidate.0 > current.0
                // Among equally active or inactive attempts, the newest
                // adoption timestamp wins; index order breaks timestamp ties.
                || (candidate.0 == current.0
                    && (candidate.1.as_str(), candidate.2)
                        > (current.1.as_str(), current.2))
        });
        if replace {
            selected = Some(candidate);
        }
    }

    if let Some((_, _, _, adoption_run_id, adoption)) = selected {
        record.adoption_run_id = Some(adoption_run_id);
        record.candidate_adoption = Some(adoption);
    } else {
        record.adoption_run_id = None;
        record.candidate_adoption = None;
    }
    Ok(())
}

/// Claim or resume the one controller-owned candidate adoption for a run. The
/// record is written before promotion can invoke a workspace provider or gate.
pub fn start_candidate_adoption(
    run_id: &str,
    candidate_sha: &str,
    ai_model: &str,
    active_gate: &str,
) -> Result<AgentTaskRunRecord> {
    start_candidate_adoption_with_rerun_policy(run_id, candidate_sha, ai_model, active_gate, false)
}

pub fn start_candidate_adoption_with_rerun_policy(
    run_id: &str,
    candidate_sha: &str,
    ai_model: &str,
    active_gate: &str,
    rerun_completed_gates: bool,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let candidate_sha = candidate_sha.to_string();
    let ai_model = ai_model.to_string();
    let active_gate = active_gate.to_string();
    let mut conflict = false;
    let record = store::mutate_record(&run_id, |record| {
        let now = now_timestamp();
        if let Some(existing) = record.candidate_adoption.as_mut() {
            if existing.gate_process_group.is_some_and(|pgid| {
                homeboy_core::process::isolated_process_group_is_running(pgid).unwrap_or(false)
            }) {
                conflict = true;
                return false;
            }
            if existing.state == "verification_running" {
                if homeboy_core::process::pid_is_running(existing.owner_pid) {
                    conflict = true;
                    return false;
                }
                existing.state = "interrupted".to_string();
                existing.phase = "owner_stale".to_string();
                existing.updated_at = now.clone();
                existing.terminal_error = Some("adoption owner process is not running".to_string());
            }
            if existing.state == "interrupted"
                && existing.candidate_sha == candidate_sha
                && existing.ai_model == ai_model
            {
                existing.state = "verification_running".to_string();
                existing.phase = "verification".to_string();
                existing.active_gate = active_gate.clone();
                existing.owner_pid = std::process::id();
                existing.heartbeat_at = now.clone();
                existing.updated_at = now.clone();
                existing.resume_count += 1;
                existing.terminal_error = None;
                record.updated_at = Some(now);
                return true;
            }
            if existing.state == "completed"
                && existing.candidate_sha == candidate_sha
                && existing.ai_model == ai_model
                && !rerun_completed_gates
            {
                conflict = true;
                return false;
            }
            if existing.state == "verification_running" || existing.state == "interrupted" {
                conflict = true;
                return false;
            }
        }
        record.candidate_adoption = Some(AgentTaskCandidateAdoptionAttempt {
            candidate_sha: candidate_sha.clone(),
            ai_model: ai_model.clone(),
            state: "verification_running".to_string(),
            phase: "verification".to_string(),
            active_gate: active_gate.clone(),
            started_at: now.clone(),
            updated_at: now.clone(),
            owner_pid: std::process::id(),
            heartbeat_at: now.clone(),
            gate_process_group: None,
            gate_started_at: None,
            gate_timeout_seconds: None,
            gate_output_tail: String::new(),
            resume_count: 0,
            terminal_error: None,
            completed_at: None,
            result: None,
        });
        record.updated_at = Some(now);
        true
    })?;
    let record = match record {
        Some(record) => record,
        None => store::read_record(&run_id)?,
    };
    if conflict {
        return Err(Error::validation_invalid_argument(
            "candidate_ref",
            "candidate adoption conflicts with an existing durable attempt; use its immutable candidate SHA and model after recovery",
            Some(candidate_sha),
            None,
        ));
    }
    let attempt = record.candidate_adoption.as_ref().ok_or_else(|| {
        Error::internal_unexpected("candidate adoption claim did not persist a durable attempt")
    })?;
    if attempt.state != "verification_running"
        || attempt.owner_pid != std::process::id()
        || attempt.candidate_sha != candidate_sha
        || attempt.ai_model != ai_model
    {
        return Err(Error::validation_invalid_argument(
            "candidate_ref",
            "candidate adoption conflicts with an existing durable attempt; use its immutable candidate SHA and model after recovery",
            Some(candidate_sha),
            None,
        ));
    }
    Ok(record)
}

pub fn start_candidate_adoption_gate(
    run_id: &str,
    command: &str,
    process_group: u32,
    timeout_seconds: u64,
) -> Result<()> {
    let run_id = sanitize_run_id(run_id);
    store::mutate_record(&run_id, |record| {
        let Some(attempt) = record.candidate_adoption.as_mut() else {
            return false;
        };
        if attempt.state != "verification_running" {
            return false;
        }
        let now = now_timestamp();
        attempt.phase = "gate_running".to_string();
        attempt.active_gate = command.to_string();
        attempt.gate_process_group = Some(process_group);
        attempt.gate_started_at = Some(now.clone());
        attempt.gate_timeout_seconds = Some(timeout_seconds);
        attempt.gate_output_tail.clear();
        attempt.heartbeat_at = now.clone();
        attempt.updated_at = now.clone();
        record.updated_at = Some(now);
        true
    })?;
    Ok(())
}

pub fn heartbeat_candidate_adoption_gate(run_id: &str, output_tail: &str) -> Result<()> {
    let run_id = sanitize_run_id(run_id);
    store::mutate_record(&run_id, |record| {
        let Some(attempt) = record.candidate_adoption.as_mut() else {
            return false;
        };
        if attempt.state != "verification_running" {
            return false;
        }
        let now = now_timestamp();
        attempt.heartbeat_at = now.clone();
        attempt.updated_at = now.clone();
        attempt.gate_output_tail = output_tail.to_string();
        record.updated_at = Some(now);
        true
    })?;
    Ok(())
}

pub fn candidate_adoption_cancel_requested(run_id: &str) -> Result<bool> {
    Ok(store::read_record(&sanitize_run_id(run_id))?
        .candidate_adoption
        .as_ref()
        .is_some_and(|attempt| attempt.state == "cancel_requested" || attempt.state == "cancelled"))
}

pub fn checkpoint_candidate_adoption(run_id: &str, phase: &str, active_gate: &str) -> Result<()> {
    let run_id = sanitize_run_id(run_id);
    store::mutate_record(&run_id, |record| {
        let Some(attempt) = record.candidate_adoption.as_mut() else {
            return false;
        };
        if attempt.state != "verification_running" {
            return false;
        }
        let now = now_timestamp();
        attempt.phase = phase.to_string();
        attempt.active_gate = active_gate.to_string();
        attempt.updated_at = now.clone();
        attempt.heartbeat_at = now.clone();
        record.updated_at = Some(now);
        true
    })?;
    Ok(())
}

pub fn finish_candidate_adoption(
    run_id: &str,
    error: Option<String>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = store::mutate_record(&run_id, |record| {
        let Some(attempt) = record.candidate_adoption.as_mut() else {
            return false;
        };
        if attempt.state == "cancelled" || attempt.state == "cancel_requested" {
            return false;
        }
        let now = now_timestamp();
        attempt.updated_at = now.clone();
        attempt.heartbeat_at = now.clone();
        attempt.completed_at = Some(now.clone());
        attempt.terminal_error = error.clone();
        attempt.state = if error.is_some() {
            "failed"
        } else {
            "completed"
        }
        .to_string();
        attempt.phase = "terminal".to_string();
        record.updated_at = Some(now);
        true
    })?;
    Ok(record.unwrap_or(store::read_record(&run_id)?))
}

pub fn record_candidate_adoption_result(run_id: &str, result: Value) -> Result<()> {
    let run_id = sanitize_run_id(run_id);
    store::mutate_record(&run_id, |record| {
        let Some(attempt) = record.candidate_adoption.as_mut() else {
            return false;
        };
        attempt.result = Some(result);
        attempt.updated_at = now_timestamp();
        record.updated_at = Some(attempt.updated_at.clone());
        true
    })?;
    Ok(())
}

pub(crate) fn reconcile_candidate_adoption(record: &mut AgentTaskRunRecord) -> bool {
    let Some(attempt) = record.candidate_adoption.as_mut() else {
        return false;
    };
    if attempt.state != "verification_running"
        || homeboy_core::process::pid_is_running(attempt.owner_pid)
    {
        return false;
    }
    let now = now_timestamp();
    attempt.state = "interrupted".to_string();
    attempt.phase = if attempt.gate_process_group.is_some_and(|pgid| {
        homeboy_core::process::isolated_process_group_is_running(pgid).unwrap_or(false)
    }) {
        "gate_orphaned"
    } else {
        "owner_stale"
    }
    .to_string();
    attempt.updated_at = now.clone();
    attempt.terminal_error = Some(if attempt.phase == "gate_orphaned" {
        "adoption controller stopped while its gate process group remains live; cancel the adoption before resuming"
    } else {
        "adoption owner process is not running; rerun adopt with the recorded candidate SHA to resume"
    }.to_string());
    record.updated_at = Some(now);
    true
}

pub fn candidate_adoption_recovery_outcome(
    record: &AgentTaskRunRecord,
    task: &AgentTaskRequest,
) -> Option<AgentTaskOutcome> {
    let expired_handoff = record.lab_handoff.as_ref().is_some_and(|handoff| {
        record.state == AgentTaskRunState::Cancelled
            && record.aggregate_path.is_none()
            && record.totals.is_none()
            && record.artifact_refs.is_empty()
            && record.provider_handles.is_empty()
            && record.latest_executor_evidence.is_none()
            && record.lab_handoff_validation_error().is_none()
            && handoff.state == AgentTaskLabHandoffState::Expired
            && handoff.runner_job_id.is_none()
            && record.metadata["phase"] == "handoff_rejected"
            && record.metadata["provider_executions_consumed"] == 0
            && record.metadata["handoff_acceptance"]["state"] == "expired"
            && record.metadata["handoff_acceptance"]["reason"] == EXPIRED_LAB_HANDOFF_REASON
    });
    let failed_preacceptance = record.state == AgentTaskRunState::Failed
        && record.metadata["phase"] == "lab_handoff_preacceptance"
        && record.metadata["provider_executions_consumed"] == 0
        && record.provider_handles.is_empty()
        && no_runner_job_recorded(record)
        && record.lifecycle.external_runtime_ids.is_empty()
        && record.lifecycle.provider_runtime.iter().all(|runtime| {
            runtime.external_runtime_ids.is_empty()
                && runtime.metadata["evidence_source"] == "canonical_executor_outcome"
        })
        && record.metadata["pre_execution_failure"]["phase"] == "lab_handoff_preacceptance"
        && is_pre_provider_transport_recovery(
            &record.metadata["pre_execution_failure"]["candidate_adoption_recovery"],
        );
    (expired_handoff || failed_preacceptance).then(|| {
        build_pre_execution_failure_outcome(
            &record.run_id,
            task,
            "lab_handoff_preacceptance",
            &Error::internal_unexpected(EXPIRED_LAB_HANDOFF_REASON.to_string()),
        )
    })
}

fn no_runner_job_recorded(record: &AgentTaskRunRecord) -> bool {
    record.runner_job_id().is_none()
        && record
            .lab_handoff
            .as_ref()
            .is_none_or(|handoff| handoff.runner_job_id.is_none())
        && record.metadata["runner_job_id"].is_null()
        && record.metadata["job_id"].is_null()
}

/// Schema tag stamped on the pre-provider candidate-adoption recovery marker
/// produced when a Lab handoff fails before any provider executes. It is the
/// single source of truth for that marker's identity across the adoption
/// pipeline (recording, promotion eligibility, publication eligibility).
pub(crate) const CANDIDATE_ADOPTION_RECOVERY_SCHEMA: &str =
    "homeboy/agent-task-candidate-adoption-recovery/v1";

/// True when `recovery` is an authenticated pre-provider transport-failure
/// recovery marker: the exact shape that authorizes adopting an externally
/// prepared candidate whose original attempt never ran a provider.
///
/// This is the *one* definition of that check. The adoption recovery pipeline
/// validates the same marker at several independent boundaries (candidate
/// source resolution, promotion eligibility, publication eligibility); routing
/// them all through here keeps those boundaries from drifting apart — the drift
/// that made this path regress repeatedly (issue #8983).
pub(crate) fn is_pre_provider_transport_recovery(recovery: &Value) -> bool {
    recovery["schema"] == CANDIDATE_ADOPTION_RECOVERY_SCHEMA
        && recovery["reason"] == "pre_provider_transport_failure"
        && recovery["provider_executions_consumed"] == 0
}

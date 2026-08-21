//! Lab offload proxy planning, phase recording, staging-controller job binding,
//! and detached Lab run recording. Extracted from `lifecycle_ops` to keep that
//! module within the god-file threshold (#9927).

use serde_json::{json, Value};

use super::*;

#[derive(Debug, Clone)]
pub struct DetachedLabRunRecord<'a> {
    pub run_id: &'a str,
    pub runner_id: &'a str,
    pub runner_job_id: &'a str,
    pub remote_workspace: &'a str,
    pub remote_command: &'a [String],
}

/// How a Lab offload entry point materializes a durable run that is not already
/// present in the store it was handed.
///
/// Every entry point in this module reads its record first and submits only on
/// the fall-through, and that submission is the one reach a rooted Lab offload
/// function still makes past its lifecycle store: `submit_plan_in_store` admits
/// through `homeboy_core::controller_runtime`, whose FIFO admission queue and
/// content-addressed pin store hang off the *process* data root and are
/// machine-global on purpose (#7505, #12608). That is not the split this
/// campaign forbids — an admission is not lifecycle state, and rooting only
/// half of it is explicitly the wrong fix — but it is a real limit, so the seam
/// is named rather than hidden. A caller that has already decided admission (a
/// hermetic test, or a controller that admitted once for a whole dispatch)
/// supplies its own submission and reaches nothing process-global at all; the
/// shape mirrors `AgentTaskLifecycleStore::submit_plan_with_runtime_admission`.
pub(crate) type LabOffloadSubmission<'a> =
    &'a dyn Fn(&AgentTaskLifecycleStore, &AgentTaskPlan, &str) -> Result<AgentTaskRunRecord>;

/// The controller's own submission: the machine-global controller-runtime
/// admission a real Lab dispatch must take before it owns a durable run.
fn admitted_lab_offload_submission(
    lifecycle_store: &AgentTaskLifecycleStore,
    plan: &AgentTaskPlan,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    submit_plan_in_store(lifecycle_store, plan, Some(run_id))
}

/// Atomically persist a daemon-accepted Lab job before a caller can inspect its
/// snapshot. The typed identity keeps every acceptance path on the canonical
/// run/runner/job comparison used by reconciliation and terminal projection.
pub fn bind_accepted_lab_runner_job(
    identity: &homeboy_core::lab_contract::RunnerJobIdentity,
    remote_workspace: &str,
    remote_command: &[String],
) -> Result<AgentTaskRunRecord> {
    bind_accepted_lab_runner_job_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        identity,
        remote_workspace,
        remote_command,
    )
}

/// The store-rooted counterpart of [`bind_accepted_lab_runner_job`].
///
/// Acceptance is a durable transfer of one run to one runner daemon, so the
/// validation, the record it is validated against, and the accepted handoff
/// written back all have to name the same installation. Rooting only the
/// wrapper would leave the daemon's authoritative binding landing wherever the
/// process environment happened to point (#7505).
pub(crate) fn bind_accepted_lab_runner_job_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    identity: &homeboy_core::lab_contract::RunnerJobIdentity,
    remote_workspace: &str,
    remote_command: &[String],
) -> Result<AgentTaskRunRecord> {
    if !identity.is_complete() {
        return Err(Error::validation_invalid_argument(
            "runner_job_identity",
            "accepted Lab runner job identity requires run id, runner id, and runner job id",
            Some(identity.describe()),
            None,
        ));
    }
    record_detached_lab_run_in_store(
        lifecycle_store,
        DetachedLabRunRecord {
            run_id: &identity.run_id,
            runner_id: &identity.runner_id,
            runner_job_id: &identity.runner_job_id,
            remote_workspace,
            remote_command,
        },
    )
}

#[derive(Debug, Clone)]
pub struct LabOffloadProxyPlan<'a> {
    pub run_id: &'a str,
    pub runner_id: &'a str,
    pub remote_workspace: &'a str,
    pub remote_command: &'a [String],
    /// The user task plan, materialized on the controller before the temporary
    /// runner handoff is recorded.
    pub durable_plan: Option<&'a AgentTaskPlan>,
}

/// Persist the controller-owned parent before handing an agent-task workload to
/// a Lab. The runner owns child execution; this record owns the stable local
/// identity and is reconciled from that child once it is accepted.
pub fn record_lab_offload_planned(input: LabOffloadProxyPlan<'_>) -> Result<AgentTaskRunRecord> {
    record_lab_offload_planned_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        input,
    )
}

/// The store-rooted counterpart of [`record_lab_offload_planned`].
pub(crate) fn record_lab_offload_planned_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    input: LabOffloadProxyPlan<'_>,
) -> Result<AgentTaskRunRecord> {
    record_lab_offload_planned_with_submission_in_store(
        lifecycle_store,
        input,
        &admitted_lab_offload_submission,
    )
}

/// Persist the controller-owned parent using a caller-supplied submission for
/// the run that does not exist yet. See [`LabOffloadSubmission`].
pub(crate) fn record_lab_offload_planned_with_submission_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    input: LabOffloadProxyPlan<'_>,
    submit: LabOffloadSubmission<'_>,
) -> Result<AgentTaskRunRecord> {
    record_lab_offload_proxy_in_store(
        lifecycle_store,
        input.run_id,
        input.runner_id,
        input.remote_workspace,
        input.remote_command,
        input.durable_plan,
        submit,
    )
}

/// The controller-owned setup progress recorded before a runner job exists.
///
/// The ambient entry point keeps its positional signature; the rooted siblings
/// take this instead so they stay inside the argument budget and read the same
/// way as [`DetachedLabRunRecord`] and [`LabOffloadProxyPlan`].
#[derive(Debug, Clone)]
pub struct LabOffloadPhaseRecord<'a> {
    pub requested_run_id: &'a str,
    pub runner_id: &'a str,
    pub phase: &'a str,
    pub remote_workspace: Option<&'a str>,
    pub source_checkout: Option<&'a Value>,
    pub provider_rotation: Option<&'a Value>,
    pub durable_plan: Option<&'a AgentTaskPlan>,
}

/// Persist controller-owned setup progress before a runner job exists.
pub fn record_lab_offload_phase(
    requested_run_id: &str,
    runner_id: &str,
    phase: &str,
    remote_workspace: Option<&str>,
    source_checkout: Option<&Value>,
    provider_rotation: Option<&Value>,
    durable_plan: Option<&AgentTaskPlan>,
) -> Result<AgentTaskRunRecord> {
    record_lab_offload_phase_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        LabOffloadPhaseRecord {
            requested_run_id,
            runner_id,
            phase,
            remote_workspace,
            source_checkout,
            provider_rotation,
            durable_plan,
        },
    )
}

/// The store-rooted counterpart of [`record_lab_offload_phase`].
///
/// The proxy read-or-submit, the phase metadata decided from what it returned,
/// and the write-back are one operation. Resolving the proxy in one home and
/// committing the phase into another would leave a run advertising a setup
/// phase that its own record never entered (#7505).
pub(crate) fn record_lab_offload_phase_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    input: LabOffloadPhaseRecord<'_>,
) -> Result<AgentTaskRunRecord> {
    record_lab_offload_phase_with_submission_in_store(
        lifecycle_store,
        input,
        &admitted_lab_offload_submission,
    )
}

/// Persist controller-owned setup progress using a caller-supplied submission
/// for the run that does not exist yet. See [`LabOffloadSubmission`].
pub(crate) fn record_lab_offload_phase_with_submission_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    input: LabOffloadPhaseRecord<'_>,
    submit: LabOffloadSubmission<'_>,
) -> Result<AgentTaskRunRecord> {
    let placeholder_workspace = input.remote_workspace.unwrap_or("pending");
    let mut record = record_lab_offload_proxy_in_store(
        lifecycle_store,
        input.requested_run_id,
        input.runner_id,
        placeholder_workspace,
        &[],
        input.durable_plan,
        submit,
    )?;
    if record.state.is_terminal() {
        return Ok(record);
    }
    record.updated_at = Some(now_timestamp());
    let phase_started_at = record.updated_at.clone().unwrap_or_else(now_timestamp);
    let metadata = record.ensure_metadata_object();
    record_lab_offload_phase_metadata(metadata, input.phase, &phase_started_at);
    metadata.insert("provider_state".to_string(), json!("pending"));
    if let Some(remote_workspace) = input.remote_workspace {
        metadata.insert("remote_workspace".to_string(), json!(remote_workspace));
    }
    if let Some(source_checkout) = input.source_checkout {
        metadata.insert("source_checkout".to_string(), source_checkout.clone());
    }
    if let Some(provider_rotation) = input.provider_rotation {
        metadata.insert("provider_rotation".to_string(), provider_rotation.clone());
    }
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Replace a pre-acceptance Lab proxy with the verified controller-local
/// outcome when an automatic placement discovers immutable runner identity
/// drift before daemon submission.
pub fn record_local_lab_identity_fallback(
    run_id: &str,
    runner_id: &str,
    identity_drift: &Value,
    fallback_reason: &str,
) -> Result<AgentTaskRunRecord> {
    record_local_lab_identity_fallback_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        runner_id,
        identity_drift,
        fallback_reason,
    )
}

/// The store-rooted counterpart of [`record_local_lab_identity_fallback`].
pub(crate) fn record_local_lab_identity_fallback_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    runner_id: &str,
    identity_drift: &Value,
    fallback_reason: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    if record.state.is_terminal() {
        return Ok(record);
    }
    if record.has_accepted_lab_handoff() || record.runner_job_id().is_some() {
        return Err(Error::validation_invalid_argument(
            "lab_identity_fallback",
            "cannot replace an accepted Lab runner job with a local fallback",
            Some(record.run_id.clone()),
            None,
        ));
    }

    record.updated_at = Some(now_timestamp());
    record.lab_handoff = None;
    let metadata = record.ensure_metadata_object();
    for key in [
        "runner_id",
        "runner_job_id",
        "runner_execution_record",
        "runner_submission_intent",
        "runner_handoff",
        "handoff_acceptance",
        "lab_admission_reservation",
        "lab_staging_controller_job_id",
        "lab_staging_controller_runner_id",
        "remote_workspace",
        "remote_command",
    ] {
        metadata.remove(key);
    }
    metadata.insert("phase".to_string(), json!("local_fallback"));
    metadata.insert(
        "phase_activity".to_string(),
        json!("Lab identity drift returned the attempt to controller-local execution"),
    );
    metadata.insert("provider_state".to_string(), json!("pending"));
    metadata.insert(
        "lab_identity_fallback".to_string(),
        json!({
            "schema": "homeboy/lab-identity-fallback/v1",
            "selected_runner": runner_id,
            "identity_drift": identity_drift,
            "fallback_reason": fallback_reason,
            "final_placement": "local",
            "runner_jobs_created": 0,
            "transport_retry_attempts": 0,
        }),
    );
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Record child setup executions against the controller proxy. A staging job
/// can outlive the foreground caller, so its runner IDs belong to the durable
/// phase record rather than only transient command output.
pub fn record_lab_offload_phase_executions(
    run_id: &str,
    phase: &str,
    execution_ids: impl IntoIterator<Item = String>,
) -> Result<AgentTaskRunRecord> {
    record_lab_offload_phase_executions_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        phase,
        execution_ids,
    )
}

/// The store-rooted counterpart of [`record_lab_offload_phase_executions`].
///
/// The terminal guard is read from the same record the execution ids are
/// written back onto, so a staging job cannot be recorded against a run that
/// another installation already finished (#7505).
pub(crate) fn record_lab_offload_phase_executions_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    phase: &str,
    execution_ids: impl IntoIterator<Item = String>,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    if record.state.is_terminal() {
        return Ok(record);
    }
    let execution_ids: Vec<String> = execution_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect();
    record.updated_at = Some(now_timestamp());
    let phase_started_at = record.updated_at.clone().unwrap_or_else(now_timestamp);
    let metadata = record.ensure_metadata_object();
    record_lab_offload_phase_metadata(metadata, phase, &phase_started_at);
    metadata.insert(
        "materialization_execution_ids".to_string(),
        json!(execution_ids),
    );
    metadata.insert(
        "materialization_resume".to_string(),
        json!("resume reuses the controller proxy and recorded completed staging"),
    );
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Bind the controller-owned staging job separately from the eventual runner job.
pub fn record_lab_staging_controller_job(
    run_id: &str,
    runner_id: &str,
    controller_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    record_lab_staging_controller_job_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        runner_id,
        controller_job_id,
    )
}

/// The store-rooted counterpart of [`record_lab_staging_controller_job`].
///
/// The staging job id is the controller's only handle on a materialization it
/// can outlive, so the terminal guard and the binding must name one record in
/// one installation (#7505).
pub(crate) fn record_lab_staging_controller_job_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    runner_id: &str,
    controller_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    if record.state.is_terminal() {
        return Ok(record);
    }
    record.updated_at = Some(now_timestamp());
    let started_at = record.updated_at.clone().unwrap_or_else(now_timestamp);
    let metadata = record.ensure_metadata_object();
    record_lab_offload_phase_metadata(metadata, "materializing", &started_at);
    metadata.insert(
        "lab_staging_controller_job_id".to_string(),
        json!(controller_job_id),
    );
    metadata.insert(
        "lab_staging_controller_runner_id".to_string(),
        json!(runner_id),
    );
    metadata.insert("materialization_owner".to_string(), json!("controller_job"));
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Name a reserved Lab admission on the durable run the moment it is taken,
/// before any further dispatch work can fail.
///
/// A reservation that exists only inside the controller process is unfindable:
/// a caller killed between reservation and runner acceptance leaves capacity
/// held by an admission nothing can associate with a run id, which is what
/// forced manual job-ID cancellation in #9163. The daemon's admission-lease
/// sweep still owns automatic reclaim; this write owns *identity*, so an
/// operator can see which run holds which reservation and when it self-expires.
pub fn record_lab_admission_reservation(
    run_id: &str,
    runner_id: &str,
    daemon_lease_id: &str,
    reservation_job_id: &str,
    lease_expires_at_ms: u64,
) -> Result<AgentTaskRunRecord> {
    record_lab_admission_reservation_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        runner_id,
        daemon_lease_id,
        reservation_job_id,
        lease_expires_at_ms,
    )
}

/// The store-rooted counterpart of [`record_lab_admission_reservation`].
///
/// The whole point of this write is that an operator can find the run holding a
/// reservation. A reservation named in one installation while the run it names
/// lives in another is exactly the unfindable state #9163 forced manual
/// job-id cancellation for, so the read and the write are one home (#7505).
pub(crate) fn record_lab_admission_reservation_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    runner_id: &str,
    daemon_lease_id: &str,
    reservation_job_id: &str,
    lease_expires_at_ms: u64,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    if record.state.is_terminal() {
        return Ok(record);
    }
    record.updated_at = Some(now_timestamp());
    let reserved_at = record.updated_at.clone().unwrap_or_else(now_timestamp);
    let record_run_id = record.run_id.clone();
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "lab_admission_reservation".to_string(),
        json!({
            "state": "reserved",
            "runner_id": runner_id,
            "daemon_lease_id": daemon_lease_id,
            "reservation_job_id": reservation_job_id,
            "lease_expires_at_ms": lease_expires_at_ms,
            "reserved_at": reserved_at,
            "reclaim": "the runner daemon reconciles this reservation automatically once its lease expires without a live controller",
            "cancel_command": format!("homeboy agent-task cancel {record_run_id}"),
        }),
    );
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Preserve the controller-stage terminal context on the durable parent after
/// its generic controller job has failed.
pub fn record_lab_staging_controller_failure(
    run_id: &str,
    phase: &str,
    controller_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    record_lab_staging_controller_failure_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        phase,
        controller_job_id,
    )
}

/// The store-rooted counterpart of [`record_lab_staging_controller_failure`].
///
/// This is terminal-stage evidence about one run, and the retry command it
/// publishes is only actionable in the installation the record lives in, so the
/// read and the write name the same store (#7505).
pub(crate) fn record_lab_staging_controller_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    phase: &str,
    controller_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "lab_staging_controller_failure".to_string(),
        json!({
            "phase": phase,
            "controller_job_id": controller_job_id,
            "classification": "lab_staging",
            "retry_command": format!("homeboy agent-task retry {run_id}"),
            "cleanup_status": "controller-owned cleanup pending terminal confirmation",
        }),
    );
    record.updated_at = Some(now_timestamp());
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

fn record_lab_offload_phase_metadata(
    metadata: &mut serde_json::Map<String, Value>,
    phase: &str,
    started_at: &str,
) {
    let previous_phase = metadata
        .get("phase")
        .and_then(Value::as_str)
        .map(str::to_string);
    if previous_phase.as_deref() != Some(phase) {
        if let Some(previous_phase) = previous_phase {
            if let Some(entry) = metadata
                .get_mut("phase_history")
                .and_then(Value::as_array_mut)
                .and_then(|entries| {
                    entries.iter_mut().rev().find(|entry| {
                        entry.get("phase").and_then(Value::as_str) == Some(previous_phase.as_str())
                            && entry.get("ended_at").is_none()
                    })
                })
            {
                entry["ended_at"] = json!(started_at);
            }
        }
        metadata
            .entry("phase_history".to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("phase history is an array")
            .push(json!({ "phase": phase, "started_at": started_at }));
    }
    metadata.insert("phase".to_string(), json!(phase));
    metadata.insert(
        "phase_activity".to_string(),
        json!(format!("Homeboy {phase}")),
    );
}

pub fn record_detached_lab_run(input: DetachedLabRunRecord<'_>) -> Result<AgentTaskRunRecord> {
    record_detached_lab_run_in_store(&AgentTaskLifecycleStore::from_current_environment()?, input)
}

/// The store-rooted counterpart of [`record_detached_lab_run`].
///
/// Lab acceptance is the single durable transfer of a run to a runner daemon:
/// it takes the handoff lock, reads (or resubmits) the record, validates the
/// accepted identity against what is already persisted, and writes the accepted
/// handoff back. Every one of those steps has to name the same installation —
/// an acceptance validated against one home's record and committed into
/// another's would silently permit two different runners to own the same run,
/// and the handoff lock would not have excluded either of them (#7505).
pub(crate) fn record_detached_lab_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    input: DetachedLabRunRecord<'_>,
) -> Result<AgentTaskRunRecord> {
    record_detached_lab_run_with_submission_in_store(
        lifecycle_store,
        input,
        &admitted_lab_offload_submission,
    )
}

/// Accept a detached Lab run, submitting a not-yet-present run through a
/// caller-supplied submission. See [`LabOffloadSubmission`].
///
/// This is the fall-through the acceptance path has always had: a runner
/// daemon can accept a job whose controller-side record does not exist here
/// yet, or exists only as an untyped legacy row. Naming the submission is what
/// lets a caller that has already decided admission keep this whole operation
/// inside the store it handed in.
pub(crate) fn record_detached_lab_run_with_submission_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    input: DetachedLabRunRecord<'_>,
    submit: LabOffloadSubmission<'_>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(input.run_id);
    let _lock = LabHandoffLock::lock_in_store(lifecycle_store, &run_id)?;
    let plan = detached_lab_plan(&run_id, &input);
    let mut record = match lifecycle_store.read_record(&run_id) {
        Ok(record) => record,
        Err(error)
            if error.code == ErrorCode::InternalJsonError
                && lifecycle_store.record_lacks_typed_metadata(&run_id)? =>
        {
            submit(lifecycle_store, &plan, &run_id)?
        }
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => {
            submit(lifecycle_store, &plan, &run_id)?
        }
        Err(error) => return Err(error),
    };
    if let Some(problem) = record.lab_handoff_validation_error() {
        return Err(Error::validation_invalid_argument(
            "lab_handoff",
            problem,
            Some(record.run_id.clone()),
            None,
        ));
    }
    // The accepted handoff is the controller-side attachment authority. Capture
    // the exact durable run binding here so later attachment writes cannot be
    // authorized by a substituted workspace claim. The claim ledger is a
    // permission gate over the retained workspace, so it is read from this
    // store's own roots rather than the process environment's (#7505).
    require_record_workspace_owner_in_store(&lifecycle_store.workspace_claim_store(), &record)?;
    if let Some(accepted) = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.state == AgentTaskLabHandoffState::Accepted
            && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
    }) {
        // Idempotent re-acceptance: the incoming acceptance names the same
        // run/runner/job as the already-accepted handoff. Route through the
        // shared `RunnerJobIdentity` so this agrees with every other
        // handoff-identity site rather than hand-rolling the tuple compare.
        // Both identities are scoped to this run, so the run id is `record.run_id`
        // on each side (the compare reduces to runner + job, as before).
        let accepted_identity = homeboy_core::lab_contract::RunnerJobIdentity::new(
            record.run_id.as_str(),
            accepted.runner_id.as_str(),
            accepted.runner_job_id.as_deref().unwrap_or_default(),
        );
        let incoming_identity = homeboy_core::lab_contract::RunnerJobIdentity::new(
            record.run_id.as_str(),
            input.runner_id,
            input.runner_job_id,
        );
        if accepted_identity.matches(&incoming_identity) {
            return Ok(record);
        }
        return Err(Error::validation_invalid_argument(
            "lab_handoff",
            format!(
                "Lab handoff for run '{}' is already accepted by runner '{}' job '{}'; refusing a different acceptance",
                record.run_id,
                accepted.runner_id,
                accepted.runner_job_id.as_deref().unwrap_or_default(),
            ),
            Some(record.run_id.clone()),
            None,
        ));
    }
    if record.lab_handoff.is_none() && record.runner_id().is_some_and(|id| id != input.runner_id) {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!(
                "Lab handoff for run '{}' is assigned to runner '{}'; refusing acceptance from '{}'",
                record.run_id,
                record.runner_id().unwrap_or_default(),
                input.runner_id,
            ),
            Some(record.run_id.clone()),
            None,
        ));
    }
    if let Some(pending) = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.state == AgentTaskLabHandoffState::Pending
            && handoff.authority == AgentTaskLabHandoffAuthority::Controller
    }) {
        if pending.runner_id != input.runner_id {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                format!(
                    "Lab handoff for run '{}' is pending acceptance by runner '{}'; refusing acceptance from '{}'",
                    record.run_id, pending.runner_id, input.runner_id,
                ),
                Some(record.run_id.clone()),
                None,
            ));
        }
    }
    let expired_unaccepted_handoff = record.state == AgentTaskRunState::Cancelled
        && record.lab_handoff.as_ref().is_some_and(|handoff| {
            handoff.state == AgentTaskLabHandoffState::Expired
                && handoff.authority == AgentTaskLabHandoffAuthority::Controller
                && handoff.runner_id == input.runner_id
        });
    if !expired_unaccepted_handoff
        && matches!(
            record.state,
            AgentTaskRunState::Succeeded
                | AgentTaskRunState::PartialRecoverable
                | AgentTaskRunState::PartialFailure
                | AgentTaskRunState::Failed
                | AgentTaskRunState::Cancelled
        )
    {
        // A terminal proxy must not be resurrected. A later runner job may
        // attach finalized evidence, but only from the original Lab runner.
        if record.runner_id() == Some(input.runner_id) {
            return Ok(record);
        }
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("agent-task run '{}' is already terminal", record.run_id),
            Some(record.run_id),
            None,
        ));
    }
    if let Err(error) = lifecycle_store.read_controller_plan(&run_id) {
        fail_missing_lab_attempt_plan_in_store(lifecycle_store, &mut record, &error)?;
        return Err(Error::internal_io(
            format!(
                "cannot bind Lab runner job because durable attempt plan is unavailable: {}",
                error.message
            ),
            Some(run_id),
        ));
    }
    record.updated_at = Some(now_timestamp());
    set_run_state(&mut record, AgentTaskRunState::Running);
    update_lifecycle_heartbeat(&mut record);
    for task in &mut record.tasks {
        if task.state == AgentTaskState::Queued {
            task.state = AgentTaskState::Running;
        }
    }
    let accepted_at = record.updated_at.clone();
    let accepted_at = accepted_at.unwrap_or_else(now_timestamp);
    let pending_handoff = record.lab_handoff.clone().unwrap_or_else(|| {
        AgentTaskLabHandoff::pending(
            input.runner_id,
            record.submitted_at.clone(),
            accepted_at.clone(),
        )
    });
    let mut accepted_handoff = pending_handoff.accepted(input.runner_job_id, accepted_at.clone());
    accepted_handoff.workspace_identity = record.workspace_identity.clone();
    accepted_handoff.workspace_lifecycle_revision = record.workspace_lifecycle_revision;
    accepted_handoff.workspace_owner_lease = record.workspace_owner_lease.clone();
    accepted_handoff.workspace_claim = record.workspace_claim.clone();
    record.lab_handoff = Some(accepted_handoff);
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!("lab_offload_detached_handoff"));
    if let Some(intent) = metadata.get_mut("runner_submission_intent") {
        intent["state"] = json!("accepted");
        intent["runner_job_id"] = json!(input.runner_job_id);
        intent["accepted_at"] = json!(accepted_at);
    }
    metadata.insert(
        "handoff_acceptance".to_string(),
        json!({
            "state": "accepted",
            "accepted_at": accepted_at,
            "runner_job_id": input.runner_job_id,
        }),
    );
    metadata.insert("phase".to_string(), json!("awaiting_runner_result"));
    metadata.insert(
        "phase_activity".to_string(),
        json!("controller handoff complete; awaiting authoritative runner daemon result"),
    );
    metadata.insert("provider_state".to_string(), json!("active"));
    let source_snapshot = metadata
        .get("source_checkout")
        .cloned()
        .unwrap_or(Value::Null);
    metadata.insert(
        "runner_handoff".to_string(),
        json!({
            "state": "in_flight",
            "authority": "runner_daemon",
            "identity": {
                "run_id": run_id,
                "runner_id": input.runner_id,
                "runner_job_id": input.runner_job_id,
            },
            "source_snapshot": source_snapshot,
            "continuation": {
                "intent": "reconcile_runner_job",
                "on_active": "retain_running",
                "on_terminal": "project_authoritative_daemon_result_once",
            },
        }),
    );
    metadata.insert("runner_id".to_string(), json!(input.runner_id));
    metadata.insert("runner_job_id".to_string(), json!(input.runner_job_id));
    metadata.insert(
        "remote_workspace".to_string(),
        json!(input.remote_workspace),
    );
    metadata.insert("remote_command".to_string(), json!(input.remote_command));
    metadata.insert(
        "runner_execution_record".to_string(),
        serde_json::to_value(
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::in_flight(
                input.runner_job_id,
                input.runner_id,
                "daemon",
            )
            .with_job_id(input.runner_job_id)
            .with_agent_task_run_id(&run_id),
        )
        .unwrap_or(Value::Null),
    );
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    metadata.remove(METADATA_KEY_STALE_RUNNING);
    metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Persist the controller-owned Lab proxy inside an explicitly rooted store.
///
/// There is deliberately no ambient sibling of this any more. Its two callers
/// — `record_lab_offload_planned` and `record_lab_offload_phase` — are the
/// public entry points, and each now resolves exactly one root and hands it
/// here, rather than this helper resolving a second one behind whichever store
/// they were given (#7505).
fn record_lab_offload_proxy_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    requested_run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
    durable_plan: Option<&AgentTaskPlan>,
    submit: LabOffloadSubmission<'_>,
) -> Result<AgentTaskRunRecord> {
    validate_lab_handoff_plan(durable_plan)?;
    let run_id = sanitize_run_id(requested_run_id);
    let input = DetachedLabRunRecord {
        run_id: &run_id,
        runner_id,
        // This placeholder is removed immediately below. Keeping construction
        // centralized lets the proxy and bound child share one plan shape.
        runner_job_id: "unbound",
        remote_workspace,
        remote_command,
    };
    let mut plan = detached_lab_plan(&run_id, &input);
    let task = &mut plan.tasks[0];
    if let Some(inputs) = task.inputs.as_object_mut() {
        inputs.remove("runner_job_id");
    }
    task.source_refs.clear();
    if let Some(materialization) = task.workspace.materialization.as_object_mut() {
        materialization.remove("runner_job_id");
    }
    if let Some(metadata) = task.metadata.as_object_mut() {
        metadata.remove("runner_job_id");
    }
    if let Some(metadata) = plan.metadata.as_object_mut() {
        metadata.remove("runner_job_id");
    }
    let mut record = match lifecycle_store.read_record(&run_id) {
        Ok(record) => record,
        Err(error)
            if error.code == ErrorCode::InternalJsonError
                && lifecycle_store.record_lacks_typed_metadata(&run_id)? =>
        {
            submit(lifecycle_store, durable_plan.unwrap_or(&plan), &run_id)?
        }
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => {
            submit(lifecycle_store, durable_plan.unwrap_or(&plan), &run_id)?
        }
        Err(error) => return Err(error),
    };
    if let Some(problem) = record.lab_handoff_validation_error() {
        return Err(Error::validation_invalid_argument(
            "lab_handoff",
            problem,
            Some(record.run_id.clone()),
            None,
        ));
    }
    if let Some(accepted) = record.lab_handoff.as_ref().filter(|handoff| {
        handoff.state == AgentTaskLabHandoffState::Accepted
            && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
    }) {
        if accepted.runner_id == runner_id {
            return Ok(record);
        }
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!(
                "Lab handoff for run '{}' is already accepted by runner '{}'; refusing resume on '{}'",
                record.run_id, accepted.runner_id, runner_id,
            ),
            Some(record.run_id.clone()),
            None,
        ));
    }
    // A previous interruption may have committed the record but not its plan.
    // Repair from the controller-compiled plan before exposing another handoff
    // phase; without it the runner would later create a fake running attempt.
    if lifecycle_store.read_controller_plan(&run_id).is_err() {
        if let Some(durable_plan) = durable_plan {
            let plan_path = lifecycle_store.write_controller_plan(&run_id, durable_plan)?;
            record.plan_path = plan_path.display().to_string();
        } else {
            let error = Error::internal_io(
                "durable attempt plan is unavailable during Lab handoff recovery",
                Some(record.plan_path.clone()),
            );
            fail_missing_lab_attempt_plan_in_store(lifecycle_store, &mut record, &error)?;
            return Err(error);
        }
    }
    record.plan_path = lifecycle_store
        .controller_plan_path(&run_id)
        .display()
        .to_string();
    if record.state.is_terminal() {
        return Ok(record);
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!("lab_offload_controller_proxy"));
    // This record is the controller's durable projection of a runner handoff.
    // It remains controller-owned until a runner-local record is independently
    // discovered, so controller-generated commands must keep resolving here.
    metadata.insert("lifecycle_store_owner".to_string(), json!("controller"));
    metadata.insert("runner_id".to_string(), json!(runner_id));
    if !durable_plan
        .map(|plan| plan.services.is_empty())
        .unwrap_or(true)
    {
        metadata.insert(
            "managed_service_supervisor".to_string(),
            json!({
                "runner_id": runner_id,
                "remote_workspace": remote_workspace,
                "state_ref": format!("homeboy://runner/{runner_id}/agent-task-runs/{run_id}/service-supervisor/state"),
                "operations": ["status", "stop", "reconcile"],
            }),
        );
    }
    if remote_workspace != "pending" {
        metadata.insert("remote_workspace".to_string(), json!(remote_workspace));
    }
    if !remote_command.is_empty() {
        metadata.insert("remote_command".to_string(), json!(remote_command));
    }
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    metadata.remove(METADATA_KEY_STALE_RUNNING);
    metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
    metadata.insert(
        "runner_execution_record".to_string(),
        serde_json::to_value(
            homeboy_core::runner_execution_envelope::RunnerExecutionRecord::planned(
                &run_id, runner_id, "daemon",
            )
            .with_agent_task_run_id(&run_id),
        )
        .unwrap_or(Value::Null),
    );
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

fn validate_lab_handoff_plan(durable_plan: Option<&AgentTaskPlan>) -> Result<()> {
    if let Some(plan) = durable_plan {
        plan.validate_managed_services().map_err(|message| {
            Error::validation_invalid_argument("services.cleanup_deadline_ms", message, None, None)
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod admission_tests {
    use std::collections::HashMap;

    use super::*;
    use crate::agent_task_scheduler::{AgentTaskManagedService, AgentTaskManagedServiceLifecycle};

    #[test]
    fn lab_handoff_admission_rejects_an_invalid_managed_service() {
        let mut plan = AgentTaskPlan::new("invalid-lab-service", Vec::new());
        plan.services.push(AgentTaskManagedService {
            version: AgentTaskManagedService::VERSION,
            id: "invalid".to_string(),
            command: vec!["fixture".to_string()],
            cwd: None,
            env: HashMap::new(),
            env_allowlist: Vec::new(),
            secret_env: Vec::new(),
            secret_env_plan: None,
            host: "127.0.0.1".to_string(),
            port: None,
            port_env: None,
            socket_handoff: false,
            readiness: None,
            cleanup_deadline_ms: 0,
            public_url: None,
            browser_origin_probe: None,
            lifecycle: AgentTaskManagedServiceLifecycle::Plan,
            target: None,
        });

        let error = validate_lab_handoff_plan(Some(&plan)).expect_err("invalid plan");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn identity_fallback_replaces_only_an_unaccepted_proxy_with_local_evidence() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "identity-fallback";
            let plan = AgentTaskPlan::new(run_id, Vec::new());
            submit_plan(&plan, Some(run_id)).expect("persist run");
            record_lab_offload_phase(
                run_id,
                "homeboy-lab",
                "provider_dispatch",
                Some("/runner/workspace"),
                None,
                None,
                Some(&plan),
            )
            .expect("seed unaccepted proxy");

            record_local_lab_identity_fallback(
                run_id,
                "homeboy-lab",
                &json!({ "mismatch_predicate": "test_identity_drift" }),
                "runner_identity_drift: test",
            )
            .expect("record fallback");

            let record = store::read_record(run_id).expect("fallback record");
            assert!(record.lab_handoff.is_none());
            assert!(record.metadata.get("runner_id").is_none());
            assert!(record.metadata.get("runner_job_id").is_none());
            assert!(record.metadata.get("runner_execution_record").is_none());
            assert_eq!(
                record.metadata["lab_identity_fallback"]["selected_runner"],
                "homeboy-lab"
            );
            assert_eq!(
                record.metadata["lab_identity_fallback"]["final_placement"],
                "local"
            );
            assert_eq!(
                record.metadata["lab_identity_fallback"]["runner_jobs_created"],
                0
            );
            assert_eq!(
                record.metadata["lab_identity_fallback"]["transport_retry_attempts"],
                0
            );
        });
    }
}

/// Terminalize a Lab attempt whose durable plan is unrecoverable.
///
/// The failure is decided from a plan read out of one store, so it is written
/// back to that same store: a terminal failure recorded in a different
/// installation than the one that was missing the plan would be evidence about
/// a run nobody asked about (#7505).
fn fail_missing_lab_attempt_plan_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    error: &Error,
) -> Result<()> {
    record.updated_at = Some(now_timestamp());
    set_run_state(record, AgentTaskRunState::Failed);
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = AgentTaskState::Failed;
        }
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "pre_execution_failure".to_string(),
        json!({
            "phase": "lab_attempt_plan_recovery",
            "error": error.message,
        }),
    );
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    lifecycle_store.write_record(record)
}

fn detached_lab_plan(run_id: &str, input: &DetachedLabRunRecord<'_>) -> AgentTaskPlan {
    let task = AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: format!("{run_id}-lab-handoff"),
        group_key: Some("lab-offload".to_string()),
        parent_plan_id: None,
        executor: AgentTaskExecutor {
            backend: "homeboy-lab".to_string(),
            selector: Some(input.runner_id.to_string()),
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: Value::Null,
        },
        instructions: "Detached Lab agent-task run handed off to a durable runner job.".to_string(),
        inputs: json!({
            "runner_id": input.runner_id,
            "runner_job_id": input.runner_job_id,
            "remote_workspace": input.remote_workspace,
            "remote_command": input.remote_command,
        }),
        source_refs: vec![AgentTaskSourceRef {
            kind: "lab-offload-runner-job".to_string(),
            uri: format!(
                "homeboy://runner/{}/job/{}",
                input.runner_id, input.runner_job_id
            ),
            revision: None,
        }],
        workspace: AgentTaskWorkspace {
            mode: AgentTaskWorkspaceMode::Existing,
            root: Some(input.remote_workspace.to_string()),
            kind: Some("lab-offload".to_string()),
            cleanup: Some("preserve".to_string()),
            materialization: json!({
                "runner_id": input.runner_id,
                "runner_job_id": input.runner_job_id,
            }),
            ..AgentTaskWorkspace::default()
        },
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        output_declarations: Vec::new(),
        runtime_tools: Vec::new(),
        metadata: json!({
            "kind": "lab_offload_detached_handoff",
            "runner_id": input.runner_id,
            "runner_job_id": input.runner_job_id,
        }),
    };
    let mut plan = AgentTaskPlan::new(format!("{run_id}-lab-offload"), vec![task]);
    plan.group_key = Some("lab-offload".to_string());
    plan.metadata = json!({
        "kind": "lab_offload_detached_handoff",
        "runner_id": input.runner_id,
        "runner_job_id": input.runner_job_id,
        "remote_workspace": input.remote_workspace,
    });
    plan
}

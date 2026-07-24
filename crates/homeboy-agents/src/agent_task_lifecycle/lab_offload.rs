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

/// Atomically persist a daemon-accepted Lab job before a caller can inspect its
/// snapshot. The typed identity keeps every acceptance path on the canonical
/// run/runner/job comparison used by reconciliation and terminal projection.
pub fn bind_accepted_lab_runner_job(
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
    record_detached_lab_run(DetachedLabRunRecord {
        run_id: &identity.run_id,
        runner_id: &identity.runner_id,
        runner_job_id: &identity.runner_job_id,
        remote_workspace,
        remote_command,
    })
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
    record_lab_offload_proxy(
        &input.run_id,
        input.runner_id,
        input.remote_workspace,
        input.remote_command,
        input.durable_plan,
    )
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
    let placeholder_workspace = remote_workspace.unwrap_or("pending");
    let mut record = record_lab_offload_proxy(
        requested_run_id,
        runner_id,
        placeholder_workspace,
        &[],
        durable_plan,
    )?;
    if record.state.is_terminal() {
        return Ok(record);
    }
    record.updated_at = Some(now_timestamp());
    let phase_started_at = record.updated_at.clone().unwrap_or_else(now_timestamp);
    let metadata = record.ensure_metadata_object();
    record_lab_offload_phase_metadata(metadata, phase, &phase_started_at);
    metadata.insert("provider_state".to_string(), json!("pending"));
    if let Some(remote_workspace) = remote_workspace {
        metadata.insert("remote_workspace".to_string(), json!(remote_workspace));
    }
    if let Some(source_checkout) = source_checkout {
        metadata.insert("source_checkout".to_string(), source_checkout.clone());
    }
    if let Some(provider_rotation) = provider_rotation {
        metadata.insert("provider_rotation".to_string(), provider_rotation.clone());
    }
    store::write_record(&record)?;
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
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
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
    store::write_record(&record)?;
    Ok(record)
}

/// Bind the controller-owned staging job separately from the eventual runner job.
pub fn record_lab_staging_controller_job(
    run_id: &str,
    runner_id: &str,
    controller_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
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
    store::write_record(&record)?;
    Ok(record)
}

/// Preserve the controller-stage terminal context on the durable parent after
/// its generic controller job has failed.
pub fn record_lab_staging_controller_failure(
    run_id: &str,
    phase: &str,
    controller_job_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
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
    store::write_record(&record)?;
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
    let run_id = sanitize_run_id(input.run_id);
    let _lock = LabHandoffLock::lock(&run_id)?;
    let plan = detached_lab_plan(&run_id, &input);
    let mut record = match store::read_record(&run_id) {
        Ok(record) => record,
        Err(error)
            if error.code == ErrorCode::InternalJsonError
                && store::record_lacks_typed_metadata(&run_id)? =>
        {
            submit_plan(&plan, Some(&run_id))?
        }
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => {
            submit_plan(&plan, Some(&run_id))?
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
    if let Err(error) = store::read_controller_plan(&run_id) {
        fail_missing_lab_attempt_plan(&mut record, &error)?;
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
    record.lab_handoff = Some(pending_handoff.accepted(input.runner_job_id, accepted_at.clone()));
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
    store::write_record(&record)?;
    Ok(record)
}

fn record_lab_offload_proxy(
    requested_run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    remote_command: &[String],
    durable_plan: Option<&AgentTaskPlan>,
) -> Result<AgentTaskRunRecord> {
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
    let mut record = match store::read_record(&run_id) {
        Ok(record) => record,
        Err(error)
            if error.code == ErrorCode::InternalJsonError
                && store::record_lacks_typed_metadata(&run_id)? =>
        {
            submit_plan(durable_plan.unwrap_or(&plan), Some(&run_id))?
        }
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => {
            submit_plan(durable_plan.unwrap_or(&plan), Some(&run_id))?
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
    if store::read_controller_plan(&run_id).is_err() {
        if let Some(durable_plan) = durable_plan {
            let plan_path = store::write_plan(&run_id, durable_plan)?;
            record.plan_path = plan_path.display().to_string();
        } else {
            let error = Error::internal_io(
                "durable attempt plan is unavailable during Lab handoff recovery",
                Some(record.plan_path.clone()),
            );
            fail_missing_lab_attempt_plan(&mut record, &error)?;
            return Err(error);
        }
    }
    record.plan_path = store::controller_plan_path(&run_id)?.display().to_string();
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
    if remote_workspace != "pending" {
        metadata.insert("remote_workspace".to_string(), json!(remote_workspace));
    }
    if !remote_command.is_empty() {
        metadata.insert("remote_command".to_string(), json!(remote_command));
    }
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
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
    store::write_record(&record)?;
    Ok(record)
}

fn fail_missing_lab_attempt_plan(record: &mut AgentTaskRunRecord, error: &Error) -> Result<()> {
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
    store::write_record(record)
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

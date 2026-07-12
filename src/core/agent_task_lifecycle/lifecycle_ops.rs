use super::*;

pub fn submit_plan(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let run_id = requested_run_id
        .map(sanitize_run_id)
        .unwrap_or_else(default_run_id);
    let plan_path = store::write_plan(&run_id, plan)?;

    let mut metadata = json!({
        "task_count": plan.tasks.len(),
        "max_concurrency": plan.options.max_concurrency,
        "provider_run_ids": [],
        "lifecycle_schema": RUN_LIFECYCLE_RECORD_SCHEMA,
        "note": "submitted tasks are durable; provider run ids are recorded after an executor returns them as generic artifacts or evidence refs"
    });
    if let Ok(runner_id) = std::env::var(crate::core::runner::RUNNER_ID_ENV) {
        if !runner_id.trim().is_empty() {
            metadata["runner_id"] = json!(runner_id);
        }
    }
    if let Some(route) = crate::core::notification_route::current() {
        route.insert_into_metadata(&mut metadata);
    }

    let record = AgentTaskRunRecord {
        schema: schemas::RUN.to_string(),
        run_id,
        plan_id: plan.plan_id.clone(),
        state: AgentTaskRunState::Queued,
        submitted_at: now_timestamp(),
        updated_at: None,
        plan_path: plan_path.display().to_string(),
        aggregate_path: None,
        totals: None,
        tasks: plan.tasks.iter().map(queued_task).collect(),
        artifact_refs: Vec::new(),
        provider_handles: Vec::new(),
        latest_executor_evidence: None,
        lifecycle: lifecycle_for_submitted_plan(plan),
        metadata,
    };
    store::write_record(&record)?;
    Ok(record)
}

/// Bind an inherited route when a detached workload recreates an agent-task run.
pub fn persist_notification_route(
    run_id: &str,
    route: &crate::core::notification_route::NotificationRoute,
) -> Result<()> {
    let mut record = store::read_record(run_id)?;
    route.insert_into_metadata(&mut record.metadata);
    store::write_record(&record)
}

pub fn record_completed_run(
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let mut record = submit_plan(plan, requested_run_id)?;
    record_aggregate(&mut record, plan, aggregate)
}

pub fn load_plan(run_id: &str) -> Result<AgentTaskPlan> {
    let record = store::read_record(&resolve_run_id(run_id)?)?;
    store::read_plan_path(&record.plan_path)
}

pub fn mark_running(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if record.state == AgentTaskRunState::Running && record.owner_process_is_running() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already running under pid {}",
                record.run_id,
                record.owner_pid().unwrap_or_default()
            ),
            Some(record.run_id),
            None,
        ));
    }
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    let reclaimed_stale = record.state == AgentTaskRunState::Running;
    record.updated_at = Some(now_timestamp());
    set_run_state(&mut record, AgentTaskRunState::Running);
    update_lifecycle_heartbeat(&mut record);
    for task in &mut record.tasks {
        if task.state == AgentTaskState::Queued {
            task.state = AgentTaskState::Running;
        }
    }
    record.record_runner_metadata(reclaimed_stale);
    store::write_record(&record)?;
    Ok(record)
}

#[cfg(test)]
pub(crate) fn rewrite_record_for_test<F>(run_id: &str, mut rewrite: F) -> Result<AgentTaskRunRecord>
where
    F: FnMut(&mut AgentTaskRunRecord),
{
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    rewrite(&mut record);
    store::write_record(&record)?;
    Ok(record)
}

pub fn claim_next_queued_run() -> Result<Option<AgentTaskRunRecord>> {
    let mut queued: Vec<AgentTaskRunRecord> = store::read_records()?
        .into_iter()
        .filter(|record| record.state == AgentTaskRunState::Queued)
        .collect();
    queued.sort_by(|left, right| {
        left.submitted_at
            .cmp(&right.submitted_at)
            .then_with(|| left.run_id.cmp(&right.run_id))
    });

    for record in queued {
        match mark_running(&record.run_id) {
            Ok(claimed) => return Ok(Some(claimed)),
            Err(error) if error.code == ErrorCode::ValidationInvalidArgument => continue,
            Err(error) => return Err(error),
        }
    }

    Ok(None)
}

pub fn record_run_aggregate(
    run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    record_aggregate(&mut record, plan, aggregate)
}

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    let requested_run_id = sanitize_run_id(run_id);
    let resolved_run_id = resolve_run_id(run_id)?;
    let mut record = store::read_record(&resolved_run_id)?;
    if let (Ok(aggregate), Ok(plan)) = (
        store::read_aggregate(&record.run_id),
        store::read_plan_path(&record.plan_path),
    ) {
        let aggregate_path = store::aggregate_path(&record.run_id)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "aggregate.json".to_string());
        let mut reconciled = record.clone();
        apply_aggregate_to_record(&mut reconciled, &plan, &aggregate, aggregate_path);

        if reconciled != record {
            if let Err(error) = store::write_record(&reconciled) {
                reconciled
                    .ensure_metadata_object()
                    .insert("finalization_error".to_string(), json!(error.message));
            }

            record = reconciled;
        }
    }
    reconcile_runner_job_terminal_state(&mut record);
    record.annotate_stale_running();
    if requested_run_id != record.run_id {
        if let Ok(index) = store::read_cook_index(&requested_run_id) {
            let metadata = record.ensure_metadata_object();
            metadata.insert("cook_alias".to_string(), json!(requested_run_id));
            metadata.insert(
                "cook_index".to_string(),
                serde_json::to_value(index).unwrap_or(Value::Null),
            );
        }
    }
    Ok(record)
}

fn reconcile_runner_job_terminal_state(record: &mut AgentTaskRunRecord) {
    if record.state != AgentTaskRunState::Running {
        return;
    }
    let (Some(runner_id), Some(job_id)) = (
        record.runner_id().map(str::to_string),
        record.runner_job_id().map(str::to_string),
    ) else {
        return;
    };
    let Ok(snapshot) = crate::core::runners::runner_job_log_snapshot(&runner_id, &job_id) else {
        return;
    };
    if !matches!(
        snapshot.job.status,
        crate::core::api_jobs::JobStatus::Succeeded
            | crate::core::api_jobs::JobStatus::Failed
            | crate::core::api_jobs::JobStatus::Cancelled
    ) {
        return;
    }
    apply_runner_job_terminal_state(record, snapshot.job.status, &snapshot.events);
    let _ = store::write_record(record);
}

pub(crate) fn apply_runner_job_terminal_state(
    record: &mut AgentTaskRunRecord,
    status: crate::core::api_jobs::JobStatus,
    events: &[crate::core::api_jobs::JobEvent],
) {
    let (run_state, task_state) = match status {
        crate::core::api_jobs::JobStatus::Succeeded => {
            (AgentTaskRunState::Succeeded, AgentTaskState::Succeeded)
        }
        crate::core::api_jobs::JobStatus::Cancelled => {
            (AgentTaskRunState::Cancelled, AgentTaskState::Cancelled)
        }
        crate::core::api_jobs::JobStatus::Failed => {
            (AgentTaskRunState::Failed, AgentTaskState::Failed)
        }
        _ => return,
    };
    record.updated_at = Some(now_timestamp());
    set_run_state(record, run_state);
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = task_state;
        }
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert("runner_job_status".to_string(), json!(status));
    metadata.insert("runner_job_events".to_string(), json!(events));
    metadata.insert(
        "retryable".to_string(),
        json!(run_state != AgentTaskRunState::Succeeded),
    );
    metadata.remove("stale_running");
    metadata.remove("stale_running_reason");
}

pub fn run_status(run_id: &str, since_cursor: Option<u64>) -> Result<AgentTaskRunStatus> {
    let record = status(run_id)?;
    let (events, artifact_refs) = match store::read_aggregate(&record.run_id) {
        Ok(aggregate) => {
            let refs = artifact_refs_for_outcomes(&aggregate.outcomes);
            (aggregate.events, refs)
        }
        Err(_) => (queued_events(&record.tasks), record.artifact_refs.clone()),
    };
    let normalized_events = normalize_progress_events(&record.run_id, &events, &artifact_refs);
    let latest_event_cursor = normalized_events
        .last()
        .map(|event| event.sequence)
        .unwrap_or(0);
    let cursor = since_cursor.unwrap_or(0);
    let normalized_events = normalized_events
        .into_iter()
        .filter(|event| event.sequence > cursor)
        .collect();

    Ok(AgentTaskRunStatus {
        schema: schemas::RUN_STATUS.to_string(),
        run_id: record.run_id,
        plan_id: record.plan_id,
        state: record.state,
        submitted_at: record.submitted_at,
        updated_at: record.updated_at,
        totals: record
            .totals
            .unwrap_or_else(|| totals_for_tasks(&record.tasks)),
        latest_event_cursor,
        artifact_refs: record.artifact_refs,
        normalized_events,
    })
}

pub fn list_records() -> Result<Vec<AgentTaskRunRecord>> {
    let mut records = Vec::new();
    for record in store::read_records()? {
        match status(&record.run_id) {
            Ok(record) => records.push(record),
            Err(error) => eprintln!(
                "Warning: skipping malformed agent-task run status for {}: {}",
                record.run_id, error.message
            ),
        }
    }
    records.sort_by(|left, right| {
        right
            .updated_at
            .as_ref()
            .unwrap_or(&right.submitted_at)
            .cmp(left.updated_at.as_ref().unwrap_or(&left.submitted_at))
            .then_with(|| right.submitted_at.cmp(&left.submitted_at))
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    Ok(records)
}

pub fn run_record_exists(run_id: &str) -> Result<bool> {
    store::record_exists(&sanitize_run_id(run_id))
}

#[derive(Debug, Clone)]
pub struct DetachedLabRunRecord<'a> {
    pub run_id: &'a str,
    pub runner_id: &'a str,
    pub runner_job_id: &'a str,
    pub remote_workspace: &'a str,
    pub remote_command: &'a [String],
}

pub fn record_detached_lab_run(input: DetachedLabRunRecord<'_>) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(input.run_id);
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
    record.updated_at = Some(now_timestamp());
    set_run_state(&mut record, AgentTaskRunState::Running);
    for task in &mut record.tasks {
        if task.state == AgentTaskState::Queued {
            task.state = AgentTaskState::Running;
        }
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert("kind".to_string(), json!("lab_offload_detached_handoff"));
    metadata.insert("runner_id".to_string(), json!(input.runner_id));
    metadata.insert("runner_job_id".to_string(), json!(input.runner_job_id));
    metadata.insert(
        "remote_workspace".to_string(),
        json!(input.remote_workspace),
    );
    metadata.insert("remote_command".to_string(), json!(input.remote_command));
    metadata.insert("retryable".to_string(), json!(true));
    metadata.remove("stale_running");
    metadata.remove("stale_running_reason");
    store::write_record(&record)?;
    Ok(record)
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

pub fn mark_resuming(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    let metadata = record.ensure_metadata_object();
    metadata.insert("resume_requested_at".to_string(), json!(now_timestamp()));
    store::write_record(&record)?;
    mark_running(run_id)
}

pub fn retry(run_id: &str, requested_run_id: Option<&str>) -> Result<AgentTaskRunRecord> {
    let source = store::read_record(&resolve_run_id(run_id)?)?;
    let plan = store::read_plan_path(&source.plan_path)?;
    let mut retry = submit_plan(&plan, requested_run_id)?;
    let metadata = retry.ensure_metadata_object();
    if let Some(route) =
        crate::core::notification_route::NotificationRoute::from_metadata(&source.metadata)
    {
        // Retries are new durable runs, but retain the initiating route. Resume
        // operates on the same record and therefore needs no copy.
        metadata.insert(
            crate::core::notification_route::NOTIFICATION_ROUTE_METADATA_KEY.to_string(),
            serde_json::to_value(route).expect("notification route is serializable"),
        );
    }
    metadata.insert("retry_of".to_string(), json!(source.run_id));
    metadata.insert("retry_requested_at".to_string(), json!(now_timestamp()));
    store::write_record(&retry)?;
    Ok(retry)
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    let run_id = resolve_run_id(run_id)?;
    let record = store::read_record(&run_id)?;
    let (events, artifact_refs) = match store::read_aggregate(&run_id) {
        Ok(aggregate) => {
            let refs = artifact_refs_for_outcomes(&aggregate.outcomes);
            (aggregate.events, refs)
        }
        Err(_) => (queued_events(&record.tasks), record.artifact_refs.clone()),
    };
    let normalized_events = normalize_progress_events(&run_id, &events, &artifact_refs);
    Ok(AgentTaskRunLog {
        schema: schemas::RUN_LOG.to_string(),
        run_id,
        events,
        normalized_events,
    })
}

pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    let run_id = resolve_run_id(run_id)?;
    let record = store::read_record(&run_id)?;
    let aggregate = store::read_aggregate(&run_id).ok();
    let latest_executor_evidence = record.latest_executor_evidence.as_ref();
    Ok(AgentTaskRunArtifacts {
        schema: schemas::RUN_ARTIFACTS.to_string(),
        run_id,
        artifacts: aggregate_artifacts(aggregate.as_ref()),
        evidence_refs: aggregate_evidence_refs(aggregate.as_ref(), latest_executor_evidence),
    })
}

pub fn aggregate_source(run_id: &str) -> Result<(String, PathBuf)> {
    let record = store::read_record(&resolve_run_id(run_id)?)?;
    record.aggregate_path.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' has no aggregate artifact yet",
                record.run_id
            ),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let aggregate = store::read_aggregate(&record.run_id)?;
    let raw = serde_json::to_string_pretty(&aggregate).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("serialize agent-task aggregate {}", record.run_id)),
        )
    })?;
    let path = store::aggregate_path(&record.run_id)?;
    Ok((raw, path))
}

pub fn record_cook_attempt(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
) -> Result<AgentTaskCookIndex> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    let recorded_at = now_timestamp();
    let metadata = record.ensure_metadata_object();
    metadata.insert("cook_id".to_string(), json!(sanitize_run_id(cook_id)));
    metadata.insert("cook_attempt".to_string(), json!(attempt));
    store::write_record(&record)?;
    store::write_cook_index_attempt(cook_id, attempt, run_id, recorded_at)
}

pub fn cook_index(cook_id: &str) -> Result<AgentTaskCookIndex> {
    store::read_cook_index(&sanitize_run_id(cook_id))
}

fn resolve_run_id(run_id: &str) -> Result<String> {
    let run_id = sanitize_run_id(run_id);
    match store::read_cook_index(&run_id) {
        Ok(index) => Ok(index.latest_run_id),
        Err(_) => Ok(run_id),
    }
}

pub fn record_promotion(run_id: &str, promotion: Value) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    record.updated_at = Some(now_timestamp());
    let metadata = record.ensure_metadata_object();
    let promotions = metadata
        .entry("promotions".to_string())
        .or_insert_with(|| json!([]));
    if !promotions.is_array() {
        *promotions = json!([]);
    }
    promotions
        .as_array_mut()
        .expect("promotions array")
        .push(promotion.clone());
    metadata.insert("latest_promotion".to_string(), promotion);
    store::write_record(&record)?;
    Ok(record)
}

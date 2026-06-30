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

pub fn record_completed_run(
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let mut record = submit_plan(plan, requested_run_id)?;
    record_aggregate(&mut record, plan, aggregate)
}

pub fn load_plan(run_id: &str) -> Result<AgentTaskPlan> {
    let record = store::read_record(&sanitize_run_id(run_id))?;
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
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
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
    record.annotate_stale_running();
    Ok(record)
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
    let source = store::read_record(&sanitize_run_id(run_id))?;
    let plan = store::read_plan_path(&source.plan_path)?;
    let mut retry = submit_plan(&plan, requested_run_id)?;
    let metadata = retry.ensure_metadata_object();
    metadata.insert("retry_of".to_string(), json!(source.run_id));
    metadata.insert("retry_requested_at".to_string(), json!(now_timestamp()));
    store::write_record(&retry)?;
    Ok(retry)
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    let run_id = sanitize_run_id(run_id);
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
    let run_id = sanitize_run_id(run_id);
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
    let record = store::read_record(&sanitize_run_id(run_id))?;
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

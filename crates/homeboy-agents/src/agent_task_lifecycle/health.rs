use super::*;
use homeboy_core::observation::{ObservationStore, RunRecord};

pub(crate) const HEALTH_SAMPLE_LIMIT: usize = 20;
const QUARANTINE_KEY: &str = "agent_task_lifecycle_quarantine";

pub(crate) fn diagnose_run(
    run: &RunRecord,
) -> std::result::Result<AgentTaskRunRecord, AgentTaskRecordHealthItem> {
    let quarantined = run.metadata_json.get(QUARANTINE_KEY).is_some();
    let remediation = if quarantined {
        "inspect metadata.agent_task_lifecycle_quarantine and restore a durable plan before rerunning reconciliation"
    } else {
        "run `homeboy agent-task reconcile-records --dry-run` to inspect repair evidence"
    };
    let record = store::record_from_run(run).map_err(|_| AgentTaskRecordHealthItem {
        run_id: run.id.clone(),
        reason: if run.metadata_json.get("agent_task_run").is_none() {
            AgentTaskRecordHealthReason::MissingMetadata
        } else {
            AgentTaskRecordHealthReason::MalformedMetadata
        },
        quarantined,
        remediation: remediation.to_string(),
    })?;
    let reason = if record.lab_handoff_validation_error().is_some() {
        Some(AgentTaskRecordHealthReason::MalformedMetadata)
    } else if record.schema != schemas::RUN
        || record.lifecycle.schema != RUN_LIFECYCLE_RECORD_SCHEMA
    {
        Some(AgentTaskRecordHealthReason::LegacySchema)
    } else if RunExecutionState::from(record.state) != record.lifecycle.execution.state {
        Some(AgentTaskRecordHealthReason::ConflictingProjections)
    } else {
        None
    };
    match reason {
        Some(reason) => Err(AgentTaskRecordHealthItem {
            run_id: run.id.clone(),
            reason,
            quarantined,
            remediation: remediation.to_string(),
        }),
        None => Ok(record),
    }
}

pub(crate) fn record_health_item(
    health: &mut AgentTaskRecordHealthSummary,
    item: AgentTaskRecordHealthItem,
) {
    match item.reason {
        AgentTaskRecordHealthReason::MissingMetadata
        | AgentTaskRecordHealthReason::MalformedMetadata => health.malformed += 1,
        AgentTaskRecordHealthReason::LegacySchema => health.legacy += 1,
        AgentTaskRecordHealthReason::ConflictingProjections => health.conflicting += 1,
    }
    if item.quarantined {
        health.quarantined += 1;
    }
    if health.samples.len() < HEALTH_SAMPLE_LIMIT {
        health.samples.push(item);
    }
}

pub fn record_health_summary() -> Result<AgentTaskRecordHealthSummary> {
    Ok(store::read_records_with_health()?.1)
}

pub fn reconcile_record_health(dry_run: bool) -> Result<AgentTaskRecordReconciliationReport> {
    let mut report = AgentTaskRecordReconciliationReport {
        schema: AGENT_TASK_RECORD_RECONCILIATION_SCHEMA.to_string(),
        dry_run,
        considered: 0,
        migrated: 0,
        quarantined: 0,
        records: Vec::new(),
    };
    for run in store::observation_runs()? {
        let Err(item) = diagnose_run(&run) else {
            continue;
        };
        // Quarantine is durable operator evidence, not a retry queue. Repeating
        // apply must be a no-op until an operator supplies new source evidence.
        if item.quarantined {
            continue;
        }
        report.considered += 1;
        let reconstructable = !run
            .metadata_json
            .pointer("/agent_task_run/lab_handoff")
            .is_some()
            && matches!(
                item.reason,
                AgentTaskRecordHealthReason::MissingMetadata
                    | AgentTaskRecordHealthReason::MalformedMetadata
            )
            && store::read_controller_plan(&run.id).is_ok();
        let action = if reconstructable || item.reason == AgentTaskRecordHealthReason::LegacySchema
        {
            "migrate"
        } else {
            "quarantine"
        };
        report.records.push(AgentTaskRecordReconciliationItem {
            run_id: run.id.clone(),
            reason: item.reason.clone(),
            action: if dry_run {
                format!("would-{action}")
            } else {
                action.to_string()
            },
        });
        if dry_run {
            continue;
        }
        if reconstructable {
            store::write_record(&reconstruct_record(&run)?)?;
            report.migrated += 1;
        } else if item.reason == AgentTaskRecordHealthReason::LegacySchema {
            let mut record = store::record_from_run(&run)?;
            let original = serde_json::to_value(&record).unwrap_or(Value::Null);
            record.schema = schemas::RUN.to_string();
            record.lifecycle.schema = RUN_LIFECYCLE_RECORD_SCHEMA.to_string();
            record.ensure_metadata_object().insert(
                "lifecycle_reconstruction".to_string(),
                json!({ "source": "legacy_typed_record", "original_record": original }),
            );
            store::write_record(&record)?;
            report.migrated += 1;
        } else {
            quarantine(&run, &item)?;
            report.quarantined += 1;
        }
    }
    Ok(report)
}

fn reconstruct_record(run: &RunRecord) -> Result<AgentTaskRunRecord> {
    let plan = store::read_controller_plan(&run.id)?;
    let state = match run.status.as_str() {
        "pass" => AgentTaskRunState::Succeeded,
        "fail" => AgentTaskRunState::Failed,
        "skipped" => AgentTaskRunState::Cancelled,
        _ => AgentTaskRunState::Running,
    };
    let timestamp = run
        .finished_at
        .clone()
        .or_else(|| Some(run.started_at.clone()));
    let mut lifecycle = RunLifecycleRecord::with_execution_state(RunExecutionState::from(state));
    lifecycle.updated_at = timestamp.clone();
    lifecycle.execution.updated_at = timestamp.clone();
    lifecycle.execution.started_at = Some(run.started_at.clone());
    if !matches!(
        state,
        AgentTaskRunState::Running | AgentTaskRunState::Queued
    ) {
        lifecycle.execution.finished_at = timestamp.clone();
    }
    Ok(AgentTaskRunRecord {
        schema: schemas::RUN.to_string(),
        run_id: run.id.clone(),
        plan_id: plan.plan_id,
        state,
        submitted_at: run.started_at.clone(),
        updated_at: timestamp,
        plan_path: store::run_dir(&run.id)?
            .join("plan.json")
            .display()
            .to_string(),
        aggregate_path: None,
        totals: None,
        tasks: plan.tasks.iter().map(queued_task).collect(),
        artifact_refs: Vec::new(),
        provider_handles: Vec::new(),
        latest_executor_evidence: None,
        lifecycle,
        lab_handoff: None,
        candidate_adoption: None,
        metadata: json!({
            "lifecycle_reconstruction": {
                "source": "observation_status_and_durable_plan",
                "original_metadata": run.metadata_json,
                "authoritative_terminal_status": run.status,
            }
        }),
    })
}

fn quarantine(run: &RunRecord, item: &AgentTaskRecordHealthItem) -> Result<()> {
    let mut metadata = run.metadata_json.clone();
    if !metadata.is_object() {
        metadata = json!({ "homeboy_original_metadata": metadata });
    }
    metadata.as_object_mut().expect("metadata object").insert(
        QUARANTINE_KEY.to_string(),
        json!({
            "schema": "homeboy/agent-task-lifecycle-quarantine/v1",
            "reason_code": item.reason,
            "remediation": item.remediation,
            "original_metadata": run.metadata_json,
        }),
    );
    ObservationStore::open_initialized()?.upsert_imported_run_preserving_terminal(&RunRecord {
        metadata_json: metadata,
        ..run.clone()
    })
}

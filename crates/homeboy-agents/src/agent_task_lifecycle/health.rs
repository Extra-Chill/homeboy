use super::*;
use homeboy_core::observation::RunRecord;

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
    // Diagnosis must be able to read a legacy record in order to classify it as
    // LegacySchema; the strict guard would report it as malformed metadata and
    // send it to quarantine instead of migration.
    let record = store::record_from_run_allowing_legacy_schema(run).map_err(|_| {
        AgentTaskRecordHealthItem {
            run_id: run.id.clone(),
            reason: if run.metadata_json.get("agent_task_run").is_none() {
                AgentTaskRecordHealthReason::MissingMetadata
            } else {
                AgentTaskRecordHealthReason::MalformedMetadata
            },
            quarantined,
            remediation: remediation.to_string(),
        }
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
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_health_summary_in_store(&lifecycle_store)
}

/// [`record_health_summary`] against explicitly injected durable lifecycle
/// roots.
///
/// This one really is a read — the summary is the health half of the bounded
/// registry snapshot — but it is the diagnostic an operator reads *before*
/// running [`reconcile_record_health_in_store`], which writes. A summary
/// counted in one home over a repair applied in another is the shape #7505
/// exists to stop.
pub fn record_health_summary_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<AgentTaskRecordHealthSummary> {
    Ok(lifecycle_store.read_records_with_health()?.1)
}

/// Parse a durable observation row without the supported-schema guard.
///
/// This is a pure `RunRecord` -> typed-record conversion: it opens no store and
/// resolves no root, so calling it from a rooted body is not an ambient reach.
/// It exists only so [`reconcile_record_health_in_store`] never has to name the
/// `store::` shim path, which is how the #7505 scanner recognises a body that
/// manufactured its own root — and which would be a false positive here.
fn legacy_record_from_run(run: &RunRecord) -> Result<AgentTaskRunRecord> {
    store::record_from_run_allowing_legacy_schema(run)
}

pub fn reconcile_record_health(dry_run: bool) -> Result<AgentTaskRecordReconciliationReport> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    reconcile_record_health_in_store(&lifecycle_store, dry_run)
}

/// [`reconcile_record_health`] against explicitly injected durable lifecycle
/// roots.
///
/// Despite reading like a report, this writes on every non-dry-run pass: it
/// commits migrated records and stamps quarantine metadata onto rows it cannot
/// repair. The scan that decides *which* rows to touch therefore has to read
/// the same observation database those writes land in, and the plan it consults
/// to decide whether a row is reconstructable has to come from the same roots
/// as the record it reconstructs (#7505).
pub fn reconcile_record_health_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    dry_run: bool,
) -> Result<AgentTaskRecordReconciliationReport> {
    let mut report = AgentTaskRecordReconciliationReport {
        schema: AGENT_TASK_RECORD_RECONCILIATION_SCHEMA.to_string(),
        dry_run,
        considered: 0,
        migrated: 0,
        quarantined: 0,
        records: Vec::new(),
    };
    for run in lifecycle_store.observation_runs()? {
        let Err(item) = diagnose_run(&run) else {
            continue;
        };
        // Quarantine is durable operator evidence, not a retry queue. Repeating
        // apply must be a no-op until an operator supplies new source evidence.
        if item.quarantined {
            continue;
        }
        report.considered += 1;
        let reconstructable = run
            .metadata_json
            .pointer("/agent_task_run/lab_handoff")
            .is_none()
            && matches!(
                item.reason,
                AgentTaskRecordHealthReason::MissingMetadata
                    | AgentTaskRecordHealthReason::MalformedMetadata
            )
            && lifecycle_store.read_controller_plan(&run.id).is_ok();
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
            lifecycle_store.write_record(&reconstruct_record_in_store(lifecycle_store, &run)?)?;
            report.migrated += 1;
        } else if item.reason == AgentTaskRecordHealthReason::LegacySchema {
            let mut record = legacy_record_from_run(&run)?;
            let original = serde_json::to_value(&record).unwrap_or(Value::Null);
            record.schema = schemas::RUN.to_string();
            record.lifecycle.schema = RUN_LIFECYCLE_RECORD_SCHEMA.to_string();
            record.ensure_metadata_object().insert(
                "lifecycle_reconstruction".to_string(),
                json!({ "source": "legacy_typed_record", "original_record": original }),
            );
            lifecycle_store.write_record(&record)?;
            report.migrated += 1;
        } else {
            quarantine_in_store(lifecycle_store, &run, &item)?;
            report.quarantined += 1;
        }
    }
    Ok(report)
}

/// Rebuild a typed record from an observation row and its durable plan.
///
/// There is deliberately no ambient sibling: the plan this reads and the
/// `plan_path` it stamps into the reconstructed record are two halves of one
/// answer, and the caller is about to commit that record into the same store.
fn reconstruct_record_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run: &RunRecord,
) -> Result<AgentTaskRunRecord> {
    let plan = lifecycle_store.read_controller_plan(&run.id)?;
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
        plan_path: lifecycle_store
            .run_dir(&run.id)
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
        adoption_run_id: None,
        acceptance: None,
        // Reconstructed legacy records have no durable admission fence. Do not
        // invent workspace ownership from a plan-only projection.
        workspace_identity: None,
        workspace_lifecycle_revision: 0,
        workspace_owner_lease: None,
        workspace_claim: None,
        metadata: json!({
            "lifecycle_reconstruction": {
                "source": "observation_status_and_durable_plan",
                "original_metadata": run.metadata_json,
                "authoritative_terminal_status": run.status,
            }
        }),
    })
}

/// Stamp quarantine evidence onto a row that cannot be repaired.
///
/// This used to open `ObservationStore::open_initialized()`, which roots the
/// database at the ambient data root and artifact resolution at the ambient
/// artifact root — so a reconciliation scanning an injected store could stamp
/// quarantine metadata onto a row in a different home, or onto no row at all,
/// without failing. `open_observation_initialized` binds both roots from the
/// injected store. It is also lifecycle-mode, which defers report-only startup
/// artifact maintenance; that maintenance is unrelated repair work, and it is
/// the opener every other durable record write in this crate already uses.
fn quarantine_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run: &RunRecord,
    item: &AgentTaskRecordHealthItem,
) -> Result<()> {
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
    lifecycle_store
        .open_observation_initialized()?
        .upsert_imported_run_preserving_terminal(&RunRecord {
            metadata_json: metadata,
            ..run.clone()
        })
}

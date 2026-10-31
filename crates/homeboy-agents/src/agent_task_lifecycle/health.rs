use super::lifecycle_store::decode_record_from_run;
use super::*;
use homeboy_core::observation::RunRecord;

pub(crate) const HEALTH_SAMPLE_LIMIT: usize = 20;
const QUARANTINE_KEY: &str = "agent_task_lifecycle_quarantine";
const FIXTURE_RUNNER_QUARANTINE_REASON: &str = "fixture_runner_provenance";

/// Fixture executor records cannot represent production runner ownership. The
/// conjunction keeps an unknown real runner fail-closed: it requires an accepted
/// runner handoff, a complete runner-job identity, and a plan made entirely of
/// the in-tree test executor.
pub(crate) fn fixture_runner_provenance(
    record: &AgentTaskRunRecord,
    plan: &AgentTaskPlan,
) -> Option<Value> {
    let runner_id = record.runner_id()?;
    let runner_job_id = record.runner_job_id()?;
    (record.has_accepted_lab_handoff()
        && !plan.tasks.is_empty()
        && plan
            .tasks
            .iter()
            .all(|task| task.executor.backend == "fixture"))
    .then(|| {
        json!({
            "schema": "homeboy/agent-task-fixture-runner-provenance/v1",
            "runner_id": runner_id,
            "runner_job_id": runner_job_id,
            "executor_backends": ["fixture"],
        })
    })
}

pub(crate) fn diagnose_run(
    run: &RunRecord,
) -> std::result::Result<AgentTaskRunRecord, AgentTaskRecordHealthItem> {
    let quarantined = run.metadata_json.get(QUARANTINE_KEY).is_some();
    // There is no in-place repair: a record this controller cannot load as the
    // current typed shape stays excluded from agent-task reads (#15742).
    let remediation = if quarantined {
        "inspect metadata.agent_task_lifecycle_quarantine; the record stays excluded from agent-task reads"
    } else {
        "the record is not a current typed agent-task run and stays excluded from agent-task reads; inspect it with `homeboy runs show <run-id>`"
    };
    let record = decode_record_from_run(run).map_err(|_| AgentTaskRecordHealthItem {
        run_id: run.id.clone(),
        reason: if run.metadata_json.get("agent_task_run").is_none() {
            AgentTaskRecordHealthReason::MissingMetadata
        } else {
            AgentTaskRecordHealthReason::MalformedMetadata
        },
        quarantined,
        remediation: remediation.to_string(),
    })?;
    let reason = if quarantined
        && run
            .metadata_json
            .pointer("/agent_task_lifecycle_quarantine/reason_code")
            .and_then(Value::as_str)
            == Some(FIXTURE_RUNNER_QUARANTINE_REASON)
    {
        Some(AgentTaskRecordHealthReason::FixtureRunnerProvenance)
    } else if record.schema != schemas::RUN
        || record.lifecycle.schema != RUN_LIFECYCLE_RECORD_SCHEMA
    {
        // Older record shapes are not migrated; they fail to load (#15742).
        Some(AgentTaskRecordHealthReason::MalformedMetadata)
    } else if RunExecutionState::from(record.state()) != record.lifecycle.execution.state {
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
        AgentTaskRecordHealthReason::ConflictingProjections => health.conflicting += 1,
        AgentTaskRecordHealthReason::FixtureRunnerProvenance => health.fixture += 1,
    }
    if item.quarantined {
        health.quarantined += 1;
    }
    if health.samples.len() < HEALTH_SAMPLE_LIMIT {
        health.samples.push(item);
    }
}

/// `record_health_summary` against explicitly injected durable lifecycle
/// roots.
///
/// The summary is the health half of the bounded registry snapshot, counted
/// from the same injected home the records are read from (#7505).
pub fn record_health_summary_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<AgentTaskRecordHealthSummary> {
    Ok(lifecycle_store.read_records_with_health_bounded(1000)?.1)
}

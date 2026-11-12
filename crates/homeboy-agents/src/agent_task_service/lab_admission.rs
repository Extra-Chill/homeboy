//! Shared ownership observation for queued attempts and the recovery planner.
use super::*;
use crate::agent_task_lifecycle;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct QueuedAdmission {
    pub live_driver: bool,
    pub liveness: AgentTaskLiveness,
    pub wait: Option<Value>,
}

/// Observe the driver lock once. A recent heartbeat alone never proves driver
/// ownership, but it gives an abandoned admission its normal liveness grace.
pub fn queued_admission(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<QueuedAdmission> {
    queued_admission_in_store(
        record,
        &agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().ok()?,
    )
}

pub(crate) fn queued_admission_in_store(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    store: &agent_task_lifecycle::AgentTaskLifecycleStore,
) -> Option<QueuedAdmission> {
    if record.state() != agent_task_lifecycle::AgentTaskRunState::Queued
        || record.runner_job_id().is_some()
    {
        return None;
    }
    let cook_id = record.metadata.get("cook_id").and_then(Value::as_str);
    let live_driver = cook_id.is_some_and(|id| {
        CookRecipeStore::for_lifecycle_store(store)
            .foreign_cook_driver(id)
            .is_ok_and(|driver| driver.is_some())
    });
    let liveness = if live_driver {
        AgentTaskLiveness::Active
    } else {
        liveness_for_record(record, chrono::Utc::now())
    };
    let start = record
        .metadata
        .pointer("/lab_admission_wait/started_at")
        .and_then(Value::as_str)
        .unwrap_or(&record.submitted_at);
    let deadline = chrono::DateTime::parse_from_rfc3339(start)
        .ok()
        .map(|start| {
            (start
                + chrono::Duration::from_std(homeboy_core::substrate_wait::SUBSTRATE_WAIT_DEADLINE)
                    .expect("substrate deadline fits chrono"))
            .to_rfc3339()
        });
    Some(QueuedAdmission {
        live_driver,
        liveness,
        wait: record.runner_id().map(|runner_id| {
            json!({
                "runner_id": runner_id, "started_at": start, "deadline": deadline,
                "blocker": record.metadata.pointer("/lab_admission_wait/blocker")
                    .or_else(|| record.metadata.get("pre_execution_failure"))
                    .or_else(|| record.metadata.get("runner_availability")),
            })
        }),
    })
}

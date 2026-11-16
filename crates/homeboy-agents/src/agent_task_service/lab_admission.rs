//! Shared ownership observation for queued attempts and the recovery planner.
use super::*;
use crate::agent_task_lifecycle;
use serde_json::{json, Value};

/// Single live-step reader for status, diagnosis, Cook blockers and resume.
pub fn execution_step_wait(record: &agent_task_lifecycle::AgentTaskRunRecord) -> Option<Value> {
    if record.state().is_terminal() {
        return None;
    }
    let key = record.metadata["execution_scheduling"]["current_intent_id"].as_str()?;
    let path = homeboy_core::paths::observation_db().ok()?;
    if !homeboy_core::api_jobs::JobStore::exists_at_path(&path) {
        return None;
    }
    let store = homeboy_core::api_jobs::JobStore::open_without_reconciliation(path).ok()?;
    let mut wait = store.step_runner_wait(key).ok()??;
    let runner = wait["runner_eligible"].as_str()?;
    let reason = wait["reason"].as_str()?;
    let action = match reason {
        "daemon_incompatible" | "daemon_not_fresh" => {
            format!("homeboy runner refresh-homeboy {runner} --reconnect")
        }
        "stale_jobs" => format!("homeboy runner reconcile {runner}"),
        "capacity_reached" => "wait for runner capacity to become available".to_string(),
        "pressure" => "wait for runner resource pressure to subside".to_string(),
        _ => format!("inspect runner {runner} eligibility"),
    };
    let context = wait["builds"].clone();
    let builds = if context["runner_build"].is_string() {
        format!(
            "; controller build {}, runner build {}",
            context["controller_build"], context["runner_build"]
        )
    } else {
        String::new()
    };
    let message = format!(
        "runner {runner}: {reason}; waiting since {}{builds}; next action: {action}",
        wait["waiting_since"].as_str().unwrap_or("unknown")
    );
    wait["builds"] = context;
    wait["next_action"] = json!(action);
    wait["message"] = json!(message);
    Some(wait)
}

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
    let step_wait = execution_step_wait(record);
    let liveness = if live_driver || step_wait.is_some() {
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
        wait: step_wait.or_else(|| {
            record.runner_id().map(|runner_id| {
                json!({
                    "runner_id": runner_id, "started_at": start, "deadline": deadline,
                    "blocker": record.metadata.pointer("/lab_admission_wait/blocker")
                        .or_else(|| record.metadata.get("pre_execution_failure"))
                        .or_else(|| record.metadata.get("runner_availability")),
                })
            })
        }),
    })
}

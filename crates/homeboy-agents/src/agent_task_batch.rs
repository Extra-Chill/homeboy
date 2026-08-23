use chrono::Utc;
use fs4::fs_std::FileExt;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::PathBuf;
use uuid::Uuid;

const COORDINATOR_LEASE_SECONDS: i64 = 300;

use crate::agent_task_dependency_graph::{
    dependency_graph_readiness, AgentTaskDependencyNode, AgentTaskDependencyState,
};
use crate::agent_task_lifecycle::{self, AgentTaskRunState};
use crate::agent_task_schedule::AgentTaskPlan;
use crate::agent_task_service;
use homeboy_core::{paths, Error, ErrorCode, Result};

mod types;

pub use types::*;

/// Child runs inherit the current run-scoped notification route at submission.
pub fn submit_plan_batch(
    plan: &AgentTaskPlan,
    requested_batch_id: Option<&str>,
) -> Result<AgentTaskBatchRecord> {
    validate_plan_batch(plan)?;
    AgentTaskBatchStore::from_current_data_root()?.submit_plan_batch_with(
        plan,
        requested_batch_id,
        agent_task_lifecycle::run_record_exists,
        |child_plan, run_id| agent_task_lifecycle::submit_plan(child_plan, Some(run_id)),
    )
}

fn validate_plan_batch(plan: &AgentTaskPlan) -> Result<()> {
    if plan.tasks.is_empty() {
        return Err(Error::validation_invalid_argument(
            "input",
            "agent-task batch requires at least one task",
            None,
            None,
        ));
    }
    if !plan.output_dependencies.is_empty() {
        return Err(Error::validation_invalid_argument(
            "input",
            "agent-task batch submit supports independent tasks; use fanout submit/run-plan for dependent workflow plans",
            Some(plan.plan_id.clone()),
            None,
        ));
    }
    Ok(())
}

fn submit_plan_batch_with_store<F, E>(
    store: &AgentTaskBatchStore,
    plan: &AgentTaskPlan,
    requested_batch_id: Option<&str>,
    mut run_record_exists: E,
    mut submit_child: F,
) -> Result<AgentTaskBatchRecord>
where
    F: FnMut(&AgentTaskPlan, &str) -> Result<crate::agent_task_lifecycle::AgentTaskRunRecord>,
    E: FnMut(&str) -> Result<bool>,
{
    validate_plan_batch(plan)?;

    let batch_id = requested_batch_id
        .map(sanitize_id)
        .unwrap_or_else(|| format!("agent-task-batch-{}", Uuid::new_v4()));
    let mut child_run_ids = HashSet::new();
    let child_run_ids = plan
        .tasks
        .iter()
        .map(|task| {
            let child_run_id = child_run_id(&batch_id, &task.task_id);
            if !child_run_ids.insert(child_run_id.clone()) {
                return Err(Error::validation_invalid_argument(
                    "task_id",
                    format!(
                        "agent-task batch child run id '{}' is duplicated after sanitizing task ids",
                        child_run_id
                    ),
                    Some(task.task_id.clone()),
                    None,
                ));
            }
            if run_record_exists(&child_run_id)? {
                return Err(Error::validation_invalid_argument(
                    "batch_id",
                    format!(
                        "agent-task batch child run id '{}' already exists; choose a different batch id",
                        child_run_id
                    ),
                    Some(batch_id.clone()),
                    None,
                ));
            }
            Ok(child_run_id)
        })
        .collect::<Result<Vec<_>>>()?;

    // Persist the batch boundary before creating children. A later submission
    // failure must still leave an inspectable batch identity for recovery.
    let mut record = AgentTaskBatchRecord {
        schema: AGENT_TASK_BATCH_SCHEMA.to_string(),
        batch_id,
        plan_id: plan.plan_id.clone(),
        state: AgentTaskBatchState::Queued,
        submitted_at: now_timestamp(),
        updated_at: None,
        task_count: plan.tasks.len(),
        child_runs: plan
            .tasks
            .iter()
            .zip(&child_run_ids)
            .map(|(task, run_id)| AgentTaskBatchChildRun {
                task_id: task.task_id.clone(),
                run_id: run_id.clone(),
                state: AgentTaskRunState::Queued,
            })
            .collect(),
        metadata: batch_metadata(plan),
    };
    store.write_batch(&record)?;

    for (index, task) in plan.tasks.iter().enumerate() {
        let child_run_id = child_run_ids[index].clone();
        let child_plan = child_plan(plan, task.clone(), &record.batch_id);
        let child_record = submit_child(&child_plan, &child_run_id)?;
        let child = &mut record.child_runs[index];
        child.run_id = child_record.run_id;
        child.state = child_record.state;
        record.updated_at = Some(now_timestamp());
        store.write_batch(&record)?;
    }

    Ok(record)
}

/// One child of a fanout run-plan batch: the durable run id the coordinator
/// dispatches and the task/cook id it was compiled from.
#[derive(Debug, Clone)]
pub struct FanoutRunBatchChild {
    pub task_id: String,
    pub run_id: String,
}

/// Persist the durable batch record for an `agent-task fanout run-plan`
/// invocation before child admission.
///
/// `fanout run-plan` executes cooks directly on the controller (unlike
/// `fanout submit`, which queues them), but it previously never wrote the
/// `agent-task-batches/<fanout_id>.json` record that `fanout status`/`artifacts`
/// read. A named, Lab-routed run-plan therefore admitted its children and then
/// failed `fanout status <id>` with `No such file or directory` (#9397).
///
/// Writing the record here, keyed by `fanout_id` with each child's durable run
/// id, lets `status` resolve every child live (including detached Lab runs and
/// retries) and survives controller exit / partial admission. Children start in
/// `Running` because run-plan dispatches immediately; `status` reconciles the
/// live per-child state on read.
pub fn persist_fanout_run_batch(
    fanout_id: &str,
    plan_id: &str,
    children: &[FanoutRunBatchChild],
    metadata: Value,
) -> Result<AgentTaskBatchRecord> {
    validate_fanout_run_batch(fanout_id, children)?;
    AgentTaskBatchStore::from_current_data_root()?
        .persist_fanout_run_batch(fanout_id, plan_id, children, metadata)
}

fn validate_fanout_run_batch(fanout_id: &str, children: &[FanoutRunBatchChild]) -> Result<()> {
    if children.is_empty() {
        return Err(Error::validation_invalid_argument(
            "cooks",
            "agent-task fanout run-plan requires at least one cook",
            Some(fanout_id.to_string()),
            None,
        ));
    }
    let mut seen = HashSet::new();
    for child in children {
        if !seen.insert(child.run_id.clone()) {
            return Err(Error::validation_invalid_argument(
                "cook_id",
                format!(
                    "agent-task fanout run-plan child run id '{}' is duplicated",
                    child.run_id
                ),
                Some(fanout_id.to_string()),
                None,
            ));
        }
    }
    Ok(())
}

pub fn persist_fanout_run_batch_in_store(
    store: &AgentTaskBatchStore,
    fanout_id: &str,
    plan_id: &str,
    children: &[FanoutRunBatchChild],
    metadata: Value,
) -> Result<AgentTaskBatchRecord> {
    validate_fanout_run_batch(fanout_id, children)?;
    let batch_id = sanitize_id(fanout_id);
    store.with_batch_lock(fanout_id, || {
        if let Ok(existing) = store.read_batch(fanout_id) {
            let expected_children = children
                .iter()
                .map(|child| (&child.task_id, &child.run_id))
                .collect::<Vec<_>>();
            let actual_children = existing
                .child_runs
                .iter()
                .map(|child| (&child.task_id, &child.run_id))
                .collect::<Vec<_>>();
            if existing.plan_id != plan_id || actual_children != expected_children {
                return Err(Error::validation_invalid_argument(
                    "fanout_id",
                    "agent-task fanout run-plan id already belongs to a different child roster",
                    Some(fanout_id.to_string()),
                    None,
                ));
            }
            return Ok(existing);
        }
        let record = AgentTaskBatchRecord {
            schema: AGENT_TASK_BATCH_SCHEMA.to_string(),
            batch_id,
            plan_id: plan_id.to_string(),
            state: AgentTaskBatchState::Planning,
            submitted_at: now_timestamp(),
            updated_at: None,
            task_count: children.len(),
            child_runs: children
                .iter()
                .map(|child| AgentTaskBatchChildRun {
                    task_id: child.task_id.clone(),
                    run_id: child.run_id.clone(),
                    state: AgentTaskRunState::Queued,
                })
                .collect(),
            metadata,
        };
        store.write_batch(&record)?;
        Ok(record)
    })
}

pub fn claim_fanout_run_batch(batch_id: &str) -> Result<Option<String>> {
    AgentTaskBatchStore::from_current_data_root()?.claim_fanout_run_batch(batch_id)
}

pub fn claim_fanout_run_batch_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
) -> Result<Option<String>> {
    store.with_batch_lock(batch_id, || {
        let mut batch = store.read_batch(batch_id)?;
        let abandoned = batch.state == AgentTaskBatchState::Admitting
            && batch.metadata["coordinator"]["heartbeat_at"]
                .as_str()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|heartbeat| {
                    Utc::now() - heartbeat.with_timezone(&Utc)
                        > chrono::Duration::seconds(COORDINATOR_LEASE_SECONDS)
                });
        if !matches!(
            batch.state,
            AgentTaskBatchState::Planning | AgentTaskBatchState::Failed
        ) && !abandoned
        {
            return Ok(None);
        }
        if !batch.metadata.is_object() {
            batch.metadata = Value::Object(serde_json::Map::new());
        }
        let metadata = batch.metadata.as_object_mut().expect("metadata object");
        metadata.remove("terminal_failure");
        let claim_id = Uuid::new_v4().to_string();
        let admission_deadline_at = (Utc::now() + chrono::Duration::seconds(COORDINATOR_LEASE_SECONDS))
            .to_rfc3339();
        metadata.insert(
            "coordinator".to_string(),
            json!({ "claim_id": claim_id, "stage": "admitting", "heartbeat_at": now_timestamp(), "admission_deadline_at": admission_deadline_at, "lease_seconds": COORDINATOR_LEASE_SECONDS }),
        );
        batch.state = AgentTaskBatchState::Admitting;
        batch.updated_at = Some(now_timestamp());
        store.write_batch(&batch)?;
        Ok(Some(claim_id))
    })
}

pub fn heartbeat_fanout_run_batch(batch_id: &str, claim_id: &str) -> Result<()> {
    AgentTaskBatchStore::from_current_data_root()?.heartbeat_fanout_run_batch(batch_id, claim_id)
}

pub fn heartbeat_fanout_run_batch_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    claim_id: &str,
) -> Result<()> {
    store.mutate_batch(batch_id, |batch| {
        let coordinator = batch
            .metadata
            .get_mut("coordinator")
            .and_then(Value::as_object_mut)
            .filter(|coordinator| {
                coordinator.get("claim_id").and_then(Value::as_str) == Some(claim_id)
            })
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "claim_id",
                    "fanout coordinator claim is stale",
                    Some(batch_id.to_string()),
                    None,
                )
            })?;
        coordinator.insert("heartbeat_at".to_string(), json!(now_timestamp()));
        // An admitting coordinator can spend longer than one lease preparing
        // gates, worktrees, and recipes. A live heartbeat renews that admission
        // lease; a dead coordinator still expires when heartbeats stop.
        coordinator.insert(
            "admission_deadline_at".to_string(),
            json!((Utc::now() + chrono::Duration::seconds(COORDINATOR_LEASE_SECONDS)).to_rfc3339()),
        );
        batch.updated_at = Some(now_timestamp());
        Ok(())
    })
}

/// Persist a terminal controller failure after durable batch planning.
pub fn record_fanout_run_batch_failure(
    batch_id: &str,
    claim_id: &str,
    stage: &str,
    failure: Value,
) -> Result<()> {
    AgentTaskBatchStore::from_current_data_root()?
        .record_fanout_run_batch_failure(batch_id, claim_id, stage, failure)
}

pub fn record_fanout_run_batch_failure_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    claim_id: &str,
    stage: &str,
    failure: Value,
) -> Result<()> {
    store.with_batch_lock(batch_id, || {
        let mut batch = store.read_batch(batch_id)?;
        if batch.metadata["coordinator"]["claim_id"].as_str() != Some(claim_id) {
            return Err(Error::validation_invalid_argument(
                "claim_id",
                "fanout coordinator claim is stale",
                Some(batch_id.to_string()),
                None,
            ));
        }
        if !batch.metadata.is_object() {
            batch.metadata = Value::Object(serde_json::Map::new());
        }
        batch
            .metadata
            .as_object_mut()
            .expect("metadata object")
            .insert(
                "terminal_failure".to_string(),
                json!({ "stage": stage, "failure": failure }),
            );
        for child in &mut batch.child_runs {
            child.state = AgentTaskRunState::Failed;
        }
        batch.state = AgentTaskBatchState::Failed;
        batch.updated_at = Some(now_timestamp());
        store.write_batch(&batch)
    })
}

/// Record child failures that occurred before Cook could create a lifecycle
/// record. These are terminal controller-admission failures, not unavailable
/// runner observations, so status must retain them as failures.
pub fn record_fanout_run_batch_failed_admissions<'a>(
    batch_id: &str,
    failed_run_ids: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    AgentTaskBatchStore::from_current_data_root()?
        .record_fanout_run_batch_failed_admissions(batch_id, failed_run_ids)
}

pub fn record_fanout_run_batch_failed_admissions_in_store<'a>(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    failed_run_ids: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    store.mutate_batch(batch_id, |batch| {
        let failed_run_ids = failed_run_ids.into_iter().collect::<HashSet<_>>();
        if failed_run_ids.is_empty() {
            return Ok(());
        }
        let mut changed = false;
        for child in &mut batch.child_runs {
            if failed_run_ids.contains(child.run_id.as_str())
                && child.state != AgentTaskRunState::Failed
            {
                child.state = AgentTaskRunState::Failed;
                changed = true;
            }
        }
        if changed {
            if !batch.metadata.is_object() {
                batch.metadata = Value::Object(serde_json::Map::new());
            }
            let metadata = batch.metadata.as_object_mut().expect("metadata object");
            let failures = metadata
                .entry("terminal_admission_failures")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .expect("admission failures are an array");
            for run_id in &failed_run_ids {
                if !failures.iter().any(|value| value.as_str() == Some(*run_id)) {
                    failures.push(Value::String((*run_id).to_string()));
                }
            }
            let totals = totals_for_children(&batch.child_runs);
            batch.state = aggregate_state(&totals);
            batch.updated_at = Some(now_timestamp());
        }
        Ok(())
    })
}

pub fn status(batch_id: &str) -> Result<AgentTaskBatchStatusReport> {
    let store = AgentTaskBatchStore::from_current_data_root()?;
    let _ = store.expire_stalled_fanout_admission(batch_id)?;
    store.status(batch_id)
}

/// Expire a live coordinator only when its durable record proves it never
/// advanced beyond pre-child admission. Detached supervision uses this to stop
/// the stranded process as soon as the durable terminal blocker is written.
pub fn expire_stalled_fanout_admission(batch_id: &str) -> Result<bool> {
    AgentTaskBatchStore::from_current_data_root()?.expire_stalled_fanout_admission(batch_id)
}

/// Turn an accepted coordinator that never leaves admission into durable,
/// actionable state. The admission deadline is independent of the heartbeat:
/// a live process that is stuck before it can create a child must not renew its
/// way into an indefinite `admitting` state.
///
/// The "did a child ever start" probe follows the injected lifecycle root for
/// the same reason the batch roster follows the injected batch root: this is one
/// decision about one wave. Read ambiently, another home's copy of the same run
/// id could report a started child and hold a genuinely stranded coordinator in
/// `admitting` forever, or report no child and terminalize a wave that is
/// running somewhere else (#7505, #12619).
pub fn expire_stalled_fanout_admission_in_store(
    store: &AgentTaskBatchStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    batch_id: &str,
) -> Result<bool> {
    store.with_batch_lock(batch_id, || {
        let mut batch = store.read_batch(batch_id)?;
        let coordinator = &batch.metadata["coordinator"];
        let heartbeat_stale = coordinator["heartbeat_at"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|heartbeat| {
                Utc::now() - heartbeat.with_timezone(&Utc)
                    > chrono::Duration::seconds(COORDINATOR_LEASE_SECONDS)
            });
        let child_started = batch.child_runs.iter().any(|child| {
            child.state != AgentTaskRunState::Queued
                || batch.metadata["child_finalizations"]
                    .get(&child.run_id)
                    .is_some()
                // This used to call agent_task_lifecycle::run_record_exists(&child.run_id),
                // which probed whatever home the environment pointed at while
                // the batch roster came from the injected one (#7505, #12619).
                || agent_task_lifecycle::run_record_exists_in_store(lifecycle_store, &child.run_id)
                    .unwrap_or(true)
        });
        let expired = batch.state == AgentTaskBatchState::Admitting
            && coordinator["stage"].as_str() == Some("admitting")
            && heartbeat_stale
            && !child_started
            && coordinator["admission_deadline_at"]
                .as_str()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|deadline| Utc::now() >= deadline.with_timezone(&Utc));
        if !expired {
            return Ok(false);
        }
        let stage = coordinator["stage"]
            .as_str()
            .unwrap_or("admitting")
            .to_string();
        let command = stalled_admission_recovery_command(&batch)?;
        if !batch.metadata.is_object() {
            batch.metadata = Value::Object(serde_json::Map::new());
        }
        batch.metadata.as_object_mut().expect("metadata object").insert(
            "terminal_failure".to_string(),
            json!({
                "stage": stage,
                "failure": {
                    "code": "coordinator_admission_timeout",
                    "message": format!("fanout coordinator did not advance beyond '{stage}' before its admission deadline"),
                    "next_action": command,
                }
            }),
        );
        for child in &mut batch.child_runs {
            child.state = AgentTaskRunState::Failed;
        }
        batch.state = AgentTaskBatchState::Failed;
        batch.updated_at = Some(now_timestamp());
        store.write_batch(&batch)?;
        Ok(true)
    })
}

fn stalled_admission_recovery_command(batch: &AgentTaskBatchRecord) -> Result<String> {
    stalled_admission_recovery_command_with(batch, |child| {
        agent_task_service::recipe_exists(&child.task_id)
    })
}

fn stalled_admission_recovery_command_with(
    batch: &AgentTaskBatchRecord,
    mut recipe_exists: impl FnMut(&AgentTaskBatchChildRun) -> Result<bool>,
) -> Result<String> {
    let recipes_exist = batch
        .child_runs
        .iter()
        .map(&mut recipe_exists)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .all(|exists| exists);
    if recipes_exist {
        return Ok(format!(
            "homeboy agent-task fanout resume {}",
            batch.batch_id
        ));
    }
    batch.metadata["replan_command"]
        .as_str()
        .filter(|command| !command.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "replan_command",
                "stalled fanout admission has no persisted child recipes or replan command",
                Some(batch.batch_id.clone()),
                None,
            )
        })
}

fn status_in_store<S, P>(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    mut child_status: S,
    mut projection_readiness: P,
) -> Result<AgentTaskBatchStatusReport>
where
    S: FnMut(&str) -> Result<agent_task_lifecycle::AgentTaskRunRecord>,
    P: FnMut(&str) -> Result<Option<String>>,
{
    let mut batch = store.read_batch(batch_id)?;
    if batch.metadata["terminal_failure"].is_object() {
        batch.state = AgentTaskBatchState::Failed;
        let commands = commands(&batch.batch_id);
        let admission_blocker = batch.metadata["terminal_failure"].clone();
        let expected = batch.child_runs.len();
        let admitted = batch
            .child_runs
            .iter()
            .filter(|child| {
                agent_task_lifecycle::run_record_exists_readonly(&child.run_id).unwrap_or(false)
            })
            .count();
        let mut next_actions = vec![commands.status.clone(), commands.artifacts.clone()];
        if let Some(command) = admission_blocker
            .pointer("/failure/next_action")
            .and_then(Value::as_str)
        {
            next_actions.insert(0, command.to_string());
        }
        return Ok(AgentTaskBatchStatusReport {
            schema: AGENT_TASK_BATCH_STATUS_SCHEMA,
            status: "failed".to_string(),
            observation_fresh: true,
            totals: totals_for_children(&batch.child_runs),
            admission: AgentTaskBatchAdmission {
                expected,
                admitted,
                rejected: expected.saturating_sub(admitted),
                absent: 0,
            },
            batch,
            unavailable_child_runs: Vec::new(),
            admission_blocker: Some(admission_blocker),
            projection_pending_child_runs: Vec::new(),
            resumable_child_runs: Vec::new(),
            resumable: false,
            dependency_graph: None,
            next_actions,
            commands,
        });
    }
    let mut unavailable_child_runs = Vec::new();
    let mut projection_pending_child_runs = Vec::new();
    let mut resumable_child_runs = Vec::new();
    let mut timed_out_child_runs = HashSet::new();
    let mut observation_fresh = true;
    let mut admitted = 0;
    for child in &mut batch.child_runs {
        if terminal_preflight_or_admission_failure(&batch.metadata, &child.run_id) {
            continue;
        }
        match child_status(&child.run_id) {
            Ok(record) => {
                admitted += 1;
                if child.state != record.state {
                    child.state = record.state;
                }
                if record
                    .totals
                    .as_ref()
                    .is_some_and(|totals| totals.timed_out > 0)
                {
                    timed_out_child_runs.insert(child.run_id.clone());
                }
                if let Some(pending) =
                    projection_pending_child(child, &record, projection_readiness(&record.run_id)?)
                {
                    projection_pending_child_runs.push(pending);
                } else if let Some(reason) = resumable_child_reason(&record) {
                    resumable_child_runs.push(AgentTaskBatchResumableChild {
                        task_id: child.task_id.clone(),
                        run_id: child.run_id.clone(),
                        state: record.state,
                        reason,
                    });
                }
            }
            Err(error) => {
                if error.code == ErrorCode::ObservationStoreBusy {
                    observation_fresh = false;
                }
                if !child.state.is_terminal() {
                    unavailable_child_runs.push(child_issue(
                        child,
                        format!("unable to read child run status: {}", error.message),
                    ));
                }
            }
        }
    }
    let mut totals = totals_for_children(&batch.child_runs);
    for child in &batch.child_runs {
        if timed_out_child_runs.contains(&child.run_id) && child.state == AgentTaskRunState::Failed
        {
            totals.failed = totals.failed.saturating_sub(1);
            totals.timed_out += 1;
        }
    }
    totals.unavailable = unavailable_child_runs.len();
    let mut state = aggregate_state(&totals);
    if batch.state != state {
        batch.state = state;
    }
    let dependency_graph =
        refresh_dependency_graph_with_finalization_statuses(&mut batch, None, &mut child_status)?;
    if let Some(graph) = &dependency_graph {
        state = aggregate_state_after_graph_refresh(&totals, graph, state);
        if batch.state != state {
            batch.state = state;
        }
    }
    // A status read only persists a changed durable projection. In particular,
    // repeated observations must retain the existing timestamp byte-for-byte.
    // Status is a read-only projection. It never writes a snapshot assembled
    // outside the mutation lock, so it cannot overwrite a newer coordinator
    // or child-finalization transition.
    let mut next_actions = batch_next_actions(
        &unavailable_child_runs,
        &projection_pending_child_runs,
        &resumable_child_runs,
        &commands(&batch.batch_id),
    );
    if let Some(graph) = &dependency_graph {
        if let Some(action) = graph["readiness"]["next_action"].as_str() {
            next_actions.insert(0, action.to_string());
        }
    }
    next_actions.truncate(8);
    let resumable = !resumable_child_runs.is_empty();
    let commands = commands(&batch.batch_id);
    let expected = batch.child_runs.len();
    Ok(AgentTaskBatchStatusReport {
        schema: AGENT_TASK_BATCH_STATUS_SCHEMA,
        status: state.outcome_status().to_string(),
        observation_fresh,
        dependency_graph,
        next_actions,
        commands,
        batch,
        totals,
        admission: AgentTaskBatchAdmission {
            expected,
            admitted,
            rejected: 0,
            absent: expected.saturating_sub(admitted),
        },
        unavailable_child_runs,
        admission_blocker: None,
        projection_pending_child_runs,
        resumable_child_runs,
        resumable,
    })
}

/// Return the same dependency projection used by resume without persisting a
/// read-side PR observation into either the lifecycle or batch record.
pub fn fanout_dependency_graph_with_finalization_statuses(
    batch_id: &str,
    statuses: &BTreeMap<String, String>,
) -> Result<Option<Value>> {
    AgentTaskBatchStore::from_current_data_root()?
        .fanout_dependency_graph_with_finalization_statuses(batch_id, statuses)
}

pub fn fanout_dependency_graph_with_finalization_statuses_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    statuses: &BTreeMap<String, String>,
) -> Result<Option<Value>> {
    let mut batch = store.read_batch(batch_id)?;
    refresh_dependency_graph_with_finalization_statuses(
        &mut batch,
        Some(statuses),
        &mut agent_task_lifecycle::persisted_status,
    )
}

fn aggregate_state_after_graph_refresh(
    totals: &AgentTaskBatchTotals,
    graph: &Value,
    state: AgentTaskBatchState,
) -> AgentTaskBatchState {
    if state != AgentTaskBatchState::Succeeded {
        return state;
    }
    let pending = graph["readiness"]["states"]
        .as_object()
        .is_some_and(|states| {
            states.values().any(|state| {
                !matches!(
                    state.as_str(),
                    Some("succeeded" | "rejected" | "failed" | "cancelled")
                )
            })
        });
    if pending {
        // Child lifecycle success only means Cook completed. A queued dependent,
        // gate invalidation, or pending PR acceptance keeps the fanout active.
        if totals.queued > 0 {
            AgentTaskBatchState::Queued
        } else {
            AgentTaskBatchState::Running
        }
    } else {
        state
    }
}

/// Apply a read-only fanout graph projection to an already reconciled batch
/// aggregate. Status uses this after observing live PR state without mutation.
pub fn fanout_aggregate_state(totals: &AgentTaskBatchTotals, graph: &Value) -> AgentTaskBatchState {
    aggregate_state_after_graph_refresh(totals, graph, aggregate_state(totals))
}

/// Reconcile persisted fanout graph state from durable child observations. A
/// candidate that passed gates but is still awaiting human merge remains an
/// explicit blocker; only a recorded merge releases its dependents.
fn refresh_dependency_graph_with_finalization_statuses<S>(
    batch: &mut AgentTaskBatchRecord,
    finalization_statuses: Option<&BTreeMap<String, String>>,
    child_status: &mut S,
) -> Result<Option<Value>>
where
    S: FnMut(&str) -> Result<agent_task_lifecycle::AgentTaskRunRecord>,
{
    let Some(graph) = batch.metadata.get("dependency_graph").cloned() else {
        return Ok(None);
    };
    let Some(nodes_value) = graph.get("nodes").cloned() else {
        return Ok(None);
    };
    let nodes: Vec<AgentTaskDependencyNode> = serde_json::from_value(nodes_value)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let mut states = BTreeMap::new();
    for child in &batch.child_runs {
        let finalization_status = if let Some(status) =
            finalization_statuses.and_then(|statuses| statuses.get(&child.task_id).cloned())
        {
            Some(status)
        } else {
            match child_status(&child.run_id) {
                Ok(record) => record
                    .metadata
                    .get("cook_finalization")
                    .and_then(|value| value.get("status"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                // A status projection has already retained the batch's last
                // durable child state; a transient read lock must not turn a
                // graph refresh into a user-visible status failure.
                Err(error) if error.code == ErrorCode::ObservationStoreBusy => None,
                Err(error) => return Err(error),
            }
        };
        let has_finalization = finalization_status.is_some();
        let mut state = match child.state {
            AgentTaskRunState::Failed => AgentTaskDependencyState::Failed,
            AgentTaskRunState::Cancelled => AgentTaskDependencyState::Cancelled,
            AgentTaskRunState::CandidateRecoverable | AgentTaskRunState::PartialRecoverable => {
                AgentTaskDependencyState::BlockedByGate
            }
            AgentTaskRunState::Succeeded => match finalization_status.as_deref() {
                // A review-ready candidate is a valid stack base. Its exact head
                // is bound by the action receipt before the dependent is resumed.
                Some("merged" | "review_ready" | "draft_published") => {
                    AgentTaskDependencyState::Succeeded
                }
                Some("rejected") => AgentTaskDependencyState::Rejected,
                _ => AgentTaskDependencyState::AwaitingAcceptance,
            },
            _ => AgentTaskDependencyState::Queued,
        };
        // A merged upstream is not enough to release a terminal dependent: its
        // rebased head must finish every durable Git/PR and invalidation step.
        // Once invalidated, remove the old review terminal state from this
        // projection so `resume` re-enters Cook's gate/review lifecycle.
        for receipt in batch.metadata["dependency_action_receipts"]
            .as_object()
            .into_iter()
            .flat_map(|receipts| receipts.values())
        {
            if receipt["action"]["downstream_id"].as_str() != Some(child.task_id.as_str()) {
                continue;
            }
            match receipt["status"].as_str() {
                // A completed invalidation only holds the child at the Cook
                // frontier while it has no replacement finalization. Once its
                // gates/review complete again, the new review state (including
                // a merge) is authoritative.
                Some("completed")
                    if receipt["gates_invalidated"] == Value::Bool(true) && !has_finalization =>
                {
                    state = AgentTaskDependencyState::Queued;
                }
                Some("blocked" | "running" | "pending") | None => {
                    state = AgentTaskDependencyState::BlockedByDependency;
                }
                _ => {}
            }
        }
        states.insert(child.task_id.clone(), state);
    }
    let (edges, readiness) = dependency_graph_readiness(&nodes, &states)?;
    let graph = json!({
        "schema": "homeboy/agent-task-fanout-dependency-graph/v1",
        "nodes": nodes,
        "edges": edges,
        "readiness": readiness,
    });
    if batch.metadata["dependency_graph"] != graph {
        batch.metadata["dependency_graph"] = graph.clone();
    }
    Ok(Some(graph))
}

/// Read the graph-projected executable frontier. Resume callers use this to
/// avoid finalizing a dependent before its upstream candidate is accepted.
pub fn fanout_ready_child_run_ids(batch_id: &str) -> Result<Option<HashSet<String>>> {
    AgentTaskBatchStore::from_current_data_root()?.fanout_ready_child_run_ids(batch_id)
}

pub fn fanout_ready_child_run_ids_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
) -> Result<Option<HashSet<String>>> {
    let report = store.status(batch_id)?;
    let Some(graph) = report.dependency_graph else {
        return Ok(None);
    };
    let ready = graph["readiness"]["ready"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<HashSet<_>>();
    Ok(Some(
        report
            .batch
            .child_runs
            .into_iter()
            .filter(|child| ready.contains(child.task_id.as_str()))
            .map(|child| child.run_id)
            .collect(),
    ))
}

/// Resolve the durable child runs owned by a fanout. The mutable batch roster is
/// only an index: each listed run must independently prove its persisted plan
/// was created for this batch before it can be dispatched.
pub fn owned_child_run_ids(batch_id: &str) -> Result<HashSet<String>> {
    AgentTaskBatchStore::from_current_data_root()?.owned_child_run_ids(batch_id)
}

pub fn owned_child_run_ids_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
) -> Result<HashSet<String>> {
    owned_child_run_ids_for(
        store.read_batch(batch_id)?,
        agent_task_lifecycle::load_controller_plan,
    )
}

fn owned_child_run_ids_for(
    batch: AgentTaskBatchRecord,
    mut load_controller_plan: impl FnMut(&str) -> Result<AgentTaskPlan>,
) -> Result<HashSet<String>> {
    let mut owned = HashSet::new();
    for child in batch.child_runs {
        let plan = load_controller_plan(&child.run_id)?;
        let plan_batch_id = plan.metadata.get("batch_id").and_then(Value::as_str);
        if plan_batch_id != Some(batch.batch_id.as_str()) {
            return Err(Error::validation_invalid_argument(
                "fanout",
                "fanout child roster entry does not match the durable child plan batch lineage",
                Some(child.run_id),
                None,
            ));
        }
        owned.insert(child.run_id);
    }
    Ok(owned)
}

/// A child is resumable when its provider attempt reached a terminal, recoverable
/// state (it produced a candidate patch) but the cook never recorded a
/// finalization — i.e. promotion/gates/PR were owned by a coordinator that
/// exited. Already-finalized or still-running children are not resumable (#9525).
fn resumable_child_reason(record: &agent_task_lifecycle::AgentTaskRunRecord) -> Option<String> {
    let finalized = record.metadata.get("cook_finalization").is_some();
    if finalized {
        return None;
    }
    match record.state {
        AgentTaskRunState::Succeeded
        | AgentTaskRunState::CandidateRecoverable
        | AgentTaskRunState::PartialRecoverable => Some(format!(
            "child run is terminal ({:?}) with a candidate but no recorded PR finalization; resume to run gates and finalize",
            record.state
        )),
        _ => None,
    }
}

fn terminal_preflight_or_admission_failure(metadata: &Value, run_id: &str) -> bool {
    metadata["terminal_failure"].is_object()
        || metadata["terminal_admission_failures"]
            .as_array()
            .is_some_and(|failures| failures.iter().any(|value| value.as_str() == Some(run_id)))
}

pub fn artifacts(batch_id: &str) -> Result<AgentTaskBatchArtifactsReport> {
    AgentTaskBatchStore::from_current_data_root()?.artifacts(batch_id)
}

/// Assemble the fanout artifacts report from explicitly injected roots.
///
/// Every child-side read here follows `lifecycle_store`, and all three of them
/// have to: the status projection reads each child's durable record, the
/// projection-readiness probe reads that child's record and aggregate, and the
/// per-child artifacts read walks the same aggregate. Split across two homes,
/// this report cannot fail while being wrong — it renders a batch roster from
/// one home and child artifacts from another, and every count in it reads back
/// self-consistent (#7505, #12619).
fn artifacts_in_store(
    store: &AgentTaskBatchStore,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    batch_id: &str,
) -> Result<AgentTaskBatchArtifactsReport> {
    // This used to call store.status(batch_id), whose child-status and
    // projection-readiness probes are the ambient `persisted_status` and
    // `terminal_artifact_projection_readiness_bounded`. Naming their rooted
    // siblings here is what keeps the whole report in one home.
    let report = status_in_store(
        store,
        batch_id,
        |run_id| agent_task_lifecycle::persisted_status_in_store(lifecycle_store, run_id),
        |run_id| {
            agent_task_lifecycle::terminal_artifact_projection_readiness_bounded_in_store(
                lifecycle_store,
                run_id,
            )
        },
    )?;
    let mut unavailable_child_runs = report.unavailable_child_runs.clone();
    let child_runs = report
        .batch
        .child_runs
        .into_iter()
        .filter_map(
            // This used to call agent_task_lifecycle::artifacts(&child.run_id),
            // which read each child's aggregate out of whatever home the
            // environment pointed at (#7505, #12619).
            |child| match agent_task_lifecycle::artifacts_in_store(lifecycle_store, &child.run_id) {
                Ok(artifacts) => {
                    let artifact_count = artifacts.artifacts.len();
                    let evidence_ref_count = artifacts.evidence_refs.len();
                    Some(Ok(AgentTaskBatchChildArtifacts {
                        task_id: child.task_id,
                        run_id: child.run_id,
                        state: child.state,
                        artifact_count,
                        evidence_ref_count,
                        artifacts,
                    }))
                }
                Err(error) => {
                    if !unavailable_child_runs
                        .iter()
                        .any(|issue| issue.run_id == child.run_id)
                    {
                        unavailable_child_runs.push(child_issue(
                            &child,
                            format!("unable to read child run artifacts: {}", error.message),
                        ));
                    }
                    None
                }
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let manifest = artifacts_manifest(&child_runs);
    let summary = AgentTaskBatchArtifactsSummary {
        child_runs: child_runs.len(),
        artifacts: manifest.artifacts.len(),
        evidence_refs: manifest.evidence_refs.len(),
    };

    Ok(AgentTaskBatchArtifactsReport {
        schema: AGENT_TASK_BATCH_ARTIFACTS_SCHEMA,
        batch_id: report.batch.batch_id,
        summary,
        manifest,
        next_actions: batch_next_actions(
            &unavailable_child_runs,
            &report.projection_pending_child_runs,
            &report.resumable_child_runs,
            &report.commands,
        ),
        unavailable_child_runs,
        child_runs,
    })
}

fn artifacts_manifest(
    children: &[AgentTaskBatchChildArtifacts],
) -> AgentTaskBatchArtifactsManifest {
    let mut manifest = AgentTaskBatchArtifactsManifest::default();
    for child in children {
        for artifact in &child.artifacts.artifacts {
            manifest.artifacts.push(AgentTaskBatchArtifactEntry {
                task_id: child.task_id.clone(),
                run_id: child.run_id.clone(),
                state: child.state,
                artifact: artifact.clone(),
            });
        }
        for evidence_ref in &child.artifacts.evidence_refs {
            manifest.evidence_refs.push(AgentTaskBatchEvidenceRefEntry {
                task_id: child.task_id.clone(),
                run_id: child.run_id.clone(),
                state: child.state,
                evidence_ref: evidence_ref.clone(),
            });
        }
    }
    manifest
}

fn child_plan(
    source: &AgentTaskPlan,
    mut task: crate::agent_task::AgentTaskRequest,
    batch_id: &str,
) -> AgentTaskPlan {
    let task_id = task.task_id.clone();
    task.parent_plan_id
        .get_or_insert_with(|| batch_id.to_string());
    let mut metadata = match task.metadata {
        Value::Object(object) => object,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut object = serde_json::Map::new();
            object.insert("base".to_string(), other);
            object
        }
    };
    metadata.insert("batch_id".to_string(), json!(batch_id));
    task.metadata = Value::Object(metadata);

    let mut child = AgentTaskPlan::new(format!("{}/{}", source.plan_id, task.task_id), vec![task]);
    child.group_key = source
        .group_key
        .clone()
        .or_else(|| Some(batch_id.to_string()));
    child.component_contracts = source.component_contracts.clone();
    if let Some(outputs) = source.artifact_outputs.get(&task_id) {
        child.artifact_outputs.insert(task_id, outputs.clone());
    }
    child.options = source.options.clone();
    child.options.max_concurrency = 1;
    child.metadata = json!({
        "batch_id": batch_id,
        "parent_plan_id": source.plan_id,
    });
    child.rebuild_homeboy_plan();
    child
}

fn batch_metadata(plan: &AgentTaskPlan) -> Value {
    json!({
        "parent_plan_id": plan.plan_id,
        "group_key": plan.group_key,
        "durable_child_runs": true,
    })
}

fn totals_for_children(children: &[AgentTaskBatchChildRun]) -> AgentTaskBatchTotals {
    let mut totals = AgentTaskBatchTotals::default();
    for child in children {
        match child.state {
            AgentTaskRunState::Queued => totals.queued += 1,
            AgentTaskRunState::Running => totals.running += 1,
            AgentTaskRunState::Succeeded => totals.succeeded += 1,
            AgentTaskRunState::CandidateRecoverable => totals.partial_failure += 1,
            AgentTaskRunState::PartialRecoverable => totals.partial_failure += 1,
            AgentTaskRunState::PartialFailure => totals.partial_failure += 1,
            AgentTaskRunState::Failed => totals.failed += 1,
            AgentTaskRunState::Cancelled => totals.cancelled += 1,
        }
    }
    totals
}

fn child_issue(child: &AgentTaskBatchChildRun, problem: String) -> AgentTaskBatchChildIssue {
    AgentTaskBatchChildIssue {
        task_id: child.task_id.clone(),
        run_id: child.run_id.clone(),
        last_known_state: Some(child.state),
        status_command: format!("homeboy agent-task status {}", child.run_id),
        artifacts_command: format!("homeboy agent-task artifacts {}", child.run_id),
        problem,
    }
}

fn projection_pending_child(
    child: &AgentTaskBatchChildRun,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    readiness: Option<String>,
) -> Option<AgentTaskBatchProjectionPendingChild> {
    let Some(reason) = readiness else {
        return None;
    };
    Some(AgentTaskBatchProjectionPendingChild {
        task_id: child.task_id.clone(),
        run_id: child.run_id.clone(),
        state: record.state,
        phase: "artifact_projection".to_string(),
        reason,
        repair_command: format!("homeboy agent-task status {}", child.run_id),
    })
}

fn batch_next_actions(
    unavailable_child_runs: &[AgentTaskBatchChildIssue],
    projection_pending_child_runs: &[AgentTaskBatchProjectionPendingChild],
    resumable_child_runs: &[AgentTaskBatchResumableChild],
    commands: &AgentTaskBatchCommands,
) -> Vec<String> {
    let mut actions = Vec::new();
    if !resumable_child_runs.is_empty() {
        actions.push(format!(
            "{} child run(s) finished their provider attempt but were never finalized (coordinator likely exited); harvest them idempotently through gates and PR finalization with `{}`",
            resumable_child_runs.len(),
            commands.resume
        ));
        actions.push(
            "resume is idempotent: already-finalized children are skipped, so repeated resume calls will not duplicate patches, commits, pushes, or PRs".to_string(),
        );
    }
    if !unavailable_child_runs.is_empty() {
        actions.push(
            "partial results only: one or more child runs could not be read from the durable run store".to_string(),
        );
        actions.push(
            "inspect unavailable_child_runs for child run ids, last known states, status commands, artifacts commands, and error details".to_string(),
        );
        actions.push(
            "if a Lab runner daemon restarted, reconcile runner-side jobs/artifacts before treating the fanout as terminal".to_string(),
        );
    }
    if !projection_pending_child_runs.is_empty() {
        actions.push(
            "resume is withheld until controller-side patch projection completes; inspect projection_pending_child_runs and run each repair_command to retry hydration".to_string(),
        );
    }
    actions
}

fn commands(batch_id: &str) -> AgentTaskBatchCommands {
    AgentTaskBatchCommands {
        status: format!("homeboy agent-task fanout status {batch_id}"),
        artifacts: format!("homeboy agent-task fanout artifacts {batch_id}"),
        run_next: format!("homeboy agent-task run-next --fanout {batch_id}"),
        resume: format!("homeboy agent-task fanout resume {batch_id}"),
    }
}

fn child_run_id(batch_id: &str, task_id: &str) -> String {
    sanitize_id(&format!("{batch_id}-{task_id}"))
}

fn sanitize_id(value: &str) -> String {
    let sanitized = paths::sanitize_path_segment(value);
    if sanitized.is_empty() {
        format!("agent-task-batch-{}", Uuid::new_v4())
    } else {
        sanitized
    }
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

/// Durable agent-task batch storage bound to an explicit filesystem root.
#[derive(Clone, Debug)]
pub struct AgentTaskBatchStore {
    root: PathBuf,
}

impl AgentTaskBatchStore {
    pub fn new(roots: paths::PathRoots) -> Self {
        Self::from_data_root(roots.data().to_path_buf())
    }

    pub fn from_environment() -> Result<Self> {
        Ok(Self::new(paths::PathRoots::from_environment()?))
    }

    /// Bind batch's data-only storage without requiring unrelated config or
    /// artifact roots. Legacy batch entry points use this to preserve their
    /// historical `HOMEBOY_DATA_DIR`-only contract.
    pub fn from_data_root(data_root: PathBuf) -> Self {
        Self {
            root: data_root.join("agent-task-batches"),
        }
    }

    pub fn from_current_data_root() -> Result<Self> {
        Ok(Self::from_data_root(paths::homeboy_data()?))
    }

    pub fn submit_plan_batch_with<F, E>(
        &self,
        plan: &AgentTaskPlan,
        requested_batch_id: Option<&str>,
        run_record_exists: E,
        submit_child: F,
    ) -> Result<AgentTaskBatchRecord>
    where
        F: FnMut(&AgentTaskPlan, &str) -> Result<crate::agent_task_lifecycle::AgentTaskRunRecord>,
        E: FnMut(&str) -> Result<bool>,
    {
        submit_plan_batch_with_store(
            self,
            plan,
            requested_batch_id,
            run_record_exists,
            submit_child,
        )
    }

    pub fn persist_fanout_run_batch(
        &self,
        fanout_id: &str,
        plan_id: &str,
        children: &[FanoutRunBatchChild],
        metadata: Value,
    ) -> Result<AgentTaskBatchRecord> {
        persist_fanout_run_batch_in_store(self, fanout_id, plan_id, children, metadata)
    }

    pub fn claim_fanout_run_batch(&self, batch_id: &str) -> Result<Option<String>> {
        claim_fanout_run_batch_in_store(self, batch_id)
    }

    pub fn heartbeat_fanout_run_batch(&self, batch_id: &str, claim_id: &str) -> Result<()> {
        heartbeat_fanout_run_batch_in_store(self, batch_id, claim_id)
    }

    pub fn record_fanout_run_batch_failure(
        &self,
        batch_id: &str,
        claim_id: &str,
        stage: &str,
        failure: Value,
    ) -> Result<()> {
        record_fanout_run_batch_failure_in_store(self, batch_id, claim_id, stage, failure)
    }

    pub fn record_fanout_run_batch_failed_admissions<'a>(
        &self,
        batch_id: &str,
        failed_run_ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        record_fanout_run_batch_failed_admissions_in_store(self, batch_id, failed_run_ids)
    }

    pub fn status(&self, batch_id: &str) -> Result<AgentTaskBatchStatusReport> {
        self.status_with(
            batch_id,
            agent_task_lifecycle::persisted_status,
            agent_task_lifecycle::terminal_artifact_projection_readiness_bounded,
        )
    }

    /// The ambient half of the pair: this method's historical contract is that
    /// the child probe follows the process environment, so it resolves that root
    /// once, in one place, and hands it to the rooted body.
    pub fn expire_stalled_fanout_admission(&self, batch_id: &str) -> Result<bool> {
        let lifecycle_store =
            agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
        expire_stalled_fanout_admission_in_store(self, &lifecycle_store, batch_id)
    }

    pub fn status_with<S, P>(
        &self,
        batch_id: &str,
        child_status: S,
        projection_readiness: P,
    ) -> Result<AgentTaskBatchStatusReport>
    where
        S: FnMut(&str) -> Result<agent_task_lifecycle::AgentTaskRunRecord>,
        P: FnMut(&str) -> Result<Option<String>>,
    {
        status_in_store(self, batch_id, child_status, projection_readiness)
    }

    pub fn record_child_finalization(
        &self,
        batch_id: &str,
        child_run_id: &str,
        finalization: Value,
    ) -> Result<()> {
        record_child_finalization_in_store(self, batch_id, child_run_id, finalization)
    }

    /// Read the persisted durable batch record.
    pub fn read_batch_record(&self, batch_id: &str) -> Result<AgentTaskBatchRecord> {
        read_batch_record_in_store(self, batch_id)
    }

    /// Record that this batch's coordinator was cancelled by its durable owner.
    pub fn record_coordinator_cancellation(&self, batch_id: &str, reason: &str) -> Result<()> {
        record_coordinator_cancellation_in_store(self, batch_id, reason)
    }

    /// Whether this batch's coordinator has been cancelled by its durable owner.
    pub fn coordinator_is_cancelled(&self, batch_id: &str) -> bool {
        coordinator_is_cancelled_in_store(self, batch_id)
    }

    pub fn dependency_action_receipt(&self, batch_id: &str, key: &str) -> Result<Option<Value>> {
        dependency_action_receipt_in_store(self, batch_id, key)
    }

    pub fn record_dependency_action_receipt(
        &self,
        batch_id: &str,
        key: &str,
        receipt: Value,
    ) -> Result<()> {
        record_dependency_action_receipt_in_store(self, batch_id, key, receipt)
    }

    /// Read the graph-projected executable frontier.
    pub fn fanout_ready_child_run_ids(&self, batch_id: &str) -> Result<Option<HashSet<String>>> {
        fanout_ready_child_run_ids_in_store(self, batch_id)
    }

    /// Resolve the durable child runs owned by a fanout.
    pub fn owned_child_run_ids(&self, batch_id: &str) -> Result<HashSet<String>> {
        owned_child_run_ids_in_store(self, batch_id)
    }

    /// The ambient half of the pair: this method's historical contract is that
    /// child records and aggregates follow the process environment, so it
    /// resolves that root once, in one place, and hands it to the rooted body.
    pub fn artifacts(&self, batch_id: &str) -> Result<AgentTaskBatchArtifactsReport> {
        let lifecycle_store =
            agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
        artifacts_in_store(self, &lifecycle_store, batch_id)
    }

    /// Return the same dependency projection used by resume without persisting
    /// a read-side PR observation into either the lifecycle or batch record.
    pub fn fanout_dependency_graph_with_finalization_statuses(
        &self,
        batch_id: &str,
        statuses: &BTreeMap<String, String>,
    ) -> Result<Option<Value>> {
        fanout_dependency_graph_with_finalization_statuses_in_store(self, batch_id, statuses)
    }

    pub fn batch_path(&self, batch_id: &str) -> PathBuf {
        self.root.join(format!("{}.json", sanitize_id(batch_id)))
    }

    pub fn write_batch(&self, record: &AgentTaskBatchRecord) -> Result<()> {
        let path = self.batch_path(&record.batch_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(error.to_string(), Some(parent.display().to_string()))
            })?;
        }
        let raw = serde_json::to_string_pretty(record).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("serialize agent-task batch {}", record.batch_id)),
            )
        })?;
        let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, raw).map_err(|error| {
            Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
        })?;
        fs::rename(&temporary, &path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })
    }

    pub fn with_batch_lock<T>(
        &self,
        batch_id: &str,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let path = self.batch_path(batch_id).with_extension("lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(error.to_string(), Some(parent.display().to_string()))
            })?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
        lock.lock_exclusive().map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        let result = operation();
        let _ = FileExt::unlock(&lock);
        result
    }

    pub fn mutate_batch<T>(
        &self,
        batch_id: &str,
        operation: impl FnOnce(&mut AgentTaskBatchRecord) -> Result<T>,
    ) -> Result<T> {
        self.with_batch_lock(batch_id, || {
            let mut batch = self.read_batch(batch_id)?;
            let result = operation(&mut batch)?;
            self.write_batch(&batch)?;
            Ok(result)
        })
    }

    pub fn read_batch(&self, batch_id: &str) -> Result<AgentTaskBatchRecord> {
        let path = self.batch_path(batch_id);
        let raw = fs::read_to_string(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        serde_json::from_str(&raw).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse agent-task batch {}", batch_id)),
            )
        })
    }
}

/// Read the persisted durable batch record. Used by the batch resume path to
/// reconstruct each child cook after the original coordinator exited (#9525).
pub fn read_batch_record(batch_id: &str) -> Result<AgentTaskBatchRecord> {
    AgentTaskBatchStore::from_current_data_root()?.read_batch_record(batch_id)
}

pub fn read_batch_record_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
) -> Result<AgentTaskBatchRecord> {
    store.read_batch(batch_id)
}

/// Persist a child's resume-time finalization outcome into the durable batch
/// record's metadata, keyed by the child run id. Repeated resume calls overwrite
/// the same key so the batch record stays a single, convergent view of what has
/// been harvested — no duplicate finalization state accumulates (#9525).
pub fn record_child_finalization(
    batch_id: &str,
    child_run_id: &str,
    finalization: Value,
) -> Result<()> {
    AgentTaskBatchStore::from_current_data_root()?.record_child_finalization(
        batch_id,
        child_run_id,
        finalization,
    )
}

pub fn record_child_finalization_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    child_run_id: &str,
    finalization: Value,
) -> Result<()> {
    store.mutate_batch(batch_id, |batch| {
        let metadata = match &mut batch.metadata {
            Value::Object(map) => map,
            other => {
                *other = json!({});
                other.as_object_mut().expect("just-created object")
            }
        };
        let finalizations = metadata
            .entry("child_finalizations".to_string())
            .or_insert_with(|| json!({}));
        if !finalizations.is_object() {
            *finalizations = json!({});
        }
        finalizations
            .as_object_mut()
            .expect("child_finalizations is an object")
            .insert(child_run_id.to_string(), finalization);
        batch.updated_at = Some(now_timestamp());
        Ok(())
    })
}

/// Record that this batch's coordinator was cancelled by its durable owner.
///
/// # Why this cannot be inferred from child state
///
/// A batch coordinator's cancellation is not derivable from its children. A
/// child that has not been claimed yet has no lifecycle record at all, so
/// [`agent_task_lifecycle::cancel_run`] has nothing to terminalize for it and a
/// coordinator that only read child records would keep starting fresh cooks
/// after the batch was cancelled. Nor is the aggregate `state` a usable signal:
/// a batch whose first children succeeded before cancellation aggregates to
/// `PartialFailure`, not `Cancelled`.
///
/// So cancellation is recorded as its own explicit fact, and the claim loop
/// reads exactly this. It is deliberately kept in `metadata` rather than in
/// `state`, because [`status`] recomputes `state` from live child observation
/// on every read and would erase a marker written there.
pub fn record_coordinator_cancellation(batch_id: &str, reason: &str) -> Result<()> {
    AgentTaskBatchStore::from_current_data_root()?.record_coordinator_cancellation(batch_id, reason)
}

pub fn record_coordinator_cancellation_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    reason: &str,
) -> Result<()> {
    store.mutate_batch(batch_id, |batch| {
        if !batch.metadata.is_object() {
            batch.metadata = json!({});
        }
        let metadata = batch.metadata.as_object_mut().expect("metadata object");
        // First writer wins: a repeated cancellation must not rewrite the instant
        // the batch was actually stopped, so replayed cancellation converges.
        if metadata.contains_key(COORDINATOR_CANCELLATION_KEY) {
            return Ok(());
        }
        metadata.insert(
            COORDINATOR_CANCELLATION_KEY.to_string(),
            json!({
                "requested_at": now_timestamp(),
                "reason": reason,
            }),
        );
        batch.updated_at = Some(now_timestamp());
        Ok(())
    })
}

/// Whether this batch's coordinator has been cancelled by its durable owner.
///
/// An unreadable or absent batch record is reported as "not cancelled" rather
/// than as an error: this is consulted from the coordinator's claim loop, where
/// a transient read failure must not be allowed to abandon a live batch.
pub fn coordinator_is_cancelled(batch_id: &str) -> bool {
    AgentTaskBatchStore::from_current_data_root()
        .map(|store| store.coordinator_is_cancelled(batch_id))
        .unwrap_or(false)
}

pub fn coordinator_is_cancelled_in_store(store: &AgentTaskBatchStore, batch_id: &str) -> bool {
    store
        .read_batch(batch_id)
        .ok()
        .and_then(|batch| batch.metadata.get(COORDINATOR_CANCELLATION_KEY).cloned())
        .is_some_and(|value| value.is_object())
}

const COORDINATOR_CANCELLATION_KEY: &str = "coordinator_cancellation";

pub fn dependency_action_receipt(batch_id: &str, key: &str) -> Result<Option<Value>> {
    AgentTaskBatchStore::from_current_data_root()?.dependency_action_receipt(batch_id, key)
}

pub fn dependency_action_receipt_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    key: &str,
) -> Result<Option<Value>> {
    let batch = store.read_batch(batch_id)?;
    Ok(batch.metadata["dependency_action_receipts"][key]
        .as_object()
        .map(|_| batch.metadata["dependency_action_receipts"][key].clone()))
}

pub fn record_dependency_action_receipt(batch_id: &str, key: &str, receipt: Value) -> Result<()> {
    AgentTaskBatchStore::from_current_data_root()?
        .record_dependency_action_receipt(batch_id, key, receipt)
}

pub fn record_dependency_action_receipt_in_store(
    store: &AgentTaskBatchStore,
    batch_id: &str,
    key: &str,
    receipt: Value,
) -> Result<()> {
    store.mutate_batch(batch_id, |batch| {
        if !batch.metadata.is_object() {
            batch.metadata = json!({});
        }
        let receipts = batch
            .metadata
            .as_object_mut()
            .expect("metadata object")
            .entry("dependency_action_receipts")
            .or_insert_with(|| json!({}));
        if !receipts.is_object() {
            *receipts = json!({});
        }
        receipts
            .as_object_mut()
            .expect("receipts object")
            .insert(key.to_string(), receipt);
        batch.updated_at = Some(now_timestamp());
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch_store() -> (tempfile::TempDir, AgentTaskBatchStore) {
        let temp = tempfile::tempdir().expect("temporary batch data root");
        let store = AgentTaskBatchStore::from_data_root(temp.path().to_path_buf());
        (temp, store)
    }

    #[test]
    fn terminal_admission_failure_is_not_reclassified_as_unavailable() {
        let (_temp, store) = batch_store();
        let batch_id = format!("fanout-admission-failure-{}", uuid::Uuid::new_v4());
        let child_run_id = format!("cook-workspace-bound-child-{}", uuid::Uuid::new_v4());
        store
            .persist_fanout_run_batch(
                &batch_id,
                &batch_id,
                &[FanoutRunBatchChild {
                    task_id: "workspace-bound-child".to_string(),
                    run_id: child_run_id.clone(),
                }],
                Value::Null,
            )
            .expect("persist fanout");
        store
            .record_fanout_run_batch_failed_admissions(&batch_id, [child_run_id.as_str()])
            .expect("record terminal admission failure");

        let report = store.status(&batch_id).expect("terminal batch status");
        assert_eq!(report.status, "failed");
        assert_eq!(report.batch.state, AgentTaskBatchState::Failed);
        assert_eq!(report.totals.failed, 1);
        assert!(report.unavailable_child_runs.is_empty());
    }

    #[test]
    fn aggregate_state_matrix_is_shared_by_immediate_and_durable_batches() {
        for (name, totals, state, exit_code) in [
            (
                "queued-with-terminal-child",
                AgentTaskBatchTotals {
                    queued: 1,
                    succeeded: 1,
                    ..Default::default()
                },
                AgentTaskBatchState::Queued,
                0,
            ),
            (
                "running-with-terminal-child",
                AgentTaskBatchTotals {
                    running: 1,
                    failed: 1,
                    ..Default::default()
                },
                AgentTaskBatchState::Running,
                0,
            ),
            (
                "all-success",
                AgentTaskBatchTotals {
                    succeeded: 2,
                    ..Default::default()
                },
                AgentTaskBatchState::Succeeded,
                0,
            ),
            (
                "success-and-no-op",
                AgentTaskBatchTotals {
                    succeeded: 2,
                    ..Default::default()
                },
                AgentTaskBatchState::Succeeded,
                0,
            ),
            (
                "mixed",
                AgentTaskBatchTotals {
                    succeeded: 1,
                    failed: 1,
                    ..Default::default()
                },
                AgentTaskBatchState::PartialFailure,
                1,
            ),
            (
                "all-failed",
                AgentTaskBatchTotals {
                    failed: 2,
                    ..Default::default()
                },
                AgentTaskBatchState::Failed,
                1,
            ),
            (
                "coordinator-infrastructure-failure",
                AgentTaskBatchTotals {
                    failed: 1,
                    ..Default::default()
                },
                AgentTaskBatchState::Failed,
                1,
            ),
            (
                "cancelled",
                AgentTaskBatchTotals {
                    cancelled: 2,
                    ..Default::default()
                },
                AgentTaskBatchState::Cancelled,
                1,
            ),
            (
                "timed-out",
                AgentTaskBatchTotals {
                    timed_out: 1,
                    ..Default::default()
                },
                AgentTaskBatchState::TimedOut,
                1,
            ),
        ] {
            let actual = aggregate_state(&totals);
            assert_eq!(actual, state, "{name}");
            assert_eq!(actual.exit_code(), exit_code, "{name}");
        }
    }

    #[test]
    fn record_child_finalization_is_idempotent_and_convergent() {
        let (_temp, store) = batch_store();
        store
            .persist_fanout_run_batch(
                "batch/converge",
                "fanout/converge",
                &[FanoutRunBatchChild {
                    task_id: "a".to_string(),
                    run_id: "batch_converge-a".to_string(),
                }],
                Value::Null,
            )
            .expect("batch submitted");

        store
            .record_child_finalization(
                "batch/converge",
                "batch_converge-a",
                json!({ "status": "review_ready", "attempt": 1 }),
            )
            .expect("first finalization recorded");
        // A repeated resume overwrites the same key rather than accumulating.
        store
            .record_child_finalization(
                "batch/converge",
                "batch_converge-a",
                json!({ "status": "review_ready", "attempt": 2 }),
            )
            .expect("second finalization overwrites");

        let batch = store.read_batch("batch/converge").expect("batch record");
        let finalizations = batch.metadata["child_finalizations"]
            .as_object()
            .expect("child_finalizations recorded");
        assert_eq!(finalizations.len(), 1);
        assert_eq!(finalizations["batch_converge-a"]["attempt"], 2);
    }

    #[test]
    fn fanout_run_plan_persists_batch_record_readable_by_status() {
        let (_temp, store) = batch_store();
        let children = vec![
            FanoutRunBatchChild {
                task_id: "audit".to_string(),
                run_id: "cook-audit".to_string(),
            },
            FanoutRunBatchChild {
                task_id: "rules".to_string(),
                run_id: "cook-rules".to_string(),
            },
        ];

        let record = store
            .persist_fanout_run_batch(
                "rules-memory-gaps-20260721",
                "rules-memory-gaps-20260721",
                &children,
                json!({ "source": "fanout-run-plan" }),
            )
            .expect("batch record persisted");

        assert_eq!(record.batch_id, "rules-memory-gaps-20260721");
        assert_eq!(record.task_count, 2);
        assert_eq!(record.state, AgentTaskBatchState::Planning);

        // The exact failure from #9397: `fanout status <id>` could not read the
        // batch file because run-plan never wrote it. It is now readable.
        let persisted = store
            .read_batch("rules-memory-gaps-20260721")
            .expect("batch record readable");
        assert_eq!(persisted.child_runs.len(), 2);
        assert_eq!(persisted.child_runs[0].run_id, "cook-audit");
        assert_eq!(persisted.child_runs[1].run_id, "cook-rules");
        assert!(persisted
            .child_runs
            .iter()
            .all(|child| child.state == AgentTaskRunState::Queued));
    }

    #[test]
    fn fanout_run_plan_rejects_duplicate_child_run_ids() {
        let children = vec![
            FanoutRunBatchChild {
                task_id: "a".to_string(),
                run_id: "cook-dup".to_string(),
            },
            FanoutRunBatchChild {
                task_id: "b".to_string(),
                run_id: "cook-dup".to_string(),
            },
        ];

        let error = persist_fanout_run_batch("dup-batch", "dup-batch", &children, Value::Null)
            .expect_err("duplicate child run ids rejected");

        assert!(error.message.contains("duplicated"));
    }

    #[test]
    fn fanout_run_plan_rejects_empty_cooks() {
        let error = persist_fanout_run_batch("empty-batch", "empty-batch", &[], Value::Null)
            .expect_err("empty cooks rejected");

        assert!(error.message.contains("at least one cook"));
    }

    #[test]
    fn fanout_run_plan_reuses_a_repaired_preflight_roster_without_overwriting_it() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "repair".to_string(),
            run_id: "repair-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("repair-wave", "repair-wave", &children, json!({}))
            .expect("persist planning record");
        let claim_id = store
            .claim_fanout_run_batch("repair-wave")
            .expect("claim")
            .expect("claim id");
        store
            .record_fanout_run_batch_failure(
                "repair-wave",
                &claim_id,
                "worktree_preflight",
                json!({ "message": "worktree missing" }),
            )
            .expect("record preflight failure");
        let failed = store
            .status("repair-wave")
            .expect("preflight failure remains observable");
        assert_eq!(failed.batch.state, AgentTaskBatchState::Failed);
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.totals.failed, 1);
        assert!(failed.unavailable_child_runs.is_empty());

        let replay = store
            .persist_fanout_run_batch("repair-wave", "repair-wave", &children, json!({}))
            .expect("replay repaired roster");

        assert_eq!(replay.state, AgentTaskBatchState::Failed);
        assert!(replay.metadata.get("terminal_failure").is_some());
        assert_eq!(replay.child_runs[0].run_id, "repair-run");
    }

    #[test]
    fn fanout_run_plan_replay_preserves_an_interrupted_coordinator_state() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "interrupted".to_string(),
            run_id: "interrupted-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("interrupted-wave", "interrupted-wave", &children, json!({}))
            .expect("persist planning record");
        let mut interrupted = store
            .read_batch("interrupted-wave")
            .expect("read planning record");
        interrupted.metadata = json!({ "coordinator": { "stage": "admission" } });
        store
            .write_batch(&interrupted)
            .expect("record interruption checkpoint");

        let replay = store
            .persist_fanout_run_batch("interrupted-wave", "interrupted-wave", &children, json!({}))
            .expect("idempotent replay");

        assert_eq!(replay.metadata["coordinator"]["stage"], "admission");
        assert_eq!(replay.child_runs[0].run_id, "interrupted-run");
    }

    #[test]
    fn fanout_run_plan_allows_only_one_coordinator_claim() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "only".to_string(),
            run_id: "only-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("claimed-wave", "claimed-wave", &children, json!({}))
            .expect("persist planning record");

        assert!(store
            .claim_fanout_run_batch("claimed-wave")
            .expect("first claim")
            .is_some());
        assert!(store
            .claim_fanout_run_batch("claimed-wave")
            .expect("second claim is denied")
            .is_none());
        assert_eq!(
            store
                .read_batch("claimed-wave")
                .expect("claimed batch")
                .state,
            AgentTaskBatchState::Admitting
        );
    }

    #[test]
    fn fanout_run_plan_recovers_an_abandoned_coordinator_lease() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "lease".to_string(),
            run_id: "lease-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("expired-wave", "expired-wave", &children, json!({}))
            .expect("persist planning record");
        assert!(store
            .claim_fanout_run_batch("expired-wave")
            .expect("first claim")
            .is_some());
        store
            .mutate_batch("expired-wave", |batch| {
                batch.metadata["coordinator"]["heartbeat_at"] = json!("2000-01-01T00:00:00Z");
                Ok(())
            })
            .expect("expire lease");

        assert!(store
            .claim_fanout_run_batch("expired-wave")
            .expect("recover abandoned claim")
            .is_some());
        assert_eq!(
            store
                .read_batch("expired-wave")
                .expect("reclaimed batch")
                .state,
            AgentTaskBatchState::Admitting
        );
    }

    #[test]
    fn expired_admission_is_a_durable_terminal_blocker_with_a_recovery_command() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "stuck".to_string(),
            run_id: "stuck-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("stuck-wave", "stuck-wave", &children, json!({}))
            .expect("persist");
        store
            .claim_fanout_run_batch("stuck-wave")
            .expect("claim")
            .expect("claim id");
        store
            .mutate_batch("stuck-wave", |batch| {
                batch.metadata["coordinator"]["admission_deadline_at"] =
                    json!("2000-01-01T00:00:00Z");
                batch.metadata["coordinator"]["heartbeat_at"] = json!("2000-01-01T00:00:00Z");
                batch.metadata["replan_command"] =
                    json!("homeboy agent-task fanout run-plan --input @plan.json");
                Ok(())
            })
            .expect("expire admission");

        assert!(store
            .expire_stalled_fanout_admission("stuck-wave")
            .expect("terminalize stalled admission"));
        let status = store.status("stuck-wave").expect("read failed batch");

        assert_eq!(status.batch.state, AgentTaskBatchState::Failed);
        assert_eq!(status.batch.child_runs[0].state, AgentTaskRunState::Failed);
        assert_eq!(
            status.batch.metadata["terminal_failure"]["failure"]["code"],
            "coordinator_admission_timeout"
        );
        assert_eq!(
            status.batch.metadata["terminal_failure"]["failure"]["next_action"],
            "homeboy agent-task fanout run-plan --input @plan.json"
        );
    }

    #[test]
    fn stalled_admission_recovery_resumes_only_when_every_recipe_exists() {
        let batch = AgentTaskBatchRecord {
            schema: AGENT_TASK_BATCH_SCHEMA.to_string(),
            batch_id: "recipe-wave".to_string(),
            plan_id: "recipe-wave".to_string(),
            state: AgentTaskBatchState::Admitting,
            submitted_at: now_timestamp(),
            updated_at: None,
            task_count: 2,
            child_runs: vec![
                AgentTaskBatchChildRun {
                    task_id: "a".to_string(),
                    run_id: "a-run".to_string(),
                    state: AgentTaskRunState::Queued,
                },
                AgentTaskBatchChildRun {
                    task_id: "b".to_string(),
                    run_id: "b-run".to_string(),
                    state: AgentTaskRunState::Queued,
                },
            ],
            metadata: json!({
                "replan_command": "homeboy agent-task fanout run-plan --input @plan.json"
            }),
        };

        assert_eq!(
            stalled_admission_recovery_command_with(&batch, |_| Ok(false))
                .expect("pre-recipe recovery command"),
            "homeboy agent-task fanout run-plan --input @plan.json"
        );
        assert_eq!(
            stalled_admission_recovery_command_with(&batch, |_| Ok(true))
                .expect("recipe-backed recovery command"),
            "homeboy agent-task fanout resume recipe-wave"
        );
    }

    #[test]
    fn a_heartbeat_renews_slow_preflight_admission_without_false_expiry() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "slow-preflight".to_string(),
            run_id: "slow-preflight-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("slow-preflight", "slow-preflight", &children, json!({}))
            .expect("persist");
        let claim_id = store
            .claim_fanout_run_batch("slow-preflight")
            .expect("claim")
            .expect("claim id");
        store
            .mutate_batch("slow-preflight", |batch| {
                batch.metadata["coordinator"]["admission_deadline_at"] =
                    json!("2000-01-01T00:00:00Z");
                batch.metadata["coordinator"]["heartbeat_at"] = json!("2000-01-01T00:00:00Z");
                Ok(())
            })
            .expect("age initial admission lease");

        store
            .heartbeat_fanout_run_batch("slow-preflight", &claim_id)
            .expect("healthy preflight heartbeat");

        assert!(!store
            .expire_stalled_fanout_admission("slow-preflight")
            .expect("live preflight is retained"));
        assert_ne!(
            store.read_batch("slow-preflight").expect("batch").metadata["coordinator"]
                ["admission_deadline_at"],
            json!("2000-01-01T00:00:00Z")
        );
    }

    #[test]
    fn expired_admission_with_a_fresh_heartbeat_or_started_child_is_not_terminated() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "live".to_string(),
            run_id: "live-run".to_string(),
        }];
        for (batch_id, started) in [("live-wave", false), ("started-wave", true)] {
            store
                .persist_fanout_run_batch(batch_id, batch_id, &children, json!({}))
                .expect("persist");
            store
                .claim_fanout_run_batch(batch_id)
                .expect("claim")
                .expect("claim id");
            store
                .mutate_batch(batch_id, |batch| {
                    batch.metadata["coordinator"]["admission_deadline_at"] =
                        json!("2000-01-01T00:00:00Z");
                    if started {
                        batch.metadata["coordinator"]["heartbeat_at"] =
                            json!("2000-01-01T00:00:00Z");
                        batch.child_runs[0].state = AgentTaskRunState::Running;
                    }
                    Ok(())
                })
                .expect("set durable progress");

            assert!(
                !store
                    .expire_stalled_fanout_admission(batch_id)
                    .expect("observe active admission"),
                "{batch_id} must retain its active coordinator"
            );
            assert_eq!(
                store.read_batch(batch_id).expect("batch").state,
                AgentTaskBatchState::Admitting
            );
        }
    }

    #[test]
    fn status_projects_the_exact_returned_admission_blocker() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "source".to_string(),
            run_id: "source-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("source-wave", "source-wave", &children, json!({}))
            .expect("persist");
        let claim_id = store
            .claim_fanout_run_batch("source-wave")
            .expect("claim")
            .expect("claim id");
        store
            .record_fanout_run_batch_failure(
                "source-wave",
                &claim_id,
                "source_staging",
                json!({
                    "code": "source_package_too_large",
                    "message": "source package exceeds configured entry or total size bounds",
                    "next_action": "homeboy agent-task fanout resume source-wave",
                }),
            )
            .expect("record source blocker");

        let status = store.status("source-wave").expect("project blocker");
        assert_eq!(
            status.admission_blocker.as_ref().unwrap()["stage"],
            "source_staging"
        );
        assert_eq!(
            status.admission_blocker.as_ref().unwrap()["failure"]["code"],
            "source_package_too_large"
        );
        assert_eq!(
            status.next_actions[0],
            "homeboy agent-task fanout resume source-wave"
        );
    }

    #[test]
    fn replacement_claim_rejects_a_stale_owner_failure() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "stale".to_string(),
            run_id: "stale-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("stale-wave", "stale-wave", &children, json!({}))
            .expect("persist");
        let old = store
            .claim_fanout_run_batch("stale-wave")
            .expect("claim")
            .expect("claim id");
        store
            .mutate_batch("stale-wave", |batch| {
                batch.metadata["coordinator"]["heartbeat_at"] = json!("2000-01-01T00:00:00Z");
                Ok(())
            })
            .expect("expire");
        let replacement = store
            .claim_fanout_run_batch("stale-wave")
            .expect("replacement claim")
            .expect("replacement id");

        assert!(store
            .record_fanout_run_batch_failure("stale-wave", &old, "coordinator", json!({}))
            .is_err());
        store
            .heartbeat_fanout_run_batch("stale-wave", &replacement)
            .expect("replacement heartbeat");
    }

    #[test]
    fn fresh_owned_heartbeat_prevents_reclaim_after_an_expired_prior_timestamp() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "live".to_string(),
            run_id: "live-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("live-wave", "live-wave", &children, json!({}))
            .expect("persist");
        let claim_id = store
            .claim_fanout_run_batch("live-wave")
            .expect("claim")
            .expect("claim id");
        // Simulate a coordinator whose initial lease would have expired, then
        // refresh it as the periodic worker does during a long batch run.
        store
            .mutate_batch("live-wave", |batch| {
                batch.metadata["coordinator"]["heartbeat_at"] = json!("2000-01-01T00:00:00Z");
                Ok(())
            })
            .expect("age heartbeat");
        store
            .heartbeat_fanout_run_batch("live-wave", &claim_id)
            .expect("live heartbeat");

        assert!(store
            .claim_fanout_run_batch("live-wave")
            .expect("claim check")
            .is_none());
    }

    #[test]
    fn stale_heartbeat_cannot_change_replacement_owner() {
        let (_temp, store) = batch_store();
        let children = vec![FanoutRunBatchChild {
            task_id: "heartbeat".to_string(),
            run_id: "heartbeat-run".to_string(),
        }];
        store
            .persist_fanout_run_batch("heartbeat-wave", "heartbeat-wave", &children, json!({}))
            .expect("persist");
        let old = store
            .claim_fanout_run_batch("heartbeat-wave")
            .expect("claim")
            .expect("claim id");
        store
            .mutate_batch("heartbeat-wave", |batch| {
                batch.metadata["coordinator"]["heartbeat_at"] = json!("2000-01-01T00:00:00Z");
                Ok(())
            })
            .expect("expire");
        let replacement = store
            .claim_fanout_run_batch("heartbeat-wave")
            .expect("replacement")
            .expect("replacement id");

        assert!(store
            .heartbeat_fanout_run_batch("heartbeat-wave", &old)
            .is_err());
        assert_eq!(
            store.read_batch("heartbeat-wave").expect("batch").metadata["coordinator"]["claim_id"],
            json!(replacement)
        );
    }
}

use std::fs;
use std::path::PathBuf;

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};

use super::{
    sanitize_run_id, AgentTaskCookIndex, AgentTaskCookIndexAttempt,
    AgentTaskCookLatestSubstantiveCandidate, AgentTaskRunRecord, AgentTaskRunState,
};
use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy_core::engine::local_files::{
    write_json_file as write_json, write_json_file_owner_only as write_private_json,
};
use homeboy_core::observation::{ObservationStore, RunListFilter, RunRecord, RunStatus};
use homeboy_core::{build_identity, paths, Error, ErrorCode, Result};

#[cfg(test)]
static FAIL_NEXT_RECORD_WRITE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static INTERRUPT_AFTER_TERMINAL_COMMIT: AtomicBool = AtomicBool::new(false);

pub(super) fn write_plan(run_id: &str, plan: &AgentTaskPlan) -> Result<PathBuf> {
    homeboy_core::config::with_config_lock(|| {
        let mut plan = plan.clone();
        migrate_execution_budget(&mut plan)?;
        let path = run_dir(run_id)?.join("plan.json");
        write_private_json(&path, &plan)?;
        Ok(path)
    })
}

pub(super) fn read_plan_path(path: &str) -> Result<AgentTaskPlan> {
    let plan = read_json(&PathBuf::from(path))?;
    validate_execution_budget(&plan)?;
    Ok(plan)
}

pub(super) fn read_controller_plan(run_id: &str) -> Result<AgentTaskPlan> {
    let path = controller_plan_path(run_id)?;
    let plan = read_json(&path).map_err(|error| {
        Error::internal_io(
            format!(
                "authoritative controller-owned plan for agent-task run `{run_id}` is unavailable at {}; lifecycle record plan_path is runner execution transport and cannot be used for controller readback: {}",
                path.display(),
                error.message
            ),
            Some(path.display().to_string()),
        )
    })?;
    validate_execution_budget(&plan)?;
    Ok(plan)
}

pub(super) fn controller_plan_path(run_id: &str) -> Result<PathBuf> {
    Ok(run_dir(run_id)?.join("plan.json"))
}

/// Controller lifecycle operations resolve the plan from their durable run
/// identity. `AgentTaskRunRecord::plan_path` can be runner-local transport
/// evidence after a Lab projection and is never controller execution authority.
pub(super) fn read_controller_plan_for_execution(run_id: &str) -> Result<AgentTaskPlan> {
    homeboy_core::config::with_config_lock(|| {
        let path = controller_plan_path(run_id)?;
        let mut plan = read_controller_plan(run_id)?;
        if migrate_execution_budget(&mut plan)? {
            write_private_json(&path, &plan)?;
        }
        Ok(plan)
    })
}

fn validate_execution_budget(plan: &AgentTaskPlan) -> Result<()> {
    match plan.options.execution_budget.version {
        0 | crate::agent_task_scheduler::AgentTaskExecutionBudget::VERSION => Ok(()),
        version => Err(Error::validation_invalid_argument(
            "execution_budget.version",
            format!(
                "unsupported agent-task execution budget version {version}; this Homeboy build supports version {}",
                crate::agent_task_scheduler::AgentTaskExecutionBudget::VERSION
            ),
            Some(version.to_string()),
            None,
        )),
    }
}

fn migrate_execution_budget(plan: &mut AgentTaskPlan) -> Result<bool> {
    plan.options
        .execution_budget
        .migrate_legacy()
        .map_err(|message| {
            Error::validation_invalid_argument(
                "execution_budget.version",
                message,
                Some(plan.options.execution_budget.version.to_string()),
                None,
            )
        })
}

pub(super) fn write_aggregate(run_id: &str, aggregate: &AgentTaskAggregate) -> Result<PathBuf> {
    let path = run_dir(run_id)?.join("aggregate.json");
    write_json(&path, aggregate)?;
    Ok(path)
}

pub(super) fn read_aggregate(run_id: &str) -> Result<AgentTaskAggregate> {
    match read_mirrored_aggregate(run_id)? {
        Some(aggregate) => Ok(aggregate),
        None => read_json(&aggregate_path(run_id)?),
    }
}

pub(super) fn aggregate_path(run_id: &str) -> Result<PathBuf> {
    Ok(run_dir(run_id)?.join("aggregate.json"))
}

pub(super) fn write_record(record: &AgentTaskRunRecord) -> Result<()> {
    write_record_with_aggregate(record, read_mirrored_aggregate(&record.run_id)?)
}

/// Serialize a record read-modify-write so independent lifecycle projections do
/// not replace metadata written by another controller operation.
pub(super) fn mutate_record(
    run_id: &str,
    mutate: impl FnOnce(&mut AgentTaskRunRecord) -> bool,
) -> Result<Option<AgentTaskRunRecord>> {
    let record = homeboy_core::config::with_config_lock(|| {
        let mut record = read_record(run_id)?;
        if !mutate(&mut record) {
            return Ok(None);
        }
        let committed = write_record_with_aggregate_without_workspace_authority(
            &record,
            read_mirrored_aggregate(&record.run_id)?,
        )?;
        Ok(Some(committed))
    })?;
    if let Some(record) = record.as_ref() {
        // Terminal authority has its own config lock. Persist it after releasing
        // the record mutation lock so a terminal update cannot self-deadlock.
        super::workspace_authority::persist_terminal_from_record(record)?;
    }
    Ok(record)
}

/// Commit the controller projection and child aggregate in one observation row.
/// The JSON aggregate is a post-commit cache: readers use the committed row, so
/// interruption before or after cache persistence exposes a complete state.
pub(super) fn write_aggregate_and_record(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<PathBuf> {
    write_record_with_aggregate(record, Some(aggregate.clone()))?;
    #[cfg(test)]
    if INTERRUPT_AFTER_TERMINAL_COMMIT.swap(false, Ordering::SeqCst) {
        return Err(Error::internal_io(
            "injected interruption after terminal lifecycle commit",
            Some(record.run_id.clone()),
        ));
    }
    write_aggregate(&record.run_id, aggregate)
}

#[cfg(test)]
pub(super) fn fail_next_record_write_for_test() {
    FAIL_NEXT_RECORD_WRITE.store(true, Ordering::SeqCst);
}

#[cfg(test)]
pub(super) fn interrupt_after_terminal_commit_for_test() {
    INTERRUPT_AFTER_TERMINAL_COMMIT.store(true, Ordering::SeqCst);
}

fn write_record_with_aggregate(
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
) -> Result<()> {
    let committed = write_record_with_aggregate_without_workspace_authority(record, aggregate)?;
    super::workspace_authority::persist_terminal_from_record(&committed)
}

fn write_record_with_aggregate_without_workspace_authority(
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
) -> Result<AgentTaskRunRecord> {
    #[cfg(test)]
    if FAIL_NEXT_RECORD_WRITE.swap(false, Ordering::SeqCst) {
        return Err(Error::internal_io(
            "injected lifecycle record persistence failure",
            Some(record.run_id.clone()),
        ));
    }
    let store = ObservationStore::open_initialized()?;
    let metadata_json = merge_observation_metadata(
        store
            .get_run(&record.run_id)?
            .map(|run| run.metadata_json)
            .unwrap_or_else(|| json!({})),
        observation_metadata(record, aggregate)?,
    );
    store.upsert_imported_run_preserving_terminal(&RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: plan_id_component(record),
        started_at: record.submitted_at.clone(),
        finished_at: terminal_finished_at(record),
        status: run_status(record.state).to_string(),
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version: Some(build_identity::current().version),
        git_sha: None,
        rig_id: None,
        metadata_json,
    })?;
    let committed = store.get_run(&record.run_id)?.ok_or_else(|| {
        Error::internal_unexpected(format!(
            "committed agent-task run record is unavailable: {}",
            record.run_id
        ))
    })?;
    record_from_run(&committed)
}

pub(super) fn read_record(run_id: &str) -> Result<AgentTaskRunRecord> {
    let store = ObservationStore::open_initialized()?;
    let run = store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("agent-task run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })?;
    record_from_run(&run)
}

/// Bypass typed record validation solely to seed corruption-recovery fixtures.
/// Production writes and normal test rewrites must use `write_record`.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn inject_raw_record_metadata_for_corruption_test(
    run_id: &str,
    inject: impl FnOnce(&mut Value),
) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let mut run = store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("agent-task run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })?;
    inject(&mut run.metadata_json);
    store.upsert_imported_run(&run)
}

pub(super) fn write_cook_index_attempt(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    recorded_at: String,
    candidate: Option<AgentTaskCookLatestSubstantiveCandidate>,
) -> Result<AgentTaskCookIndex> {
    let cook_id = sanitize_run_id(cook_id);
    let run_id = sanitize_run_id(run_id);
    let path = cook_index_path(&cook_id)?;
    let mut index = if path.exists() {
        read_json(&path)?
    } else {
        AgentTaskCookIndex {
            schema: super::records::schemas::COOK_INDEX.to_string(),
            cook_id: cook_id.clone(),
            latest_run_id: run_id.clone(),
            latest_substantive_candidate: None,
            attempts: Vec::new(),
        }
    };
    if index
        .attempts
        .iter()
        .any(|entry| entry.run_id == run_id && entry.attempt != attempt)
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "durable Cook index maps this run to a different attempt",
            Some(run_id),
            None,
        ));
    }
    index.cook_id = cook_id;
    index.attempts.retain(|entry| entry.run_id != run_id);
    index.attempts.push(AgentTaskCookIndexAttempt {
        attempt,
        run_id,
        recorded_at,
    });
    index.latest_run_id = index
        .attempts
        .iter()
        .max_by_key(|entry| entry.attempt)
        .expect("Cook index has the recorded attempt")
        .run_id
        .clone();
    if let Some(candidate) = candidate {
        let replace = index
            .latest_substantive_candidate
            .as_ref()
            .is_none_or(|current| {
                candidate.attempt > current.attempt
                    || (candidate.attempt == current.attempt && candidate.run_id >= current.run_id)
            });
        if replace {
            index.latest_substantive_candidate = Some(candidate);
        }
    }
    write_json(&path, &index)?;
    Ok(index)
}

#[cfg(test)]
pub(super) fn write_cook_index_for_test(index: &AgentTaskCookIndex) -> Result<()> {
    write_json(&cook_index_path(&sanitize_run_id(&index.cook_id))?, index)
}

pub(super) fn read_cook_index(cook_id: &str) -> Result<AgentTaskCookIndex> {
    read_json(&cook_index_path(cook_id)?)
}

pub(super) fn cook_index_exists(cook_id: &str) -> Result<bool> {
    Ok(cook_index_path(cook_id)?.exists())
}

/// Claim the one terminal notification a Cook is allowed to deliver.
///
/// The claim is the creation of the marker file itself: `create_new` is
/// `O_EXCL`, so exactly one caller — in any process, on any thread — observes
/// `Ok(true)`. This is the cook-scoped counterpart of
/// `ObservationStore::mark_notification_delivered`, which cannot serve here
/// because it is keyed on a `runs` row and a Cook id is an alias with no row
/// of its own.
pub(super) fn claim_cook_notification(cook_id: &str, marker: &Value) -> Result<bool> {
    let path = cook_index_path(&sanitize_run_id(cook_id))?.with_file_name("notification.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            let bytes = serde_json::to_vec(marker).map_err(|error| {
                Error::internal_json(error.to_string(), Some(path.display().to_string()))
            })?;
            file.write_all(&bytes).map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}

pub(super) fn update_cook_index(
    cook_id: &str,
    mutate: impl FnOnce(&mut AgentTaskCookIndex) -> bool,
) -> Result<Option<AgentTaskCookIndex>> {
    let path = cook_index_path(&sanitize_run_id(cook_id))?;
    if !path.exists() {
        return Ok(None);
    }
    let mut index = read_json(&path)?;
    if mutate(&mut index) {
        // `write_json` is atomic; this makes the pointer update one durable
        // index transaction without nesting the configuration lock it owns.
        write_json(&path, &index)?;
    }
    Ok(Some(index))
}

pub(super) fn record_exists(run_id: &str) -> Result<bool> {
    Ok(ObservationStore::open_initialized()?
        .get_run(run_id)?
        .is_some())
}

pub(super) fn record_lacks_typed_metadata(run_id: &str) -> Result<bool> {
    Ok(ObservationStore::open_initialized()?
        .get_run(run_id)?
        .is_some_and(|run| run.metadata_json.get("agent_task_run").is_none()))
}

pub(super) fn read_records() -> Result<Vec<AgentTaskRunRecord>> {
    Ok(read_records_with_health()?.0)
}

pub(super) fn read_retry_successors(source_run_id: &str) -> Result<Vec<AgentTaskRunRecord>> {
    let runs = ObservationStore::open_initialized()?.list_runs_by_retry_of(
        "agent-task",
        source_run_id,
        16,
    )?;
    runs.iter().map(record_from_run).collect()
}

pub(super) fn read_records_with_health(
) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
    let mut health = super::AgentTaskRecordHealthSummary::healthy();
    let mut records = Vec::new();
    for run in observation_runs()? {
        match super::health::diagnose_run(&run) {
            Ok(record) => {
                health.healthy += 1;
                records.push(record);
            }
            Err(item) => super::health::record_health_item(&mut health, item),
        }
    }
    Ok((records, health))
}

pub(super) fn observation_runs() -> Result<Vec<RunRecord>> {
    let store = ObservationStore::open_initialized()?;
    let filter = RunListFilter {
        kind: Some("agent-task".to_string()),
        limit: Some(1000),
        ..Default::default()
    };
    store.list_runs(filter)
}

fn observation_metadata(
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
) -> Result<Value> {
    let record_json = serde_json::to_value(record).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("serialize agent-task run {}", record.run_id)),
        )
    })?;
    Ok(json!({
        "schema": "homeboy/agent-task-observation-record/v1",
        "agent_task_run": record_json,
        "agent_task_aggregate": aggregate,
    }))
}

fn merge_observation_metadata(mut existing: Value, typed: Value) -> Value {
    if !existing.is_object() {
        existing = json!({ "homeboy_original_metadata": existing });
    }
    if let (Some(existing), Some(typed)) = (existing.as_object_mut(), typed.as_object()) {
        for (key, value) in typed {
            existing.insert(key.clone(), value.clone());
        }
    }
    existing
}

pub(super) fn record_from_run(run: &RunRecord) -> Result<AgentTaskRunRecord> {
    let value = run.metadata_json.get("agent_task_run").ok_or_else(|| {
        Error::new(
            ErrorCode::InternalJsonError,
            format!(
                "observation run {} is missing agent_task_run metadata",
                run.id
            ),
            json!({ "context": run.id }),
        )
    })?;
    let mut record: AgentTaskRunRecord =
        serde_json::from_value(value.clone()).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse agent-task run {}", run.id)),
            )
        })?;
    record.hydrate_legacy_lab_handoff();
    if let Some(problem) = record.lab_handoff_validation_error() {
        return Err(Error::internal_json(
            problem,
            Some(format!("validate agent-task Lab handoff {}", run.id)),
        ));
    }
    Ok(record)
}

fn read_mirrored_aggregate(run_id: &str) -> Result<Option<AgentTaskAggregate>> {
    let store = ObservationStore::open_initialized()?;
    let Some(run) = store.get_run(run_id)? else {
        return Ok(None);
    };
    let Some(value) = run.metadata_json.get("agent_task_aggregate") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse agent-task aggregate {}", run.id)),
            )
        })
}

fn run_status(state: AgentTaskRunState) -> &'static str {
    match state {
        AgentTaskRunState::Queued | AgentTaskRunState::Running => RunStatus::Running.as_str(),
        AgentTaskRunState::Succeeded => RunStatus::Pass.as_str(),
        AgentTaskRunState::CandidateRecoverable => RunStatus::Fail.as_str(),
        AgentTaskRunState::PartialRecoverable => RunStatus::Fail.as_str(),
        AgentTaskRunState::PartialFailure | AgentTaskRunState::Failed => RunStatus::Fail.as_str(),
        AgentTaskRunState::Cancelled => RunStatus::Skipped.as_str(),
    }
}

fn terminal_finished_at(record: &AgentTaskRunRecord) -> Option<String> {
    if record.state.is_terminal() {
        record
            .updated_at
            .clone()
            .or_else(|| Some(record.submitted_at.clone()))
    } else {
        None
    }
}

fn plan_id_component(record: &AgentTaskRunRecord) -> Option<String> {
    record
        .metadata
        .get("repo")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            record
                .metadata
                .get("kind")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let raw = fs::read_to_string(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))
}

pub(super) fn run_dir(run_id: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("agent-task-runs")
        .join(sanitize_run_id(run_id)))
}

fn cook_index_path(cook_id: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("agent-task-cooks")
        .join(sanitize_run_id(cook_id))
        .join("index.json"))
}

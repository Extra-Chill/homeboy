use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{json, Value};

use super::{sanitize_run_id, AgentTaskRunRecord, AgentTaskRunState};
use crate::core::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use crate::core::observation::{ObservationStore, RunListFilter, RunRecord, RunStatus};
use crate::core::{paths, Error, ErrorCode, Result};

pub(super) fn write_plan(run_id: &str, plan: &AgentTaskPlan) -> Result<PathBuf> {
    let path = run_dir(run_id)?.join("plan.json");
    write_json(&path, plan)?;
    Ok(path)
}

pub(super) fn read_plan_path(path: &str) -> Result<AgentTaskPlan> {
    read_json(&PathBuf::from(path))
}

pub(super) fn write_aggregate(run_id: &str, aggregate: &AgentTaskAggregate) -> Result<PathBuf> {
    let path = run_dir(run_id)?.join("aggregate.json");
    write_json(&path, aggregate)?;
    mirror_aggregate(run_id, aggregate)?;
    Ok(path)
}

pub(super) fn read_aggregate(run_id: &str) -> Result<AgentTaskAggregate> {
    read_json(&aggregate_path(run_id)?)
        .or_else(|error| read_mirrored_aggregate(run_id)?.ok_or(error))
}

pub(super) fn aggregate_path(run_id: &str) -> Result<PathBuf> {
    Ok(run_dir(run_id)?.join("aggregate.json"))
}

pub(super) fn write_record(record: &AgentTaskRunRecord) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let metadata_json = observation_metadata(record, read_mirrored_aggregate(&record.run_id)?)?;
    store.upsert_imported_run(&RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: record.plan_id_component(),
        started_at: record.submitted_at.clone(),
        finished_at: terminal_finished_at(record),
        status: run_status(record.state).to_string(),
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        git_sha: None,
        rig_id: None,
        metadata_json,
    })
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

pub(super) fn record_exists(run_id: &str) -> Result<bool> {
    Ok(ObservationStore::open_initialized()?
        .get_run(run_id)?
        .is_some())
}

pub(super) fn read_records() -> Result<Vec<AgentTaskRunRecord>> {
    let store = ObservationStore::open_initialized()?;
    let filter = RunListFilter {
        kind: Some("agent-task".to_string()),
        limit: Some(1000),
        ..Default::default()
    };
    let mut records = Vec::new();
    for run in store.list_runs(filter)? {
        match record_from_run(&run) {
            Ok(record) => records.push(record),
            Err(error) => eprintln!(
                "Warning: skipping malformed agent-task run record {}: {}",
                run.id, error.message
            ),
        }
    }

    Ok(records)
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

fn record_from_run(run: &RunRecord) -> Result<AgentTaskRunRecord> {
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
    serde_json::from_value(value.clone()).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("parse agent-task run {}", run.id)),
        )
    })
}

fn mirror_aggregate(run_id: &str, aggregate: &AgentTaskAggregate) -> Result<()> {
    let record = match read_record(run_id) {
        Ok(record) => record,
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => return Ok(()),
        Err(error) => return Err(error),
    };
    let store = ObservationStore::open_initialized()?;
    let metadata_json = observation_metadata(&record, Some(aggregate.clone()))?;
    store.upsert_imported_run(&RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: record.plan_id_component(),
        started_at: record.submitted_at.clone(),
        finished_at: terminal_finished_at(&record),
        status: run_status(record.state).to_string(),
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        git_sha: None,
        rig_id: None,
        metadata_json,
    })
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
        AgentTaskRunState::PartialFailure | AgentTaskRunState::Failed => RunStatus::Fail.as_str(),
        AgentTaskRunState::Cancelled => RunStatus::Skipped.as_str(),
    }
}

fn terminal_finished_at(record: &AgentTaskRunRecord) -> Option<String> {
    match record.state {
        AgentTaskRunState::Succeeded
        | AgentTaskRunState::PartialFailure
        | AgentTaskRunState::Failed
        | AgentTaskRunState::Cancelled => record
            .updated_at
            .clone()
            .or_else(|| Some(record.submitted_at.clone())),
        AgentTaskRunState::Queued | AgentTaskRunState::Running => None,
    }
}

trait AgentTaskRunRecordExt {
    fn plan_id_component(&self) -> Option<String>;
}

impl AgentTaskRunRecordExt for AgentTaskRunRecord {
    fn plan_id_component(&self) -> Option<String> {
        self.metadata
            .get("repo")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                self.metadata
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let raw = fs::read_to_string(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))
}

fn write_json<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::internal_unexpected(format!("path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(error.to_string(), Some(parent.display().to_string()))
    })?;
    let json = serde_json::to_string_pretty(value).map_err(|error| {
        Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

fn run_dir(run_id: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("agent-task-runs")
        .join(sanitize_run_id(run_id)))
}

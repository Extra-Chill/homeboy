use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use super::{sanitize_run_id, AgentTaskRunRecord};
use crate::core::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use crate::core::{paths, Error, Result};

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
    Ok(path)
}

pub(super) fn read_aggregate(run_id: &str) -> Result<AgentTaskAggregate> {
    read_json(&aggregate_path(run_id)?)
}

pub(super) fn aggregate_path(run_id: &str) -> Result<PathBuf> {
    Ok(run_dir(run_id)?.join("aggregate.json"))
}

pub(super) fn write_record(record: &AgentTaskRunRecord) -> Result<()> {
    write_json(&record_path(&record.run_id)?, record)
}

pub(super) fn read_record(run_id: &str) -> Result<AgentTaskRunRecord> {
    read_json(&record_path(run_id)?)
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

fn record_path(run_id: &str) -> Result<PathBuf> {
    Ok(run_dir(run_id)?.join("status.json"))
}

fn run_dir(run_id: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("agent-task-runs")
        .join(sanitize_run_id(run_id)))
}

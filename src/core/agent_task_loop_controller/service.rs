use super::*;
use crate::core::{agent_task_lifecycle, paths, Error, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

pub fn controller_status_report(loop_id: &str) -> Result<AgentTaskLoopControllerStatusReport> {
    let controller = controller_status(loop_id)?;
    let diagnostics = controller_status_diagnostics(&controller)?;
    Ok(AgentTaskLoopControllerStatusReport {
        schema: AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA.to_string(),
        controller,
        diagnostics,
    })
}

pub fn controller_status_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Result<AgentTaskLoopControllerDiagnostics> {
    controller_status_diagnostics_with(record, Utc::now(), |run_id| {
        agent_task_lifecycle::run_record_exists(run_id)
    })
}

pub(crate) fn controller_status_diagnostics_with<F>(
    record: &AgentTaskLoopControllerRecord,
    now: DateTime<Utc>,
    mut run_exists: F,
) -> Result<AgentTaskLoopControllerDiagnostics>
where
    F: FnMut(&str) -> Result<bool>,
{
    let mut pending_actions = Vec::new();
    let mut stale_pending_action_count = 0;
    let mut orphaned_pending_action_count = 0;
    let acceptance_gates = acceptance_gate_diagnostics(record);
    let missing_acceptance_gate_count = acceptance_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskLoopAcceptanceGateStatus::Missing)
        .count();
    let failed_acceptance_gate_count = acceptance_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskLoopAcceptanceGateStatus::Failed)
        .count();

    for action in record
        .next_actions
        .iter()
        .filter(|action| action.status == AgentTaskLoopActionStatus::Pending)
    {
        let age_seconds = parse_timestamp(&action.created_at).map(|created_at| {
            now.signed_duration_since(created_at.with_timezone(&Utc))
                .num_seconds()
                .max(0)
        });
        let stale = age_seconds.is_some_and(|age| age >= STALE_PENDING_ACTION_SECONDS);
        let runner_id = action_runner_id(action, record);
        let referenced_run_id = action_referenced_run_id(action, record);
        let missing_referenced_run = if let Some(run_id) = referenced_run_id.as_deref() {
            !run_exists(run_id)?
        } else {
            false
        };
        let orphaned = missing_referenced_run;
        let mut problems = Vec::new();
        if stale {
            problems.push("pending action is older than stale threshold".to_string());
        }
        if missing_referenced_run {
            problems.push("referenced run record is missing".to_string());
        }
        let recovery_commands = if stale || orphaned {
            recovery_commands_for(record, action)
        } else {
            Vec::new()
        };

        if stale {
            stale_pending_action_count += 1;
        }
        if orphaned {
            orphaned_pending_action_count += 1;
        }
        pending_actions.push(AgentTaskLoopPendingActionDiagnostic {
            action_id: action.action_id.clone(),
            action: action_name(&action.action).to_string(),
            dedupe_key: action.dedupe_key.clone(),
            runner_id,
            referenced_run_id,
            created_at: action.created_at.clone(),
            age_seconds,
            stale,
            orphaned,
            problems,
            recovery_commands,
        });
    }

    Ok(AgentTaskLoopControllerDiagnostics {
        schema: "homeboy/agent-task-loop-controller-diagnostics/v1".to_string(),
        stale_pending_threshold_seconds: STALE_PENDING_ACTION_SECONDS,
        summary: AgentTaskLoopControllerDiagnosticSummary {
            pending_action_count: pending_actions.len(),
            stale_pending_action_count,
            orphaned_pending_action_count,
            acceptance_gate_count: acceptance_gates.len(),
            missing_acceptance_gate_count,
            failed_acceptance_gate_count,
        },
        pending_actions,
        acceptance_gates,
    })
}

fn acceptance_gate_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Vec<AgentTaskLoopAcceptanceGateDiagnostic> {
    let mut declared = BTreeSet::new();
    for action in &record.next_actions {
        if let AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } = &action.action
        {
            declared.insert((bundle_id.clone(), entity_id.clone()));
        }
    }
    for result in &record.gate_results {
        declared.insert((result.bundle_id.clone(), result.entity_id.clone()));
    }
    for bundle in &record.gate_bundles {
        if !declared
            .iter()
            .any(|(bundle_id, _)| bundle_id == &bundle.bundle_id)
        {
            declared.insert((bundle.bundle_id.clone(), None));
        }
    }

    declared
        .into_iter()
        .map(|(bundle_id, entity_id)| {
            let result = record
                .gate_results
                .iter()
                .rev()
                .find(|result| result.bundle_id == bundle_id && result.entity_id == entity_id);
            let status = match result.map(|result| result.status) {
                Some(AgentTaskGateBundleStatus::Passed) => {
                    AgentTaskLoopAcceptanceGateStatus::Satisfied
                }
                Some(AgentTaskGateBundleStatus::Failed) => {
                    AgentTaskLoopAcceptanceGateStatus::Failed
                }
                Some(AgentTaskGateBundleStatus::Warn) => AgentTaskLoopAcceptanceGateStatus::Warning,
                None => AgentTaskLoopAcceptanceGateStatus::Missing,
            };
            let problems = match status {
                AgentTaskLoopAcceptanceGateStatus::Missing => {
                    vec!["acceptance gate has no recorded result".to_string()]
                }
                AgentTaskLoopAcceptanceGateStatus::Failed => {
                    vec!["acceptance gate recorded a failed result".to_string()]
                }
                AgentTaskLoopAcceptanceGateStatus::Satisfied
                | AgentTaskLoopAcceptanceGateStatus::Warning => Vec::new(),
            };

            AgentTaskLoopAcceptanceGateDiagnostic {
                bundle_id,
                entity_id,
                status,
                result_id: result.map(|result| result.result_id.clone()),
                result_status: result.map(|result| result.status),
                recorded_at: result.map(|result| result.recorded_at.clone()),
                problems,
            }
        })
        .collect()
}

pub fn create_controller(
    loop_id: &str,
    phase: &str,
    config_version: &str,
) -> Result<AgentTaskLoopControllerRecord> {
    let record = AgentTaskLoopControllerRecord::new(loop_id, phase, config_version);
    write_controller(&record)?;
    Ok(record)
}

pub fn load_controller(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    read_json(&controller_path(&sanitize_loop_id(loop_id))?)
}

pub fn controller_status(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = load_controller(loop_id)?;
    let refreshed_child_runs = refresh_stale_running_child_actions(&mut record)?;
    let refreshed_subcontrollers = refresh_subcontroller_statuses(&mut record)?;
    if refreshed_child_runs || refreshed_subcontrollers {
        write_controller(&record)?;
    }
    Ok(record)
}

pub fn list_controllers() -> Result<Vec<AgentTaskLoopControllerRecord>> {
    let root = controllers_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(root.display().to_string()),
            ));
        }
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(error.to_string(), Some(root.display().to_string()))
        })?;
        let path = entry.path().join("controller.json");
        if path.exists() {
            records.push(read_json(&path)?);
        }
    }
    records.sort_by(|left: &AgentTaskLoopControllerRecord, right| left.loop_id.cmp(&right.loop_id));
    Ok(records)
}

pub fn write_controller(record: &AgentTaskLoopControllerRecord) -> Result<()> {
    write_json(&controller_path(&record.loop_id)?, record)
}

pub fn apply_external_event(
    loop_id: &str,
    event: AgentTaskLoopExternalEvent,
) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = load_controller(loop_id)?;
    record.apply_event(event);
    write_controller(&record)?;
    Ok(record)
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

pub fn controller_record_path(loop_id: &str) -> Result<PathBuf> {
    controller_path(loop_id)
}

fn controller_path(loop_id: &str) -> Result<PathBuf> {
    Ok(controllers_root()?
        .join(sanitize_loop_id(loop_id))
        .join("controller.json"))
}

fn controllers_root() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("agent-task-loops"))
}

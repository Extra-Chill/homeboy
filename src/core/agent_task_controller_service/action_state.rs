//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;
use crate::core::agent_task_loop_controller::action_entity_id;

pub(super) fn claim_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
) -> Result<AgentTaskLoopPolicyActionRecord> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    if action.status != AgentTaskLoopActionStatus::Pending {
        return Err(Error::validation_invalid_argument(
            "action_id",
            format!(
                "controller action '{}' is {:?}, not pending",
                action.action_id, action.status
            ),
            Some(action.action_id.clone()),
            None,
        ));
    }
    action.status = AgentTaskLoopActionStatus::Running;
    let action = action.clone();
    push_controller_history(
        record,
        "controller.action.claimed",
        None,
        serde_json::json!({ "action_id": action.action_id, "dedupe_key": action.dedupe_key }),
    );
    Ok(action)
}

pub(super) fn complete_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    execution: &Value,
    exit_code: i32,
) -> Result<()> {
    let action = record
        .next_actions
        .iter()
        .find(|action| action.action_id == action_id)
        .cloned();
    let status = if exit_code == 0 {
        AgentTaskLoopActionStatus::Completed
    } else {
        AgentTaskLoopActionStatus::Failed
    };
    set_controller_action_status(record, action_id, status)?;
    if let Some(action) = action {
        if let Some((status, reason, details)) =
            infer_terminal_outcome(&action, execution, exit_code)
        {
            record.record_terminal_outcome(
                status,
                reason,
                Some(action_id.to_string()),
                action_entity_id(&action.action),
                details,
            );
        }
    }
    push_controller_history(
        record,
        if exit_code == 0 {
            "controller.action.completed"
        } else {
            "controller.action.failed"
        },
        None,
        serde_json::json!({ "action_id": action_id, "exit_code": exit_code, "execution": execution }),
    );
    Ok(())
}

pub(super) fn fail_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    message: &str,
) -> Result<()> {
    set_controller_action_status(record, action_id, AgentTaskLoopActionStatus::Failed)?;
    record.record_terminal_outcome(
        AgentTaskLoopTerminalStatus::Failed,
        message.to_string(),
        Some(action_id.to_string()),
        action_entity_id_for_record(record, action_id),
        serde_json::json!({ "error": message }),
    );
    push_controller_history(
        record,
        "controller.action.failed",
        None,
        serde_json::json!({ "action_id": action_id, "error": message }),
    );
    Ok(())
}

pub(super) fn fail_controller_action_with_diagnostics(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    diagnostics: Vec<AgentTaskLoopActionDiagnostic>,
    execution: &Value,
) -> Result<()> {
    let message = diagnostics
        .first()
        .map(|diagnostic| diagnostic.message.clone())
        .unwrap_or_else(|| "controller action failed".to_string());
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    action.status = AgentTaskLoopActionStatus::Failed;
    action.reason = message.clone();
    action.diagnostics.extend(diagnostics.clone());
    let terminal_status = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "required_workflow_artifacts_missing")
    {
        AgentTaskLoopTerminalStatus::NeedsRevalidation
    } else {
        AgentTaskLoopTerminalStatus::Failed
    };
    let entity_id = action_entity_id(&action.action);
    record.record_terminal_outcome(
        terminal_status,
        message.clone(),
        Some(action_id.to_string()),
        entity_id,
        serde_json::json!({ "diagnostics": diagnostics.clone(), "execution": execution }),
    );
    push_controller_history(
        record,
        "controller.action.failed",
        None,
        serde_json::json!({
            "action_id": action_id,
            "error": message,
            "diagnostics": diagnostics,
            "execution": execution,
        }),
    );
    Ok(())
}

pub(super) fn infer_terminal_outcome(
    action: &AgentTaskLoopPolicyActionRecord,
    execution: &Value,
    exit_code: i32,
) -> Option<(AgentTaskLoopTerminalStatus, String, Value)> {
    match &action.action {
        AgentTaskLoopPolicyAction::FanOut { entity_ids, .. }
            if entity_ids.is_empty()
                && execution
                    .get("item_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    == 0 =>
        {
            Some((
                AgentTaskLoopTerminalStatus::NoActionableFindings,
                "fan-out completed with zero target entities".to_string(),
                serde_json::json!({ "mode": "fan_out", "item_count": 0 }),
            ))
        }
        AgentTaskLoopPolicyAction::FanOut { entity_ids, .. } if exit_code == 0 => Some((
            AgentTaskLoopTerminalStatus::Passed,
            format!(
                "fan-out completed for {} target entities",
                fan_out_item_count(entity_ids, execution)
            ),
            serde_json::json!({ "mode": "fan_out", "item_count": fan_out_item_count(entity_ids, execution), "execution": execution }),
        )),
        AgentTaskLoopPolicyAction::FanOut { entity_ids, .. } => Some((
            AgentTaskLoopTerminalStatus::Failed,
            format!(
                "fan-out failed for one or more of {} target entities",
                fan_out_item_count(entity_ids, execution)
            ),
            serde_json::json!({ "mode": "fan_out", "item_count": fan_out_item_count(entity_ids, execution), "execution": execution }),
        )),
        AgentTaskLoopPolicyAction::RunGates { .. } => {
            let result = execution.get("result")?;
            let status = gate_terminal_status(result, exit_code);
            Some((
                status,
                gate_terminal_reason(status),
                serde_json::json!({ "mode": "run_gates", "result": result }),
            ))
        }
        AgentTaskLoopPolicyAction::Complete { reason } if exit_code == 0 => Some((
            AgentTaskLoopTerminalStatus::Passed,
            reason
                .clone()
                .unwrap_or_else(|| "controller completed".to_string()),
            serde_json::json!({ "mode": "complete" }),
        )),
        _ if exit_code != 0 => Some((
            AgentTaskLoopTerminalStatus::Failed,
            "controller action failed".to_string(),
            serde_json::json!({ "execution": execution }),
        )),
        _ => None,
    }
}

fn fan_out_item_count(entity_ids: &[String], execution: &Value) -> usize {
    execution
        .get("item_count")
        .and_then(Value::as_u64)
        .map(|count| count as usize)
        .unwrap_or(entity_ids.len())
}

pub(super) fn gate_terminal_status(result: &Value, exit_code: i32) -> AgentTaskLoopTerminalStatus {
    if exit_code == 0 {
        return match result.get("status").and_then(Value::as_str) {
            Some("warn") => AgentTaskLoopTerminalStatus::NoPublication,
            _ => AgentTaskLoopTerminalStatus::Passed,
        };
    }
    let needs_upstream_fix = result
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|check| check.get("classification").and_then(Value::as_str))
        .any(|classification| classification == "needs_upstream_fix");
    if needs_upstream_fix {
        AgentTaskLoopTerminalStatus::NeedsUpstreamFix
    } else {
        AgentTaskLoopTerminalStatus::BlockedByGate
    }
}

pub(super) fn gate_terminal_reason(status: AgentTaskLoopTerminalStatus) -> String {
    match status {
        AgentTaskLoopTerminalStatus::Passed => "gate bundle passed".to_string(),
        AgentTaskLoopTerminalStatus::NoPublication => {
            "gate bundle returned warnings; no publication performed".to_string()
        }
        AgentTaskLoopTerminalStatus::NeedsUpstreamFix => {
            "gate bundle identified an upstream fix requirement".to_string()
        }
        _ => "gate bundle blocked the loop".to_string(),
    }
}

pub(super) fn action_entity_id_for_record(
    record: &AgentTaskLoopControllerRecord,
    action_id: &str,
) -> Option<String> {
    record
        .next_actions
        .iter()
        .find(|action| action.action_id == action_id)
        .and_then(|action| action_entity_id(&action.action))
}

pub(super) fn set_controller_action_status(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    status: AgentTaskLoopActionStatus,
) -> Result<()> {
    let action = record
        .next_actions
        .iter_mut()
        .find(|action| action.action_id == action_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "action_id",
                format!("controller action '{action_id}' does not exist"),
                Some(action_id.to_string()),
                None,
            )
        })?;
    action.status = status;
    Ok(())
}

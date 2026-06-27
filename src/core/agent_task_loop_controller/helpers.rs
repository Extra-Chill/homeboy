use super::*;
use crate::core::agent_task_lifecycle::AgentTaskRunState;
use crate::core::{agent_task_lifecycle, Result};
use chrono::DateTime;
use serde_json::{json, Value};

pub(crate) fn action_dedupe_key(action: &AgentTaskLoopPolicyAction) -> Option<String> {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::RunCommand { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::FanOut { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::SpawnController { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::SpawnSubloop { dedupe_key, .. }
        | AgentTaskLoopPolicyAction::RouteFinding { dedupe_key, .. } => Some(dedupe_key.clone()),
        AgentTaskLoopPolicyAction::ValidateCandidatePatch {
            candidate,
            validation,
            ..
        } => Some(format!(
            "candidate-validation:{}:{}",
            candidate.candidate_id, validation.validation_id
        )),
        AgentTaskLoopPolicyAction::WaitForEvent(wait) => Some(format!("wait:{}", wait.wait_key)),
        AgentTaskLoopPolicyAction::WaitForController {
            loop_id, wait_key, ..
        } => Some(format!(
            "wait:{}",
            wait_key
                .clone()
                .unwrap_or_else(|| controller_wait_key(loop_id))
        )),
        AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } => entity_id
            .as_ref()
            .map(|entity_id| format!("gate:{bundle_id}:{entity_id}")),
        AgentTaskLoopPolicyAction::OwnPrUntilGreen { ownership, .. } => {
            Some(format!("pr-ownership:{}", ownership.ownership_id))
        }
        AgentTaskLoopPolicyAction::RequestChanges {
            target_run_id,
            feedback_id,
        } => Some(format!(
            "feedback:{}:{}",
            target_run_id,
            feedback_id.as_deref().unwrap_or("latest")
        )),
        _ => None,
    }
}

pub(crate) fn jsonpath_match_is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_i64() != Some(0) || value.as_u64().is_some_and(|n| n > 0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

pub(crate) fn action_entity_id(action: &AgentTaskLoopPolicyAction) -> Option<String> {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { entity_id, .. }
        | AgentTaskLoopPolicyAction::RunCommand { entity_id, .. }
        | AgentTaskLoopPolicyAction::SpawnController { entity_id, .. }
        | AgentTaskLoopPolicyAction::SpawnSubloop { entity_id, .. }
        | AgentTaskLoopPolicyAction::WaitForController { entity_id, .. }
        | AgentTaskLoopPolicyAction::RouteFinding { entity_id, .. }
        | AgentTaskLoopPolicyAction::RunGates { entity_id, .. }
        | AgentTaskLoopPolicyAction::OwnPrUntilGreen { entity_id, .. } => entity_id.clone(),
        AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, .. } => Some(entity_id.clone()),
        _ => None,
    }
}

pub(crate) fn action_name(action: &AgentTaskLoopPolicyAction) -> &'static str {
    match action {
        AgentTaskLoopPolicyAction::SpawnTask { .. } => "spawn_task",
        AgentTaskLoopPolicyAction::RunCommand { .. } => "run_command",
        AgentTaskLoopPolicyAction::FanOut { .. } => "fan_out",
        AgentTaskLoopPolicyAction::SpawnController { .. } => "spawn_controller",
        AgentTaskLoopPolicyAction::SpawnSubloop { .. } => "spawn_subloop",
        AgentTaskLoopPolicyAction::RouteFinding { .. } => "route_finding",
        AgentTaskLoopPolicyAction::ValidateCandidatePatch { .. } => "validate_candidate_patch",
        AgentTaskLoopPolicyAction::Join { .. } => "join",
        AgentTaskLoopPolicyAction::Retry { .. } => "retry",
        AgentTaskLoopPolicyAction::RequestChanges { .. } => "request_changes",
        AgentTaskLoopPolicyAction::RunGates { .. } => "run_gates",
        AgentTaskLoopPolicyAction::OwnPrUntilGreen { .. } => "own_pr_until_green",
        AgentTaskLoopPolicyAction::WaitForEvent(_) => "wait_for_event",
        AgentTaskLoopPolicyAction::WaitForController { .. } => "wait_for_controller",
        AgentTaskLoopPolicyAction::MarkHumanReady { .. } => "mark_human_ready",
        AgentTaskLoopPolicyAction::Complete { .. } => "complete",
        AgentTaskLoopPolicyAction::Abandon { .. } => "abandon",
        AgentTaskLoopPolicyAction::Escalate { .. } => "escalate",
    }
}

pub(crate) fn action_runner_id(
    action: &AgentTaskLoopPolicyActionRecord,
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    action_value(action)
        .as_ref()
        .and_then(|value| first_string_at_keys(value, &["runner_id", "runner", "lab_runner_id"]))
        .or_else(|| {
            first_string_at_keys(
                &record.metadata,
                &["runner_id", "runner", "lab_runner_id", "configured_runner"],
            )
        })
}

pub(crate) fn action_referenced_run_id(
    action: &AgentTaskLoopPolicyActionRecord,
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    match &action.action {
        AgentTaskLoopPolicyAction::Retry { target_run_id }
        | AgentTaskLoopPolicyAction::RequestChanges { target_run_id, .. } => {
            Some(target_run_id.clone())
        }
        _ => action_value(action)
            .as_ref()
            .and_then(|value| {
                first_string_at_keys(
                    value,
                    &[
                        "referenced_run_id",
                        "target_run_id",
                        "remote_run_id",
                        "run_id",
                    ],
                )
            })
            .or_else(|| referenced_run_id_from_dedupe(action, record)),
    }
}

pub(crate) fn referenced_run_id_from_dedupe(
    action: &AgentTaskLoopPolicyActionRecord,
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    let dedupe_key = action.dedupe_key.as_ref()?;
    record
        .dedupe_keys
        .get(dedupe_key)
        .and_then(|dedupe| dedupe.run_id.clone())
}

pub(crate) fn action_value(action: &AgentTaskLoopPolicyActionRecord) -> Option<Value> {
    serde_json::to_value(&action.action).ok()
}

pub(crate) fn first_string_at_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    if !value.trim().is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
            map.values()
                .find_map(|value| first_string_at_keys(value, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| first_string_at_keys(value, keys)),
        _ => None,
    }
}

pub(crate) fn parse_timestamp(value: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}

pub(crate) fn recovery_commands_for(
    record: &AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
) -> Vec<String> {
    let loop_id = shell_arg(&record.loop_id);
    let dispatch_flags = recovery_dispatch_flags(record, action);
    vec![
        format!("homeboy agent-task controller run {loop_id}{dispatch_flags}"),
        format!("homeboy agent-task controller resume {loop_id}{dispatch_flags}"),
    ]
}

pub(crate) fn recovery_dispatch_flags(
    record: &AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
) -> String {
    let action_value = action_value(action);
    let mut flags = Vec::new();
    if let Some(value) = first_dispatch_backend(action_value.as_ref(), &record.metadata) {
        flags.push(format!("--dispatch-backend {}", shell_arg(&value)));
    }
    if let Some(value) = first_dispatch_selector(action_value.as_ref(), &record.metadata) {
        flags.push(format!("--dispatch-selector {}", shell_arg(&value)));
    }
    if let Some(value) = first_dispatch_model(action_value.as_ref(), &record.metadata) {
        flags.push(format!("--dispatch-model {}", shell_arg(&value)));
    }
    if flags.is_empty() {
        String::new()
    } else {
        format!(" {}", flags.join(" "))
    }
}

pub(crate) fn first_dispatch_backend(action: Option<&Value>, metadata: &Value) -> Option<String> {
    action
        .and_then(|value| first_string_at_keys(value, &["dispatch_backend", "backend"]))
        .or_else(|| first_string_at_keys(metadata, &["dispatch_backend", "backend"]))
}

pub(crate) fn first_dispatch_selector(action: Option<&Value>, metadata: &Value) -> Option<String> {
    action
        .and_then(first_provider_selector)
        .or_else(|| first_provider_selector(metadata))
}

pub(crate) fn first_dispatch_model(action: Option<&Value>, metadata: &Value) -> Option<String> {
    action
        .and_then(|value| first_string_at_keys(value, &["dispatch_model", "model"]))
        .or_else(|| first_string_at_keys(metadata, &["dispatch_model", "model"]))
}

pub(crate) fn first_provider_selector(value: &Value) -> Option<String> {
    first_string_at_keys(
        value,
        &[
            "dispatch_selector",
            "provider_selector",
            "provider_id",
            "provider",
        ],
    )
    .or_else(|| first_executor_string_at_keys(value, &["selector", "provider_id", "provider"]))
}

pub(crate) fn first_executor_string_at_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(executor) = map.get("executor") {
                if let Some(value) = first_string_at_keys(executor, keys) {
                    return Some(value);
                }
            }
            if let Some(dispatch) = map.get("dispatch") {
                if let Some(value) = first_executor_string_at_keys(dispatch, keys) {
                    return Some(value);
                }
            }
            map.values()
                .find_map(|value| first_executor_string_at_keys(value, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| first_executor_string_at_keys(value, keys)),
        _ => None,
    }
}

pub(crate) fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(crate) fn refresh_subcontroller_statuses(
    record: &mut AgentTaskLoopControllerRecord,
) -> Result<bool> {
    let mut changed = false;
    let mut satisfied_waits = Vec::new();
    for subcontroller in &mut record.subcontrollers {
        let Ok(child) = load_controller(&subcontroller.loop_id) else {
            continue;
        };
        if subcontroller.state != Some(child.state) {
            subcontroller.state = Some(child.state);
            subcontroller.updated_at = now_timestamp();
            changed = true;
        }
        let terminal_states = controller_terminal_states(&subcontroller.terminal_states);
        if terminal_states.contains(&child.state) {
            if let Some(wait_key) = &subcontroller.wait_key {
                satisfied_waits.push((wait_key.clone(), child.loop_id.clone(), child.state));
            }
        }
    }

    for (wait_key, child_loop_id, child_state) in satisfied_waits {
        if let Some(wait) = record
            .waits
            .iter_mut()
            .find(|wait| wait.wait_key == wait_key && wait.status == AgentTaskLoopWaitStatus::Open)
        {
            wait.status = AgentTaskLoopWaitStatus::Satisfied;
            wait.satisfied_by_event_id = Some(format!(
                "controller-terminal:{child_loop_id}:{child_state:?}"
            ));
            changed = true;
        }
    }

    if record.open_wait_count() == 0 && record.state == AgentTaskLoopControllerState::Waiting {
        record.state = AgentTaskLoopControllerState::Running;
        changed = true;
    }

    if changed {
        record.touch();
    }
    Ok(changed)
}

pub(crate) fn refresh_stale_running_child_actions(
    record: &mut AgentTaskLoopControllerRecord,
) -> Result<bool> {
    let mut changed = false;
    let mut history_events = Vec::new();

    for index in 0..record.next_actions.len() {
        let action = &record.next_actions[index];
        if action.status != AgentTaskLoopActionStatus::Running
            || !matches!(action.action, AgentTaskLoopPolicyAction::SpawnTask { .. })
        {
            continue;
        }

        let Some(run_id) = action_referenced_run_id(action, record) else {
            continue;
        };
        let run = agent_task_lifecycle::status(&run_id)?;
        if run.state != AgentTaskRunState::Running
            || run.metadata.get("stale_running").and_then(Value::as_bool) != Some(true)
        {
            continue;
        }

        let reason = run
            .metadata
            .get("stale_running_reason")
            .and_then(Value::as_str)
            .unwrap_or("stale_running");
        let action = &mut record.next_actions[index];
        action.status = AgentTaskLoopActionStatus::Pending;
        action.reason = format!(
            "child agent-task run '{run_id}' is stale ({reason}); action reset for recovery"
        );
        action.diagnostics.push(AgentTaskLoopActionDiagnostic {
            code: "stale_child_run_recovery".to_string(),
            message: action.reason.clone(),
            runner: None,
            details: json!({
                "run_id": run_id,
                "stale_running_reason": reason,
            }),
        });
        history_events.push((
            action.action_id.clone(),
            action.dedupe_key.clone(),
            run_id,
            reason.to_string(),
        ));
        changed = true;
    }

    for (action_id, dedupe_key, run_id, reason) in history_events {
        record.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("stale-child-recovery-{}", record.history.len() + 1),
            event_type: "controller.action.stale_child_recovery".to_string(),
            recorded_at: now_timestamp(),
            entity_id: None,
            payload: json!({
                "action_id": action_id,
                "dedupe_key": dedupe_key,
                "run_id": run_id,
                "stale_running_reason": reason,
            }),
        });
    }

    if changed {
        record.touch();
    }
    Ok(changed)
}

pub(crate) fn controller_wait_key(loop_id: &str) -> String {
    format!("controller:{}:terminal", sanitize_loop_id(loop_id))
}

pub(crate) fn controller_terminal_states(
    states: &[AgentTaskLoopControllerState],
) -> Vec<AgentTaskLoopControllerState> {
    if states.is_empty() {
        vec![
            AgentTaskLoopControllerState::Completed,
            AgentTaskLoopControllerState::Failed,
            AgentTaskLoopControllerState::HumanReady,
            AgentTaskLoopControllerState::Abandoned,
            AgentTaskLoopControllerState::Escalated,
        ]
    } else {
        states.to_vec()
    }
}

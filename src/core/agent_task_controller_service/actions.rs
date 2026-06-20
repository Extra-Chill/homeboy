//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;

pub(super) fn execute_controller_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action_id: &str,
    executor: E,
    dispatch: &D,
) -> Result<AgentTaskRunResult<ControllerActionReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let action = claim_controller_action(record, action_id)?;
    controller::write_controller(record)?;

    match execute_claimed_controller_action(record, &action, executor, dispatch) {
        Ok((execution, exit_code)) => {
            let mut exit_code = exit_code;
            let missing_required_artifacts = if exit_code == 0 {
                validate_required_action_artifacts(&action, &execution)
            } else {
                Vec::new()
            };
            if missing_required_artifacts.is_empty() {
                complete_controller_action(record, action_id, &execution, exit_code)?;
            } else {
                exit_code = 1;
                fail_controller_action_with_diagnostics(
                    record,
                    action_id,
                    required_artifact_diagnostics(&missing_required_artifacts),
                    &execution,
                )?;
            }
            controller::write_controller(record)?;
            Ok(AgentTaskRunResult {
                value: ControllerActionReport {
                    schema: ACTION_RESULT_SCHEMA,
                    loop_id: record.loop_id.clone(),
                    claimed: true,
                    action_id: Some(action_id.to_string()),
                    status: Some(
                        if exit_code == 0 {
                            "completed"
                        } else {
                            "failed"
                        }
                        .to_string(),
                    ),
                    execution: Some(execution),
                    controller: record.clone(),
                },
                exit_code,
            })
        }
        Err(error) => {
            fail_controller_action(record, action_id, &error.to_string())?;
            controller::write_controller(record)?;
            Err(error)
        }
    }
}

pub(super) fn execute_claimed_controller_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    match &action.action {
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key,
            entity_id,
            request,
        } => execute_spawn_task_action(
            record,
            action,
            dedupe_key,
            entity_id.as_deref(),
            request,
            executor,
            dispatch,
        ),
        AgentTaskLoopPolicyAction::FanOut {
            dedupe_key,
            entity_ids,
            request_template,
        } => execute_fan_out_action(
            record,
            action,
            dedupe_key,
            entity_ids,
            request_template,
            executor,
            dispatch,
        ),
        AgentTaskLoopPolicyAction::SpawnController {
            dedupe_key,
            loop_id,
            entity_id,
            phase,
            config_version,
            request,
        } => execute_spawn_controller_action(
            record,
            action,
            "spawn_controller",
            dedupe_key,
            loop_id,
            entity_id.as_deref(),
            phase,
            config_version,
            request,
        ),
        AgentTaskLoopPolicyAction::SpawnSubloop {
            dedupe_key,
            loop_id,
            entity_id,
            phase,
            config_version,
            request,
        } => execute_spawn_controller_action(
            record,
            action,
            "spawn_subloop",
            dedupe_key,
            loop_id,
            entity_id.as_deref(),
            phase,
            config_version,
            request,
        ),
        AgentTaskLoopPolicyAction::Join { wait_key } => Ok((
            serde_json::json!({ "mode": "join", "wait_key": wait_key }),
            0,
        )),
        AgentTaskLoopPolicyAction::WaitForEvent(wait) => Ok((
            serde_json::json!({ "mode": "wait_for_event", "wait_key": wait.wait_key }),
            0,
        )),
        AgentTaskLoopPolicyAction::WaitForController {
            loop_id,
            entity_id,
            wait_key,
            terminal_states,
        } => execute_wait_for_controller_action(
            record,
            loop_id,
            entity_id.as_deref(),
            wait_key.as_deref(),
            terminal_states,
        ),
        AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } => execute_run_gates_action(record, bundle_id, entity_id.as_deref()),
        AgentTaskLoopPolicyAction::OwnPrUntilGreen {
            ownership,
            entity_id,
        } => execute_own_pr_until_green_action(record, ownership, entity_id.as_deref()),
        AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, reason } => {
            record.mark_human_ready(entity_id, reason.clone())?;
            Ok((
                serde_json::json!({ "mode": "mark_human_ready", "entity_id": entity_id }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::Complete { reason } => {
            record.state = AgentTaskLoopControllerState::Completed;
            Ok((
                serde_json::json!({ "mode": "complete", "reason": reason }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::Abandon { reason } => {
            record.state = AgentTaskLoopControllerState::Abandoned;
            Ok((
                serde_json::json!({ "mode": "abandon", "reason": reason }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::Escalate { reason } => {
            record.state = AgentTaskLoopControllerState::Escalated;
            Ok((
                serde_json::json!({ "mode": "escalate", "reason": reason }),
                0,
            ))
        }
        AgentTaskLoopPolicyAction::RouteFinding {
            finding,
            dedupe_key,
            entity_id,
            request_template,
        } => execute_route_finding_action(
            record,
            action,
            finding,
            dedupe_key,
            entity_id.as_deref(),
            request_template,
            executor,
            dispatch,
        ),
        AgentTaskLoopPolicyAction::Retry { target_run_id } => {
            execute_retry_action(record, action, target_run_id)
        }
        AgentTaskLoopPolicyAction::RequestChanges {
            target_run_id,
            feedback_id,
        } => execute_request_changes_action(record, action, target_run_id, feedback_id.as_deref()),
        AgentTaskLoopPolicyAction::ValidateCandidatePatch { .. } => {
            Err(Error::validation_invalid_argument(
                "action_id",
                format!(
                    "controller action '{}' is not executable by the generic controller runner yet",
                    action.action_id
                ),
                Some(action.action_id.clone()),
                None,
            ))
        }
    }
}

pub(super) fn execute_retry_action(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    target_run_id: &str,
) -> Result<(Value, i32)> {
    let retry = agent_task_service::retry(target_run_id, None, false)?;
    let retry_run_id = retry.record.run_id.clone();
    if !record
        .task_lineage
        .iter()
        .any(|lineage| lineage.run_id == retry_run_id)
    {
        record.task_lineage.push(AgentTaskLoopTaskLineage {
            run_id: retry_run_id.clone(),
            task_id: None,
            parent_run_id: Some(target_run_id.to_string()),
            parent_task_id: None,
            entity_id: None,
            dedupe_key: action.dedupe_key.clone(),
            artifact_refs: Vec::new(),
            inputs: serde_json::json!({ "target_run_id": target_run_id }),
            outputs: Value::Null,
        });
    }
    push_controller_history(
        record,
        "controller.action.retry_queued",
        None,
        serde_json::json!({
            "action_id": action.action_id,
            "target_run_id": target_run_id,
            "retry_run_id": retry_run_id,
        }),
    );
    Ok((
        serde_json::json!({
            "mode": "retry",
            "target_run_id": target_run_id,
            "retry_run_id": retry.record.run_id,
            "record": retry.record,
            "run": retry.run,
        }),
        0,
    ))
}

pub(super) fn execute_request_changes_action(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    target_run_id: &str,
    feedback_id: Option<&str>,
) -> Result<(Value, i32)> {
    let feedback_id = feedback_id
        .map(str::to_string)
        .unwrap_or_else(|| format!("feedback-{}", record.feedback.len() + 1));
    let feedback = controller::AgentTaskLoopFeedbackArtifact {
        feedback_id: feedback_id.clone(),
        review_run_id: None,
        target_run_id: Some(target_run_id.to_string()),
        target_task_id: None,
        target_entity_id: None,
        status: controller::AgentTaskLoopFeedbackStatus::ChangesRequested,
        findings: Vec::new(),
        payload: serde_json::json!({
            "action_id": action.action_id,
            "target_run_id": target_run_id,
        }),
    };
    if let Some(existing) = record
        .feedback
        .iter_mut()
        .find(|existing| existing.feedback_id == feedback.feedback_id)
    {
        *existing = feedback.clone();
    } else {
        record.feedback.push(feedback.clone());
    }
    push_controller_history(
        record,
        "controller.action.changes_requested",
        None,
        serde_json::json!({
            "action_id": action.action_id,
            "target_run_id": target_run_id,
            "feedback_id": feedback_id,
        }),
    );
    Ok((
        serde_json::json!({
            "mode": "request_changes",
            "target_run_id": target_run_id,
            "feedback": feedback,
        }),
        0,
    ))
}

pub(super) fn execute_spawn_task_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    request: &Value,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let request = request_with_required_workflow_artifacts(record, request);
    let request = &request;
    let mode = request
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("run_plan");
    match mode {
        "run_plan" => {
            let plan = plan_from_controller_request(request)?;
            let run_id = controller_request_run_id(request, dedupe_key, &action.action_id);
            let submitted = if lifecycle::run_record_exists(&run_id)? {
                lifecycle::status(&run_id)?
            } else {
                lifecycle::submit_plan(&plan, Some(&run_id))?
            };
            record_controller_spawn(
                record,
                action,
                dedupe_key,
                entity_id,
                &submitted.run_id,
                request,
            )?;
            let run_result = agent_task_service::run_submitted(submitted.run_id.clone(), executor)?;
            record_controller_aggregate_evidence(
                record,
                entity_id,
                &submitted.run_id,
                &run_result.value,
            )?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                execution_with_request_workflow_artifacts(
                    serde_json::json!({
                        "mode": mode,
                        "run_id": submitted.run_id,
                        "submitted": submitted,
                        "aggregate": aggregate_value,
                    }),
                    request,
                ),
                run_result.exit_code,
            ))
        }
        "submit" => {
            let plan = plan_from_controller_request(request)?;
            let run_id = controller_request_run_id(request, dedupe_key, &action.action_id);
            let submitted = lifecycle::submit_plan(&plan, Some(&run_id))?;
            record_controller_spawn(
                record,
                action,
                dedupe_key,
                entity_id,
                &submitted.run_id,
                request,
            )?;
            Ok((
                execution_with_request_workflow_artifacts(
                    serde_json::json!({
                        "mode": mode,
                        "run_id": submitted.run_id,
                        "submitted": submitted,
                    }),
                    request,
                ),
                0,
            ))
        }
        "run" => {
            let run_id = required_string(request, "run_id")?;
            record_controller_spawn(record, action, dedupe_key, entity_id, &run_id, request)?;
            let run_result = agent_task_service::run_submitted(run_id.clone(), executor)?;
            record_controller_aggregate_evidence(record, entity_id, &run_id, &run_result.value)?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                execution_with_request_workflow_artifacts(
                    serde_json::json!({ "mode": mode, "run_id": run_id, "aggregate": aggregate_value }),
                    request,
                ),
                run_result.exit_code,
            ))
        }
        "resume" => {
            let run_id = required_string(request, "run_id")?;
            record_controller_spawn(record, action, dedupe_key, entity_id, &run_id, request)?;
            let run_result = agent_task_service::resume(run_id.clone(), executor)?;
            record_controller_aggregate_evidence(record, entity_id, &run_id, &run_result.value)?;
            let aggregate_value = serde_json::to_value(&run_result.value)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            Ok((
                execution_with_request_workflow_artifacts(
                    serde_json::json!({ "mode": mode, "run_id": run_id, "aggregate": aggregate_value }),
                    request,
                ),
                run_result.exit_code,
            ))
        }
        "run_next" => {
            let run_result = agent_task_service::run_next(executor)?;
            let value = match run_result.value {
                Some(aggregate) => serde_json::to_value(&aggregate)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
                None => serde_json::json!({ "claimed": false }),
            };
            if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
                record_controller_spawn(record, action, dedupe_key, entity_id, run_id, request)?;
            }
            Ok((
                execution_with_request_workflow_artifacts(
                    serde_json::json!({ "mode": mode, "result": value }),
                    request,
                ),
                run_result.exit_code,
            ))
        }
        "dispatch" => {
            let (value, exit_code) = dispatch.dispatch(request)?;
            if let Some(run_id) = value.get("run_id").and_then(Value::as_str) {
                record_controller_spawn(record, action, dedupe_key, entity_id, run_id, request)?;
                record_controller_result_evidence(record, entity_id, run_id, &value)?;
            }
            Ok((
                execution_with_request_workflow_artifacts(
                    serde_json::json!({ "mode": mode, "result": value }),
                    request,
                ),
                exit_code,
            ))
        }
        other => Err(Error::validation_invalid_argument(
            "request.mode",
            format!("unsupported spawn_task request mode '{other}'"),
            Some(other.to_string()),
            Some(vec![
                "Supported modes: run_plan, submit, run, resume, run_next, dispatch".to_string(),
            ]),
        )),
    }
}

pub(super) fn execute_fan_out_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_ids: &[String],
    request_template: &Value,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    if entity_ids.is_empty() {
        return Ok((
            serde_json::json!({ "mode": "fan_out", "item_count": 0, "results": [] }),
            0,
        ));
    }

    let mut results = Vec::new();
    let mut exit_code = 0;
    for entity_id in entity_ids {
        let request = materialize_fan_out_request(request_template, entity_id);
        let child_dedupe_key = format!("{dedupe_key}:{entity_id}");
        let child_action = AgentTaskLoopPolicyActionRecord {
            action_id: format!("{}:{entity_id}", action.action_id),
            action: AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: child_dedupe_key.clone(),
                entity_id: Some(entity_id.clone()),
                request: request.clone(),
            },
            status: AgentTaskLoopActionStatus::Running,
            reason: action.reason.clone(),
            created_at: action.created_at.clone(),
            dedupe_key: Some(child_dedupe_key.clone()),
            diagnostics: Vec::new(),
        };
        let (result, child_exit_code) = execute_spawn_task_action(
            record,
            &child_action,
            &child_dedupe_key,
            Some(entity_id),
            &request,
            executor.clone(),
            dispatch,
        )?;
        if child_exit_code != 0 {
            exit_code = child_exit_code;
        }
        results.push(result);
    }
    Ok((
        serde_json::json!({ "mode": "fan_out", "item_count": entity_ids.len(), "results": results }),
        exit_code,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_spawn_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    mode: &str,
    dedupe_key: &str,
    loop_id: &str,
    entity_id: Option<&str>,
    phase: &str,
    config_version: &str,
    request: &Value,
) -> Result<(Value, i32)> {
    let mut created = false;
    let mut child = match controller::load_controller(loop_id) {
        Ok(child) => child,
        Err(_) => {
            created = true;
            let mut child = AgentTaskLoopControllerRecord::new(loop_id, phase, config_version);
            child.parent_loop_id = Some(record.loop_id.clone());
            child.parent_action_id = Some(action.action_id.clone());
            child.parent_entity_id = entity_id.map(str::to_string);
            child.metadata = request.clone();
            child
        }
    };
    let mut child_changed = false;
    if child.parent_loop_id.is_none() {
        child.parent_loop_id = Some(record.loop_id.clone());
        child_changed = true;
    }
    if child.parent_action_id.is_none() {
        child.parent_action_id = Some(action.action_id.clone());
        child_changed = true;
    }
    if child.parent_entity_id.is_none() {
        child.parent_entity_id = entity_id.map(str::to_string);
        child_changed = child.parent_entity_id.is_some();
    }
    if created || child_changed {
        controller::write_controller(&child)?;
    }
    push_controller_history(
        record,
        "controller.action.spawned_controller",
        entity_id.map(str::to_string),
        serde_json::json!({
            "action_id": action.action_id,
            "dedupe_key": dedupe_key,
            "loop_id": child.loop_id,
            "mode": mode,
            "created": created,
        }),
    );
    Ok((
        serde_json::json!({
            "mode": mode,
            "loop_id": child.loop_id,
            "phase": child.phase,
            "config_version": child.config_version,
            "created": created,
        }),
        0,
    ))
}

pub(super) fn execute_wait_for_controller_action(
    record: &mut AgentTaskLoopControllerRecord,
    loop_id: &str,
    entity_id: Option<&str>,
    wait_key: Option<&str>,
    terminal_states: &[AgentTaskLoopControllerState],
) -> Result<(Value, i32)> {
    let wait_key = wait_key
        .map(str::to_string)
        .unwrap_or_else(|| format!("controller:{}:terminal", loop_id.replace(['/', ':'], "_")));
    let terminal_states = if terminal_states.is_empty() {
        vec![
            AgentTaskLoopControllerState::Completed,
            AgentTaskLoopControllerState::Failed,
            AgentTaskLoopControllerState::HumanReady,
            AgentTaskLoopControllerState::Abandoned,
            AgentTaskLoopControllerState::Escalated,
        ]
    } else {
        terminal_states.to_vec()
    };
    let child_state = controller::controller_status(loop_id)
        .ok()
        .map(|child| child.state);
    let satisfied = child_state.is_some_and(|state| terminal_states.contains(&state));
    if satisfied {
        for wait in &mut record.waits {
            if wait.wait_key == wait_key && wait.status == controller::AgentTaskLoopWaitStatus::Open
            {
                wait.status = controller::AgentTaskLoopWaitStatus::Satisfied;
                wait.satisfied_by_event_id =
                    child_state.map(|state| format!("controller-terminal:{loop_id}:{state:?}"));
            }
        }
        if record.state == AgentTaskLoopControllerState::Waiting {
            record.state = AgentTaskLoopControllerState::Running;
        }
    } else {
        record.state = AgentTaskLoopControllerState::Waiting;
    }
    Ok((
        serde_json::json!({
            "mode": "wait_for_controller",
            "loop_id": loop_id,
            "entity_id": entity_id,
            "wait_key": wait_key,
            "terminal_states": terminal_states,
            "child_state": child_state,
            "satisfied": satisfied,
        }),
        0,
    ))
}

pub(super) fn execute_run_gates_action(
    record: &mut AgentTaskLoopControllerRecord,
    bundle_id: &str,
    entity_id: Option<&str>,
) -> Result<(Value, i32)> {
    if let Some(existing) = record
        .gate_results
        .iter()
        .rev()
        .find(|result| result.bundle_id == bundle_id && result.entity_id.as_deref() == entity_id)
        .cloned()
    {
        let exit_code = if existing.status == AgentTaskGateBundleStatus::Failed {
            1
        } else {
            0
        };
        return Ok((
            serde_json::json!({ "mode": "run_gates", "bundle_id": bundle_id, "entity_id": entity_id, "result": existing }),
            exit_code,
        ));
    }

    let bundle = record
        .gate_bundles
        .iter()
        .find(|bundle| bundle.bundle_id == bundle_id)
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "bundle_id",
                format!("gate bundle '{bundle_id}' does not exist"),
                Some(bundle_id.to_string()),
                None,
            )
        })?;

    let mut checks = Vec::new();
    for check in &bundle.checks {
        let result = match check.kind {
            AgentTaskGateBundleCheckKind::Command => run_command_gate_check(check)?,
            AgentTaskGateBundleCheckKind::Manual => AgentTaskGateCheckResult {
                check_id: check.check_id.clone(),
                status: AgentTaskGateBundleStatus::Warn,
                retryable: check.retryable,
                classification: Some("manual_gate_requires_external_result".to_string()),
                evidence: Vec::new(),
                details: serde_json::json!({ "input": check.input }),
            },
            AgentTaskGateBundleCheckKind::Api | AgentTaskGateBundleCheckKind::Tool => {
                AgentTaskGateCheckResult {
                    check_id: check.check_id.clone(),
                    status: AgentTaskGateBundleStatus::Failed,
                    retryable: check.retryable,
                    classification: Some("unsupported_generic_gate_kind".to_string()),
                    evidence: Vec::new(),
                    details: serde_json::json!({ "kind": check.kind, "input": check.input }),
                }
            }
        };
        checks.push(result);
    }

    let status = if checks
        .iter()
        .any(|check| check.status == AgentTaskGateBundleStatus::Failed)
    {
        AgentTaskGateBundleStatus::Failed
    } else if checks
        .iter()
        .any(|check| check.status == AgentTaskGateBundleStatus::Warn)
    {
        AgentTaskGateBundleStatus::Warn
    } else {
        AgentTaskGateBundleStatus::Passed
    };
    let result = AgentTaskGateBundleResult {
        result_id: format!("gate-result-{}", record.gate_results.len() + 1),
        bundle_id: bundle_id.to_string(),
        entity_id: entity_id.map(str::to_string),
        run_id: None,
        status,
        checks,
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    record.gate_results.push(result.clone());
    let exit_code = if status == AgentTaskGateBundleStatus::Failed {
        1
    } else {
        0
    };
    Ok((
        serde_json::json!({ "mode": "run_gates", "bundle_id": bundle_id, "entity_id": entity_id, "result": result }),
        exit_code,
    ))
}

pub(super) fn run_command_gate_check(
    check: &crate::core::agent_task_loop_controller::AgentTaskGateBundleCheck,
) -> Result<AgentTaskGateCheckResult> {
    let command = check
        .input
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "command",
                "command gate check requires input.command",
                Some(check.input.to_string()),
                None,
            )
        })?;
    let output = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .output()
        .map_err(|error| {
            Error::internal_io(
                format!("failed to execute gate command '{command}': {error}"),
                None,
            )
        })?;
    let status = if output.status.success() {
        AgentTaskGateBundleStatus::Passed
    } else {
        AgentTaskGateBundleStatus::Failed
    };
    Ok(AgentTaskGateCheckResult {
        check_id: check.check_id.clone(),
        status,
        retryable: check.retryable,
        classification: None,
        evidence: Vec::new(),
        details: serde_json::json!({
            "command": command,
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_route_finding_action<E, D>(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    finding: &controller::AgentTaskLoopFindingPacket,
    dedupe_key: &str,
    entity_id: Option<&str>,
    request_template: &Value,
    executor: E,
    dispatch: &D,
) -> Result<(Value, i32)>
where
    E: AgentTaskExecutorAdapter + Clone,
    D: ControllerDispatchHook,
{
    let entity_id = ensure_finding_entity(record, finding, entity_id);
    let request = materialize_route_finding_request(request_template, finding, &entity_id);
    execute_spawn_task_action(
        record,
        action,
        dedupe_key,
        Some(&entity_id),
        &request,
        executor,
        dispatch,
    )
}

pub(super) fn ensure_finding_entity(
    record: &mut AgentTaskLoopControllerRecord,
    finding: &controller::AgentTaskLoopFindingPacket,
    entity_id: Option<&str>,
) -> String {
    if let Some(entity_id) = entity_id {
        return entity_id.to_string();
    }
    let key = finding
        .reproduction_key
        .as_deref()
        .unwrap_or(&finding.finding_id);
    let entity_id = format!("finding:{}", key.replace(['/', ':', '#', ' '], "_"));
    record
        .entities
        .entry(entity_id.clone())
        .or_insert_with(|| AgentTaskLoopEntity {
            entity_id: entity_id.clone(),
            entity_type: "finding".to_string(),
            key: key.to_string(),
            dedupe_key: format!("entity:finding:{key}"),
            state: Some("routed".to_string()),
            human_ready: false,
            parent_entity_ids: Vec::new(),
            run_refs: Vec::new(),
            artifact_refs: finding.lineage.clone(),
            provenance: finding
                .lineage
                .iter()
                .map(|artifact| AgentTaskLoopProvenanceRef {
                    kind: artifact
                        .kind
                        .clone()
                        .unwrap_or_else(|| "artifact".to_string()),
                    uri: artifact.uri.clone(),
                    caused_by: Some(finding.finding_id.clone()),
                })
                .collect(),
            metadata: serde_json::json!({
                "severity": finding.severity,
                "owner": finding.owner,
                "source_transformer": finding.source_transformer,
            }),
        });
    entity_id
}

pub(super) fn materialize_route_finding_request(
    template: &Value,
    finding: &controller::AgentTaskLoopFindingPacket,
    entity_id: &str,
) -> Value {
    let mut request = template.clone();
    if let Some(object) = request.as_object_mut() {
        object.insert(
            "entity_id".to_string(),
            Value::String(entity_id.to_string()),
        );
        object.insert(
            "finding_id".to_string(),
            Value::String(finding.finding_id.clone()),
        );
        object
            .entry("finding".to_string())
            .or_insert_with(|| serde_json::to_value(finding).unwrap_or(Value::Null));
    }
    request
}

//! Cook provider timeout and startup liveness reporting.

use super::cook::*;

pub(super) fn outcome_is_timeout(outcome: &crate::agent_task::AgentTaskOutcome) -> bool {
    outcome.status == crate::agent_task::AgentTaskOutcomeStatus::Timeout
        || outcome.failure_classification
            == Some(crate::agent_task::AgentTaskFailureClassification::Timeout)
        || outcome
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.class == "agent_task.provider_timeout")
}

pub(super) fn provider_timeout_ms(
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
    plan: &AgentTaskPlan,
) -> Option<u64> {
    let timeout = aggregate
        .outcomes
        .iter()
        .find(|outcome| outcome_is_timeout(outcome))?;
    timeout
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.data["timeout_ms"].as_u64())
        .max()
        .or_else(|| {
            plan.tasks
                .iter()
                .map(|task| {
                    crate::agent_task_timeout::effective_provider_timeout_ms(
                        task.limits.timeout_ms.or(plan.options.timeout_ms),
                        task.limits.max_runtime_ms,
                    )
                })
                .max()
        })
        .or(Some(crate::agent_task_timeout::DEFAULT_PROVIDER_TIMEOUT_MS))
}

/// The runner records this only after every observable progress channel stayed
/// empty. Classify only the current terminal completion, not prior attempts.
/// It is not a wall-clock timeout: keep the distinction at Cook's operator-facing
/// boundary rather than suggesting a larger timeout.
pub(super) fn startup_without_output_liveness_diagnostic(
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
) -> Option<&crate::agent_task::AgentTaskDiagnostic> {
    aggregate
        .outcomes
        .last()?
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.class == "agent_task.provider_liveness_timeout"
                && diagnostic.data["deadline"] == "liveness"
                && [
                    "stdout_bytes",
                    "stderr_bytes",
                    "runtime_progress_events",
                    "workspace_progress_events",
                ]
                .iter()
                .all(|field| diagnostic.data[*field].as_u64() == Some(0))
        })
}

/// Project the bounded no-output liveness evidence into Cook's durable result.
///
/// The provider's terminal diagnostic remains the authority on the underlying
/// execution. This projection makes its actionable meaning visible without
/// reclassifying the attempt as a timeout or manufacturing a retry that could
/// repeat an opaque startup.
pub(super) fn make_startup_without_output_actionable(
    lifecycle_store: Option<&AgentTaskLifecycleStore>,
    report: &mut AgentTaskRunResult<AgentTaskCookReport>,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
    run_id: &str,
) {
    let Some(diagnostic) = startup_without_output_liveness_diagnostic(aggregate) else {
        return;
    };
    let mut diagnostic_value = homeboy_core::redaction::redact_json(&serde_json::json!({
        "class": diagnostic.class,
        "message": redact_diagnostic_text(&diagnostic.message),
        "data": diagnostic.data,
    }));
    bound_diagnostic_value(&mut diagnostic_value, 0);
    let status = AgentTaskCookRecoveryAction {
        action: "status".to_string(),
        command: lifecycle_store.map_or_else(
            || cook_recovery_command(run_id, &["status", run_id]),
            |store| cook_recovery_command_in_store(store, run_id, &["status", run_id]),
        ),
    };
    let diagnose = AgentTaskCookRecoveryAction {
        action: "diagnose".to_string(),
        command: lifecycle_store.map_or_else(
            || cook_recovery_command(run_id, &["diagnose", run_id]),
            |store| cook_recovery_command_in_store(store, run_id, &["diagnose", run_id]),
        ),
    };

    report.value.terminal_phase = Some("provider_startup".to_string());
    report.value.terminal_failure_classification =
        Some("provider_startup_without_output".to_string());
    report.value.stop_reason = Some(
        "provider startup produced no stdout, stderr, structured progress, or workspace activity before its bounded liveness diagnostic; this is not a provider timeout. Inspect the durable status and diagnostics before choosing a recovery path.".to_string(),
    );
    report.value.primary_failure = Some(AgentTaskCookPrimaryFailure {
        schema: "homeboy/agent-task-cook-primary-failure/v1",
        provider_id: diagnostic.data["provider"]
            .as_str()
            .unwrap_or("provider")
            .to_string(),
        operation: "startup_without_output".to_string(),
        phase: "provider_startup".to_string(),
        exit_code: 1,
        stderr_excerpt: truncate_diagnostic_text(&redact_diagnostic_text(&diagnostic.message)),
        evidence_ref: format!("homeboy://agent-task/run/{run_id}/status"),
        next_action: status.clone(),
        diagnostic: Some(diagnostic_value.clone()),
    });
    if let Some(context) = report.value.failure_context.as_mut() {
        context.phase = "provider_startup".to_string();
        context.reason_code = "provider_startup_without_output".to_string();
        context.diagnostic = Some(diagnostic_value);
        context.recovery_legal = true;
        context.recovery_reason =
            "read-only status and diagnostic inspection are safe; no timeout increase or retry is implied"
                .to_string();
        context.legal_actions = vec![status.clone(), diagnose.clone()];
        context.next_actions = vec![status, diagnose];
        context.recovery_step = Some(CookRecoveryStep::none(
            "Provider startup produced no output; inspect status and diagnostics before choosing a recovery path.",
        ));
    }
}

pub(super) fn make_provider_timeout_actionable(
    lifecycle_store: Option<&AgentTaskLifecycleStore>,
    report: &mut AgentTaskRunResult<AgentTaskCookReport>,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
    plan: &AgentTaskPlan,
    run_id: &str,
    remaining_budget: Option<AgentTaskExecutionBudget>,
    provider_execution_active: bool,
) {
    let Some(timeout_ms) = provider_timeout_ms(aggregate, plan) else {
        return;
    };
    let remaining_executions = remaining_budget
        .as_ref()
        .map(|budget| budget.max_provider_executions)
        .unwrap_or(0);
    let remaining_retries = remaining_budget
        .as_ref()
        .map(|budget| budget.max_same_provider_retries)
        .unwrap_or(0);
    let remaining_rotations = remaining_budget
        .as_ref()
        .map(|budget| budget.max_provider_rotations)
        .unwrap_or(0);
    let next_timeout_ms = timeout_ms.saturating_add(timeout_ms.max(1));
    let remaining_deadline_ms = remaining_budget
        .as_ref()
        .and_then(|budget| budget.remaining_deadline_ms(crate::agent_task_timeout::now_unix_ms()));
    let deadline_expired = remaining_deadline_ms == Some(0);
    let can_retry = remaining_executions > 0
        && remaining_retries > 0
        && next_timeout_ms > timeout_ms
        && !deadline_expired;
    let command = can_retry.then(|| {
        let continuation = lifecycle_store.map_or_else(
            || cook_recover_command(None, run_id),
            |store| cook_recovery_command_in_store(store, run_id, &["recover", run_id]),
        );
        format!("{} --timeout-ms {next_timeout_ms}", continuation)
    });
    let recovery_guidance = if deadline_expired {
        "The durable provider execution deadline is exhausted.".to_string()
    } else {
        command
            .as_deref()
            .map(|command| format!("Retry with `{command}`."))
            .unwrap_or_else(|| "No provider timeout retry budget remains.".to_string())
    };

    report.value.terminal_phase = Some("provider".to_string());
    report.value.terminal_failure_classification = Some("provider_timeout".to_string());
    report.value.stop_reason = Some(format!(
        "provider exceeded timeout_ms={timeout_ms} without a successful candidate; promotion, gates, and finalization were not reached. Remaining budget: provider_executions={remaining_executions}, same_provider_retries={remaining_retries}, provider_rotations={remaining_rotations}. {recovery_guidance}",
    ));
    if let Some(context) = report.value.failure_context.as_mut() {
        context.phase = "provider".to_string();
        context.reason_code = "provider_timeout".to_string();
        context.diagnostic = Some(serde_json::json!({
            "class": "agent_task.provider_timeout",
            "message": format!("provider exceeded timeout_ms={timeout_ms}"),
            "data": {
                "timeout_ms": timeout_ms,
                "finalization_reached": false,
                "finalization_not_reached_reason": "provider execution did not produce a successful candidate",
                "remaining_provider_executions": remaining_executions,
                "remaining_same_provider_retries": remaining_retries,
                "remaining_provider_rotations": remaining_rotations,
                "remaining_execution_deadline_ms": remaining_deadline_ms,
                "provider_execution_active": provider_execution_active,
            }
        }));
        context.recovery_legal = can_retry && !provider_execution_active;
        context.recovery_reason = if deadline_expired {
            "the durable provider execution deadline is exhausted".to_string()
        } else if !can_retry {
            "provider timeout retry budget is exhausted".to_string()
        } else if provider_execution_active {
            "provider timeout retry is blocked until the timed-out provider exits".to_string()
        } else {
            "an explicit provider timeout increase can consume the remaining retry budget"
                .to_string()
        };
        context.legal_actions.clear();
        context.next_actions.clear();
        // A timeout retry spends budget on an explicitly larger deadline: an
        // operator decision `recover` does not make on its own.
        context.recovery_step = Some(CookRecoveryStep::none(
            command
                .as_deref()
                .filter(|_| !provider_execution_active)
                .map(|command| format!("Retry with an explicit timeout increase: `{command}`."))
                .unwrap_or_else(|| context.recovery_reason.clone()),
        ));
        if provider_execution_active {
            context.next_actions.push(AgentTaskCookRecoveryAction {
                action: "status".to_string(),
                command: lifecycle_store.map_or_else(
                    || format!("homeboy agent-task diagnose {run_id} --full"),
                    |store| {
                        cook_recovery_command_in_store(
                            store,
                            run_id,
                            &["diagnose", run_id, "--full"],
                        )
                    },
                ),
            });
        }
        // A resume command is only offered when it is actually admitted right
        // now. While the provider is still running, `recovery_legal` is false
        // and the durable retry route refuses the same command (#14732).
        if let Some(command) = command {
            if !provider_execution_active {
                let action = AgentTaskCookRecoveryAction {
                    action: "resume".to_string(),
                    command,
                };
                context.legal_actions.push(action.clone());
                context.next_actions.push(action);
            }
        }
    }
}

/// Project the scheduler's already-normalized rotation evidence onto every
/// terminal Cook surface. The aggregate remains the complete record; this is a
/// bounded top-level explanation for the no-candidate terminal path.
pub(super) fn make_provider_rotation_actionable(
    lifecycle_store: Option<&AgentTaskLifecycleStore>,
    report: &mut AgentTaskRunResult<AgentTaskCookReport>,
    aggregate: &AgentTaskAggregate,
    run_id: &str,
) {
    let Some((outcome, diagnostic)) = aggregate.outcomes.iter().find_map(|outcome| {
        outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.class == "agent_task.provider_rotation_exhausted")
            .map(|diagnostic| (outcome, diagnostic))
    }) else {
        return;
    };
    let diagnostic_message = redact_diagnostic_text(&diagnostic.message);
    let mut diagnostic_value = homeboy_core::redaction::redact_json(&serde_json::json!({
        "class": diagnostic.class,
        "message": diagnostic_message,
        "data": diagnostic.data,
    }));
    bound_diagnostic_value(&mut diagnostic_value, 0);
    let routes = provider_rotation_attempts(outcome)
        .unwrap_or_default()
        .into_iter()
        .map(|attempt| {
            let model = attempt
                .candidate_producing_model
                .or(attempt.attempted_model)
                .or(attempt.model)
                .unwrap_or_else(|| "default model".to_string());
            let classification = attempt
                .failure_classification
                .and_then(|classification| serde_json::to_value(classification).ok())
                .and_then(|classification| classification.as_str().map(str::to_string))
                .unwrap_or_else(|| "unclassified_failure".to_string());
            format!("{}/{}: {classification}", attempt.backend, model)
        })
        .collect::<Vec<_>>();
    let routes = if routes.is_empty() {
        truncate_diagnostic_text(&diagnostic_message)
    } else {
        truncate_diagnostic_text(&redact_diagnostic_text(&routes.join("; ")))
    };
    let budget = aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.diagnostics)
        .find(|diagnostic| diagnostic.class == "agent_task.execution_budget_exhausted")
        .map(|diagnostic| {
            format!(
                "; {}",
                truncate_diagnostic_text(&redact_diagnostic_text(&diagnostic.message))
            )
        })
        .unwrap_or_default();
    report.value.terminal_phase = Some("provider".to_string());
    report.value.terminal_failure_classification = Some("provider_rotation_exhausted".to_string());
    report.value.stop_reason = Some(format!(
        "provider rotation exhausted without a candidate: {routes}{budget}"
    ));
    report.value.primary_failure = Some(AgentTaskCookPrimaryFailure {
        schema: "homeboy/agent-task-cook-primary-failure/v1",
        provider_id: "provider_rotation".to_string(),
        operation: "route_selection".to_string(),
        phase: "provider".to_string(),
        exit_code: 1,
        stderr_excerpt: truncate_diagnostic_text(&diagnostic_message),
        evidence_ref: format!("homeboy://agent-task/run/{run_id}/status"),
        next_action: AgentTaskCookRecoveryAction {
            action: "diagnose".to_string(),
            command: lifecycle_store.map_or_else(
                || format!("homeboy agent-task diagnose {run_id} --full"),
                |store| {
                    cook_recovery_command_in_store(store, run_id, &["diagnose", run_id, "--full"])
                },
            ),
        },
        diagnostic: Some(diagnostic_value.clone()),
    });
    if let Some(context) = report.value.failure_context.as_mut() {
        context.phase = "provider".to_string();
        context.reason_code = "provider_rotation_exhausted".to_string();
        context.diagnostic = Some(diagnostic_value);
    }
}

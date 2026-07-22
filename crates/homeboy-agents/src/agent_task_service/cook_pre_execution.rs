//! Cook attempt materialization, pre-execution failure classification, and
//! terminal-executor identity.
//!
//! Extracted from `cook.rs`: `materialize_initial_cook_attempt` (durable first
//! attempt submission), the pre-execution failure boundary
//! (`retryable_pre_execution_failure`/`with_pre_execution_phase`/
//! `pre_execution_failure_*`/`record_pre_execution_failure`) that keeps a
//! lifecycle-owned failure distinct from a provider result, and the
//! terminal-executor identity helpers (`terminal_executor_matches`/
//! `provider_rotation_attempts`/`terminal_executor_identity`) used to decide
//! follow-up rotation. These are attempt-setup and terminal-classification
//! helpers around the `run_cook` loop; grouping them out of the loop keeps the
//! spine focused. This is one of the clusters the recent cook-retry fixes grew.

use serde_json::Value;

use crate::agent_task::AgentTaskExecutor;
use crate::agent_task_lifecycle;
use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::{Error, Result};

use super::cook::{
    AgentTaskCookAttemptDispatcher, AgentTaskCookAttemptReport, AgentTaskCookReport,
    AgentTaskCookServiceOptions,
};
use super::cook_promotion::cook_report;
use super::AgentTaskRunResult;

/// Persist the controller-owned initial attempt before transport preparation so
/// runner eligibility failures remain addressable through the cook alias.
pub(crate) fn materialize_initial_cook_attempt(
    options: &AgentTaskCookServiceOptions,
) -> Result<()> {
    if agent_task_lifecycle::run_record_exists(&options.initial_run_id)? {
        return Ok(());
    }
    match agent_task_lifecycle::submit_plan(&options.initial_plan, Some(&options.initial_run_id)) {
        Ok(_) => {
            agent_task_lifecycle::record_cook_attempt(&options.cook_id, 1, &options.initial_run_id)
                .map(|_| ())
        }
        Err(error) => {
            // `submit_plan` persists admission failures before returning them.
            if agent_task_lifecycle::run_record_exists(&options.initial_run_id)? {
                agent_task_lifecycle::record_cook_attempt(
                    &options.cook_id,
                    1,
                    &options.initial_run_id,
                )?;
            }
            Err(error)
        }
    }
}

pub(crate) fn retryable_pre_execution_failure(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    record.metadata["pre_execution_failure"]["retryable"] == Value::Bool(true)
}

#[derive(Debug)]
pub(crate) struct PreExecutionFailureDetails {
    pub(crate) retryable: bool,
    pub(crate) phase: Option<String>,
    pub(crate) classification: Option<String>,
}

pub(crate) fn with_pre_execution_phase(mut error: Error, phase: &str) -> Error {
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    error.details["pre_execution_phase"] = Value::String(phase.to_string());
    error
}

pub(crate) fn pre_execution_failure_phase<'a>(
    error: &'a Error,
    dispatcher: Option<&dyn AgentTaskCookAttemptDispatcher>,
) -> &'a str {
    error
        .details
        .get("pre_execution_phase")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            dispatcher
                .map(|dispatcher| dispatcher.pre_execution_failure_phase())
                .unwrap_or("cook_pre_execution")
        })
}

pub(crate) fn pre_execution_failure_details(
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    error: &Error,
) -> PreExecutionFailureDetails {
    let failure = record.and_then(|record| record.metadata.get("pre_execution_failure"));
    PreExecutionFailureDetails {
        retryable: failure
            .and_then(|failure| failure.get("retryable"))
            .and_then(Value::as_bool)
            .unwrap_or(error.retryable == Some(true)),
        phase: failure
            .and_then(|failure| failure.get("phase"))
            .and_then(Value::as_str)
            .map(str::to_string),
        classification: failure
            .and_then(|failure| failure.get("failure_classification"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

pub(crate) fn pre_execution_failure_report(
    cook_id: String,
    attempts: Vec<AgentTaskCookAttemptReport>,
    failure: PreExecutionFailureDetails,
    error: Error,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let phase = failure.phase.as_deref().unwrap_or("cook_pre_execution");
    let classification = failure.classification.as_deref().unwrap_or("unknown");
    let mut report = cook_report(
        cook_id,
        "pre_execution_failure",
        attempts,
        None,
        Some(format!(
            "pre-provider failure in phase `{phase}` classified as `{classification}`: {error}"
        )),
        1,
    );
    report.value.terminal_phase = failure.phase;
    report.value.terminal_failure_classification = failure.classification;
    report
}

/// Pre-execution failures happen before a provider can receive work. Persist a
/// normal terminal run so the Cook alias can expose its complete retry history.
pub(crate) fn record_pre_execution_failure(
    plan: &AgentTaskPlan,
    run_id: &str,
    error: &Error,
    phase: &str,
) -> Result<()> {
    if !agent_task_lifecycle::run_record_exists(run_id)? {
        agent_task_lifecycle::submit_plan(plan, Some(run_id))?;
    }
    agent_task_lifecycle::record_pre_execution_failure(run_id, plan, phase, error)?;
    Ok(())
}

pub(crate) fn terminal_executor_matches(
    aggregate: &crate::agent_task_scheduler::AgentTaskAggregate,
    plan: &AgentTaskPlan,
    durable_provider_executions: Option<&Value>,
    follow_up: &AgentTaskExecutor,
) -> Option<bool> {
    let outcome = aggregate.outcomes.last()?;
    let terminal = terminal_executor_identity(outcome, plan, durable_provider_executions)?;
    Some(
        terminal.backend == follow_up.backend
            && terminal.selector == follow_up.selector
            && terminal.model.as_deref() == follow_up.model(),
    )
}

pub(crate) fn provider_rotation_attempts(
    outcome: &crate::agent_task::AgentTaskOutcome,
) -> Option<Vec<crate::agent_task_scheduler::AgentTaskProviderRotationAttempt>> {
    serde_json::from_value(
        outcome
            .metadata
            .pointer("/provider_rotation/attempts")?
            .clone(),
    )
    .ok()
}

pub(crate) struct TerminalExecutorIdentity {
    backend: String,
    selector: Option<String>,
    model: Option<String>,
}

pub(crate) fn terminal_executor_identity(
    outcome: &crate::agent_task::AgentTaskOutcome,
    plan: &AgentTaskPlan,
    durable_provider_executions: Option<&Value>,
) -> Option<TerminalExecutorIdentity> {
    // Rotation evidence is the only persisted source with all three executor
    // fields after a provider swap. A durable execution ledger, when present,
    // must corroborate its backend/model rather than silently selecting the
    // initial plan executor.
    if outcome
        .metadata
        .pointer("/provider_rotation/attempts")
        .is_some()
    {
        let attempt = provider_rotation_attempts(outcome)?.last()?.clone();
        let terminal = TerminalExecutorIdentity {
            backend: attempt.backend,
            selector: attempt.selector,
            model: attempt.model,
        };
        if durable_provider_executions.is_some() {
            let durable = terminal_provider_execution(outcome, durable_provider_executions)?;
            return (durable.backend == terminal.backend && durable.model == terminal.model)
                .then_some(terminal);
        }
        return Some(terminal);
    }

    // Preserve the normalized outcome identity used before durable execution
    // evidence was introduced. When both sources exist they must agree.
    if let Some(executor) = outcome.metadata.get("executor") {
        let terminal = TerminalExecutorIdentity {
            backend: executor.get("backend")?.as_str()?.to_string(),
            selector: executor
                .get("selector")
                .and_then(Value::as_str)
                .map(str::to_string),
            model: executor
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        if durable_provider_executions.is_some() {
            let durable = terminal_provider_execution(outcome, durable_provider_executions)?;
            return (durable.backend == terminal.backend && durable.model == terminal.model)
                .then_some(terminal);
        }
        return Some(terminal);
    }

    // Without rotation, the source task is authoritative for the selector only
    // if the durable terminal execution proves it remained the executor.
    let task = plan
        .tasks
        .iter()
        .find(|task| task.task_id == outcome.task_id)?;
    let durable = terminal_provider_execution(outcome, durable_provider_executions)?;
    (durable.backend == task.executor.backend && durable.model.as_deref() == task.executor.model())
        .then_some(TerminalExecutorIdentity {
            backend: task.executor.backend.clone(),
            selector: task.executor.selector.clone(),
            model: task.executor.model().map(str::to_string),
        })
}

struct DurableTerminalExecution {
    backend: String,
    model: Option<String>,
}

fn terminal_provider_execution(
    outcome: &crate::agent_task::AgentTaskOutcome,
    durable_provider_executions: Option<&Value>,
) -> Option<DurableTerminalExecution> {
    let executions = durable_provider_executions?.as_array()?;
    let terminal_attempt = executions
        .iter()
        .filter(|execution| {
            execution["task_id"] == outcome.task_id
                && matches!(
                    execution["state"].as_str(),
                    Some(
                        "succeeded"
                            | "failed"
                            | "cancelled"
                            | "timed_out"
                            | "candidate_recoverable"
                    )
                )
        })
        .filter_map(|execution| {
            execution["attempt"]
                .as_u64()
                .map(|attempt| (attempt, execution))
        })
        .max_by_key(|(attempt, _)| *attempt)?
        .0;
    let identities: Vec<_> = executions
        .iter()
        .filter(|execution| {
            execution["task_id"] == outcome.task_id
                && execution["attempt"].as_u64() == Some(terminal_attempt)
                && matches!(
                    execution["state"].as_str(),
                    Some(
                        "succeeded"
                            | "failed"
                            | "cancelled"
                            | "timed_out"
                            | "candidate_recoverable"
                    )
                )
        })
        .map(|execution| {
            Some(DurableTerminalExecution {
                backend: execution["backend"].as_str()?.to_string(),
                model: execution["model"].as_str().map(str::to_string),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let terminal = identities.first()?;
    identities
        .iter()
        .all(|identity| identity.backend == terminal.backend && identity.model == terminal.model)
        .then(|| DurableTerminalExecution {
            backend: terminal.backend.clone(),
            model: terminal.model.clone(),
        })
}

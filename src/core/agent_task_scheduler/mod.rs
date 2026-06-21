use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AgentTaskTypedArtifact,
    AGENT_TASK_OUTCOME_SCHEMA,
};
pub use crate::core::agent_task_schedule::{
    AgentTaskAdaptiveConcurrencyAction, AgentTaskAdaptiveConcurrencyDecision,
    AgentTaskAdaptiveConcurrencyInputs, AgentTaskAdaptiveConcurrencyPolicy,
    AgentTaskAdaptiveConcurrencyStatus, AgentTaskAggregate, AgentTaskAggregateStatus,
    AgentTaskAggregateTotals, AgentTaskArtifactBinding, AgentTaskArtifactLineage,
    AgentTaskArtifactOutputDeclaration, AgentTaskArtifactRunBinding, AgentTaskBackpressureStatus,
    AgentTaskCancellationToken, AgentTaskChildRun, AgentTaskExecutionContext,
    AgentTaskOutputBinding, AgentTaskOutputDependencies, AgentTaskPlan, AgentTaskProgressEvent,
    AgentTaskQueueStatus, AgentTaskResourceBudget, AgentTaskResourceBudgetStatus,
    AgentTaskRetryPolicy, AgentTaskScheduleOptions, AgentTaskState, AGENT_TASK_AGGREGATE_SCHEMA,
    AGENT_TASK_PLAN_SCHEMA,
};
use crate::core::agent_task_timeout::timeout_with_grace;
use crate::core::agent_task_timeout_artifacts::{
    append_unique_artifacts, append_unique_evidence_refs, is_actionable_patch_artifact,
    is_empty_patch_artifact, merge_timeout_outcome, TimeoutArtifactDiscovery,
};
use crate::core::config::value_type_name;

/// Authoritative execution adapter consumed by the agent-task scheduler.
///
/// Provider lifecycle payloads live with the agent-task schemas, but execution
/// dispatch goes through this single adapter shape so provider selection,
/// outcome normalization, timeouts, and cancellation do not drift across seams.
pub trait AgentTaskExecutorAdapter: Send + Sync + 'static {
    fn execute(
        &self,
        request: AgentTaskRequest,
        context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome;

    fn cancel(&self, _task_id: &str) {}
}

pub struct AgentTaskScheduler<E> {
    executor: Arc<E>,
}

impl<E> AgentTaskScheduler<E>
where
    E: AgentTaskExecutorAdapter,
{
    pub fn new(executor: E) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }

    pub fn run(&self, plan: AgentTaskPlan) -> AgentTaskAggregate {
        self.run_with_cancellation(plan, AgentTaskCancellationToken::default())
    }

    pub(crate) fn run_with_cancellation(
        &self,
        plan: AgentTaskPlan,
        cancellation: AgentTaskCancellationToken,
    ) -> AgentTaskAggregate {
        let mut plan = plan.canonicalize();
        let max_concurrency = plan.options.max_concurrency.max(1);
        let total_tasks = plan.tasks.len();
        let max_queue_depth = plan.options.max_queue_depth.or(plan.options.max_tasks);
        let retry_budget_total = plan.options.retry.max_retries_total;
        let output_dependencies = plan.output_dependencies.clone();
        let mut retry_budget_used = 0;
        let mut backpressure = Vec::new();
        let mut blocked_count = 0;
        let accepted_tasks = max_queue_depth
            .map(|max_queue_depth| max_queue_depth.min(plan.tasks.len()))
            .unwrap_or(plan.tasks.len());
        let blocked_tasks = if accepted_tasks < plan.tasks.len() {
            plan.tasks.split_off(accepted_tasks)
        } else {
            Vec::new()
        };
        let mut queued: VecDeque<ScheduledTask> = plan
            .tasks
            .drain(..)
            .map(|request| ScheduledTask {
                request,
                attempt: 1,
            })
            .collect();
        let mut running: Vec<RunningTask> = Vec::new();
        let mut outcomes = Vec::new();
        let mut completed_by_task: HashMap<String, AgentTaskOutcome> = HashMap::new();
        let mut events = Vec::new();
        let mut cancellation_notified = false;
        let max_attempts = plan.options.retry.max_attempts.max(1);
        let (tx, rx) = mpsc::channel();
        let cancellation_tx = tx.clone();
        cancellation.on_cancel(Arc::new(move || {
            let _ = cancellation_tx.send(SchedulerEvent::Cancellation);
        }));
        let mut adaptive_decisions = Vec::new();
        let mut last_effective_concurrency = None;

        for task in &queued {
            events.push(event(
                &task.request.task_id,
                AgentTaskState::Queued,
                1,
                None,
            ));
        }

        for request in blocked_tasks {
            blocked_count += 1;
            let message = format!(
                "task blocked by max_queue_depth={}",
                max_queue_depth.unwrap_or_default()
            );
            backpressure.push(AgentTaskBackpressureStatus {
                kind: "queue_depth".to_string(),
                message: message.clone(),
                task_id: Some(request.task_id.clone()),
            });
            events.push(event(
                &request.task_id,
                AgentTaskState::Blocked,
                1,
                Some(message.clone()),
            ));
            outcomes.push(AgentTaskScheduleSupport::blocked_outcome(
                request.task_id,
                message,
            ));
        }

        while !queued.is_empty() || !running.is_empty() {
            if cancellation.is_cancelled() {
                AgentTaskScheduleSupport::cancel_queued(&mut queued, &mut outcomes, &mut events);
                if !cancellation_notified {
                    for task in &running {
                        self.executor.cancel(&task.task_id);
                    }
                    cancellation_notified = true;
                }
            }

            let adaptive_decision = adaptive_concurrency_decision(
                plan.options.adaptive_concurrency.as_ref(),
                max_concurrency,
                queued.len(),
                running.len(),
                &plan.options.resource_budget,
                active_resource_units(&running),
                last_effective_concurrency,
            );
            let effective_concurrency = adaptive_decision
                .as_ref()
                .map(|decision| decision.effective_concurrency)
                .unwrap_or(max_concurrency);
            if let Some(decision) = adaptive_decision {
                last_effective_concurrency = Some(decision.effective_concurrency);
                if adaptive_decisions
                    .last()
                    .map(|previous: &AgentTaskAdaptiveConcurrencyDecision| {
                        previous.action != decision.action
                            || previous.effective_concurrency != decision.effective_concurrency
                            || previous.reason != decision.reason
                    })
                    .unwrap_or(true)
                {
                    adaptive_decisions.push(decision);
                }
            }

            if effective_concurrency == 0 && running.is_empty() && !queued.is_empty() {
                while let Some(task) = queued.pop_front() {
                    let message = "adaptive concurrency paused dispatch".to_string();
                    AgentTaskScheduleSupport::block_and_record_scheduled_task(
                        &task,
                        "adaptive_concurrency",
                        message,
                        &mut backpressure,
                        &mut events,
                        &mut outcomes,
                        &mut blocked_count,
                    );
                }
                break;
            }

            while running.len() < effective_concurrency
                && !queued.is_empty()
                && !cancellation.is_cancelled()
            {
                let Some(next_index) = AgentTaskScheduleSupport::next_dispatchable_index(
                    &queued,
                    &running,
                    &completed_by_task,
                    &output_dependencies,
                    &plan.options.per_executor_concurrency,
                    &plan.options.per_model_concurrency,
                    &plan.options.resource_budget,
                ) else {
                    if running.is_empty() {
                        if let Some(task) = queued.pop_front() {
                            if let Some(message) =
                                AgentTaskScheduleSupport::waiting_for_task_dependencies(
                                    &task,
                                    &completed_by_task,
                                    &output_dependencies,
                                )
                            {
                                let outcome = AgentTaskScheduleSupport::block_scheduled_task(
                                    &task,
                                    "output_dependency",
                                    message,
                                    &mut backpressure,
                                    &mut events,
                                );
                                record_completed_outcome(
                                    &mut completed_by_task,
                                    &mut outcomes,
                                    outcome,
                                );
                                blocked_count += 1;
                                continue;
                            }

                            let task_units =
                                task_resource_units(&task.request, &plan.options.resource_budget);
                            let max_active_units = plan
                                .options
                                .resource_budget
                                .max_active_units
                                .unwrap_or_default();
                            let message = format!(
                                "task requires resource_units={task_units} over max_active_units={max_active_units}"
                            );
                            AgentTaskScheduleSupport::block_and_record_scheduled_task(
                                &task,
                                "resource_budget",
                                message,
                                &mut backpressure,
                                &mut events,
                                &mut outcomes,
                                &mut blocked_count,
                            );
                            continue;
                        }
                        break;
                    }
                    let dependency_wait = queued.front().and_then(|task| {
                        AgentTaskScheduleSupport::waiting_for_task_dependencies(
                            task,
                            &completed_by_task,
                            &output_dependencies,
                        )
                    });
                    backpressure.push(AgentTaskBackpressureStatus {
                        kind: if dependency_wait.is_some() {
                            "output_dependency".to_string()
                        } else {
                            AgentTaskScheduleSupport::backpressure_kind(
                                &queued,
                                &running,
                                &plan.options.per_executor_concurrency,
                                &plan.options.per_model_concurrency,
                                &plan.options.resource_budget,
                            )
                            .to_string()
                        },
                        message: dependency_wait.unwrap_or_else(|| {
                            "queued tasks are waiting for scheduler capacity".to_string()
                        }),
                        task_id: queued.front().map(|task| task.request.task_id.clone()),
                    });
                    break;
                };
                let scheduled = queued.remove(next_index).expect("queued task");
                let mut request = scheduled.request;
                if let Err(outcome) = AgentTaskScheduleSupport::render_output_dependencies(
                    &mut request,
                    &completed_by_task,
                    &output_dependencies,
                ) {
                    events.push(event(
                        &outcome.task_id,
                        AgentTaskState::Skipped,
                        scheduled.attempt,
                        outcome.summary.clone(),
                    ));
                    record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                    continue;
                }
                let task_id = request.task_id.clone();
                let executor_key = executor_key(&request);
                let executor = Arc::clone(&self.executor);
                let plan_id = plan.plan_id.clone();
                let task_timeout_ms = request.limits.timeout_ms.or(plan.options.timeout_ms);
                let tx = tx.clone();
                let attempt = scheduled.attempt;
                let context = AgentTaskExecutionContext {
                    plan_id,
                    attempt,
                    cancellation: cancellation.clone(),
                };

                events.push(event(&task_id, AgentTaskState::Running, attempt, None));
                running.push(RunningTask {
                    task_id: task_id.clone(),
                    request: request.clone(),
                    executor_key,
                    model_key: model_key(&request),
                    resource_units: task_resource_units(&request, &plan.options.resource_budget),
                    attempt,
                    started_at: Instant::now(),
                    timeout_ms: task_timeout_ms,
                });

                thread::spawn(move || {
                    let outcome = executor.execute(request, context);
                    let _ = tx.send(SchedulerEvent::TaskResult(TaskResult {
                        task_id,
                        attempt,
                        outcome,
                    }));
                });
            }

            AgentTaskScheduleSupport::expire_timed_out_tasks(
                &mut running,
                &mut outcomes,
                &mut events,
                self.executor.as_ref(),
            );

            if running.is_empty() {
                continue;
            }

            let wait_timeout = running
                .iter()
                .filter_map(|task| {
                    task.timeout_ms
                        .map(|ms| timeout_with_grace(ms).saturating_sub(task.started_at.elapsed()))
                })
                .min();
            match wait_timeout.map_or_else(
                || rx.recv().map_err(|_| None),
                |timeout| rx.recv_timeout(timeout).map_err(Some),
            ) {
                Ok(SchedulerEvent::Cancellation) => {
                    continue;
                }
                Ok(SchedulerEvent::TaskResult(result)) => {
                    if cancellation.is_cancelled() {
                        AgentTaskScheduleSupport::cancel_queued(
                            &mut queued,
                            &mut outcomes,
                            &mut events,
                        );
                    }
                    let running_task =
                        AgentTaskScheduleSupport::remove_running(&mut running, &result.task_id);
                    let Some(running_task) = running_task else {
                        continue;
                    };
                    let outcome = AgentTaskScheduleSupport::normalize_outcome(
                        result.outcome,
                        Some(&running_task),
                    );
                    let state = AgentTaskScheduleSupport::state_for_outcome(&outcome);
                    events.push(event(
                        &outcome.task_id,
                        state,
                        result.attempt,
                        outcome.summary.clone(),
                    ));
                    if AgentTaskScheduleSupport::should_retry(
                        &outcome,
                        result.attempt,
                        max_attempts,
                        retry_budget_total,
                        retry_budget_used,
                        &plan.options.retry.retryable_failure_classifications,
                    ) {
                        retry_budget_used += 1;
                        let mut request = running_task.request;
                        request.parent_plan_id = Some(plan.plan_id.clone());
                        let next_attempt = result.attempt + 1;
                        events.push(event(
                            &request.task_id,
                            AgentTaskState::Queued,
                            next_attempt,
                            Some("retry queued".to_string()),
                        ));
                        queued.push_back(ScheduledTask {
                            request,
                            attempt: next_attempt,
                        });
                        continue;
                    }
                    record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                }
                Err(Some(mpsc::RecvTimeoutError::Timeout)) => {}
                Err(Some(mpsc::RecvTimeoutError::Disconnected)) | Err(None) => break,
            }
        }

        let artifact_lineage =
            AgentTaskScheduleSupport::artifact_lineage(&outcomes, &plan.artifact_outputs);
        let child_runs = child_runs_for_outcomes(&outcomes);
        let artifact_bindings = artifact_bindings_for_outcomes(&outcomes);

        AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id,
            status: AgentTaskScheduleSupport::aggregate_status(&outcomes),
            totals: AgentTaskScheduleSupport::totals(total_tasks, &outcomes),
            queue: AgentTaskScheduleSupport::queue_status(
                max_concurrency,
                plan.options.max_tasks,
                plan.options.max_queue_depth,
                blocked_count,
                &outcomes,
                &plan.options.per_executor_concurrency,
                &plan.options.per_model_concurrency,
                &plan.options.resource_budget,
                plan.options.adaptive_concurrency.as_ref(),
                &adaptive_decisions,
                &backpressure,
                retry_budget_total.map(|budget| budget.saturating_sub(retry_budget_used)),
            ),
            outcomes,
            events,
            artifact_lineage,
            child_runs,
            artifact_bindings,
        }
    }
}

#[derive(Debug, Clone)]
struct ScheduledTask {
    request: AgentTaskRequest,
    attempt: u32,
}

#[derive(Debug, Clone)]
struct RunningTask {
    task_id: String,
    request: AgentTaskRequest,
    executor_key: String,
    model_key: Option<String>,
    resource_units: u32,
    attempt: u32,
    started_at: Instant,
    timeout_ms: Option<u64>,
}

struct TaskResult {
    task_id: String,
    attempt: u32,
    outcome: AgentTaskOutcome,
}

enum SchedulerEvent {
    TaskResult(TaskResult),
    Cancellation,
}

/// Record a finalized outcome in the completed-by-task index and the ordered
/// outcomes list. Shared by the scheduler's dependency-block, dependency-render,
/// and task-completion paths to keep recording behavior identical.
fn record_completed_outcome(
    completed_by_task: &mut HashMap<String, AgentTaskOutcome>,
    outcomes: &mut Vec<AgentTaskOutcome>,
    outcome: AgentTaskOutcome,
) {
    completed_by_task.insert(outcome.task_id.clone(), outcome.clone());
    outcomes.push(outcome);
}

fn child_runs_for_outcomes(outcomes: &[AgentTaskOutcome]) -> Vec<AgentTaskChildRun> {
    outcomes
        .iter()
        .filter_map(|outcome| {
            let run_id = child_run_id(outcome)?;
            Some(AgentTaskChildRun {
                task_id: outcome.task_id.clone(),
                run_id,
                state: AgentTaskScheduleSupport::state_for_outcome(outcome),
                provider: outcome
                    .metadata
                    .get("provider")
                    .or_else(|| outcome.metadata.pointer("/provider_handle/backend"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                metadata: child_run_metadata(outcome),
            })
        })
        .collect()
}

fn artifact_bindings_for_outcomes(
    outcomes: &[AgentTaskOutcome],
) -> Vec<AgentTaskArtifactRunBinding> {
    outcomes
        .iter()
        .filter_map(|outcome| child_run_id(outcome).map(|run_id| (outcome, run_id)))
        .flat_map(|(outcome, run_id)| {
            outcome
                .artifacts
                .iter()
                .map(move |artifact| AgentTaskArtifactRunBinding {
                    task_id: outcome.task_id.clone(),
                    run_id: run_id.clone(),
                    artifact_id: artifact.id.clone(),
                    kind: artifact.kind.clone(),
                    name: artifact.name.clone(),
                    path: artifact.path.clone(),
                    url: artifact.url.clone(),
                    sha256: artifact.sha256.clone(),
                })
        })
        .collect()
}

fn child_run_id(outcome: &AgentTaskOutcome) -> Option<String> {
    first_non_empty_json_string([
        outcome.metadata.get("child_run_id"),
        outcome.metadata.get("run_id"),
        outcome.metadata.get("remote_run_id"),
        outcome.metadata.get("provider_run_id"),
        outcome.metadata.pointer("/provider_handle/provider_run_id"),
        outcome.outputs.pointer("/provider_run_result/run_id"),
        outcome.outputs.pointer("/provider_run_result/id"),
    ])
}

fn child_run_metadata(outcome: &AgentTaskOutcome) -> serde_json::Value {
    let mut metadata = serde_json::Map::new();
    for key in ["provider", "provider_handle", "provider_handles"] {
        if let Some(value) = outcome.metadata.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    serde_json::Value::Object(metadata)
}

fn first_non_empty_json_string<'a>(
    values: impl IntoIterator<Item = Option<&'a serde_json::Value>>,
) -> Option<String> {
    values.into_iter().flatten().find_map(|value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

mod outcome;
mod scheduling;
#[cfg(test)]
mod tests;

use outcome::event;
use scheduling::{
    active_resource_units, adaptive_concurrency_decision, executor_key, model_key,
    task_resource_units, AgentTaskScheduleSupport,
};

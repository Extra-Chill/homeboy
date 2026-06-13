use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AGENT_TASK_OUTCOME_SCHEMA,
};
pub use crate::core::agent_task_schedule::{
    AgentTaskAdaptiveConcurrencyAction, AgentTaskAdaptiveConcurrencyDecision,
    AgentTaskAdaptiveConcurrencyInputs, AgentTaskAdaptiveConcurrencyPolicy,
    AgentTaskAdaptiveConcurrencyStatus, AgentTaskAggregate, AgentTaskAggregateStatus,
    AgentTaskAggregateTotals, AgentTaskArtifactBinding, AgentTaskArtifactLineage,
    AgentTaskArtifactOutputDeclaration, AgentTaskBackpressureStatus, AgentTaskCancellationToken,
    AgentTaskExecutionContext, AgentTaskOutputBinding, AgentTaskOutputDependencies, AgentTaskPlan,
    AgentTaskProgressEvent, AgentTaskQueueStatus, AgentTaskResourceBudget,
    AgentTaskResourceBudgetStatus, AgentTaskRetryPolicy, AgentTaskScheduleOptions, AgentTaskState,
    AGENT_TASK_AGGREGATE_SCHEMA, AGENT_TASK_PLAN_SCHEMA,
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
        mut plan: AgentTaskPlan,
        cancellation: AgentTaskCancellationToken,
    ) -> AgentTaskAggregate {
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
                    outcomes.push(AgentTaskScheduleSupport::block_scheduled_task(
                        &task,
                        "adaptive_concurrency",
                        message,
                        &mut backpressure,
                        &mut events,
                    ));
                    blocked_count += 1;
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
                                completed_by_task.insert(outcome.task_id.clone(), outcome.clone());
                                outcomes.push(outcome);
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
                            outcomes.push(AgentTaskScheduleSupport::block_scheduled_task(
                                &task,
                                "resource_budget",
                                message,
                                &mut backpressure,
                                &mut events,
                            ));
                            blocked_count += 1;
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
                    completed_by_task.insert(outcome.task_id.clone(), outcome.clone());
                    outcomes.push(outcome);
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
                    let _ = tx.send(TaskResult {
                        task_id,
                        attempt,
                        outcome,
                    });
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

            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(result) => {
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
                    completed_by_task.insert(outcome.task_id.clone(), outcome.clone());
                    outcomes.push(outcome);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let artifact_lineage =
            AgentTaskScheduleSupport::artifact_lineage(&outcomes, &plan.artifact_outputs);

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

struct AgentTaskScheduleSupport;

impl AgentTaskScheduleSupport {
    fn next_dispatchable_index(
        queued: &VecDeque<ScheduledTask>,
        running: &[RunningTask],
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
        per_executor_concurrency: &HashMap<String, usize>,
        per_model_concurrency: &HashMap<String, usize>,
        resource_budget: &AgentTaskResourceBudget,
    ) -> Option<usize> {
        queued.iter().position(|task| {
            if !Self::dependencies_satisfied(&task.request, completed_by_task, output_dependencies)
            {
                return false;
            }

            let executor_key = executor_key(&task.request);
            let limit = per_executor_concurrency
                .get(&executor_key)
                .copied()
                .unwrap_or(usize::MAX)
                .max(1);
            let running_for_executor = running
                .iter()
                .filter(|running| running.executor_key == executor_key)
                .count();

            if running_for_executor >= limit {
                return false;
            }

            if let Some(model_key) = model_key(&task.request) {
                let model_limit = per_model_concurrency
                    .get(&model_key)
                    .copied()
                    .unwrap_or(usize::MAX)
                    .max(1);
                let running_for_model = running
                    .iter()
                    .filter(|running| running.model_key.as_ref() == Some(&model_key))
                    .count();
                if running_for_model >= model_limit {
                    return false;
                }
            }

            resource_capacity_available(&task.request, running, resource_budget)
        })
    }

    fn dependencies_satisfied(
        request: &AgentTaskRequest,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> bool {
        Self::dependency_task_ids(request, output_dependencies)
            .iter()
            .all(|task_id| completed_by_task.contains_key(task_id))
    }

    fn waiting_for_dependencies(
        request: &AgentTaskRequest,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> Option<String> {
        let missing: Vec<String> = Self::dependency_task_ids(request, output_dependencies)
            .into_iter()
            .filter(|task_id| !completed_by_task.contains_key(task_id))
            .collect();

        (!missing.is_empty()).then(|| {
            format!(
                "task blocked waiting for output dependencies: {}",
                missing.join(", ")
            )
        })
    }

    fn waiting_for_task_dependencies(
        task: &ScheduledTask,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> Option<String> {
        Self::waiting_for_dependencies(&task.request, completed_by_task, output_dependencies)
    }

    fn block_scheduled_task(
        task: &ScheduledTask,
        kind: &str,
        message: String,
        backpressure: &mut Vec<AgentTaskBackpressureStatus>,
        events: &mut Vec<AgentTaskProgressEvent>,
    ) -> AgentTaskOutcome {
        backpressure.push(AgentTaskBackpressureStatus {
            kind: kind.to_string(),
            message: message.clone(),
            task_id: Some(task.request.task_id.clone()),
        });
        events.push(event(
            &task.request.task_id,
            AgentTaskState::Blocked,
            task.attempt,
            Some(message.clone()),
        ));
        Self::blocked_outcome(task.request.task_id.clone(), message)
    }

    fn dependency_task_ids(
        request: &AgentTaskRequest,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> Vec<String> {
        let Some(dependencies) = output_dependencies.get(&request.task_id) else {
            return Vec::new();
        };
        let mut task_ids = dependencies.depends_on.clone();
        for binding in dependencies.bindings.values() {
            if !task_ids.contains(&binding.task_id) {
                task_ids.push(binding.task_id.clone());
            }
        }
        task_ids
    }

    fn render_output_dependencies(
        request: &mut AgentTaskRequest,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> Result<(), AgentTaskOutcome> {
        let Some(dependencies) = output_dependencies.get(&request.task_id) else {
            return Ok(());
        };
        let bindings = match Self::resolve_output_bindings(request, dependencies, completed_by_task)
        {
            Ok(bindings) => bindings,
            Err(message) => return Err(Self::skipped_output_dependency_outcome(request, message)),
        };

        request.instructions = render_template_string(&request.instructions, &bindings);
        render_value_templates(&mut request.inputs, &bindings);
        render_value_templates(&mut request.executor.config, &bindings);
        render_value_templates(&mut request.workspace.materialization, &bindings);
        render_value_templates(&mut request.metadata, &bindings);
        for artifact in &mut request.expected_artifacts {
            *artifact = render_template_string(artifact, &bindings);
        }
        mark_generated_from_outputs(request, dependencies, &bindings);
        Ok(())
    }

    fn resolve_output_bindings(
        request: &AgentTaskRequest,
        dependencies: &AgentTaskOutputDependencies,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
    ) -> Result<HashMap<String, Value>, String> {
        let mut bindings = HashMap::new();
        for (name, binding) in &dependencies.bindings {
            let value = Self::select_bound_output(request, name, binding, completed_by_task)?;
            bindings.insert(name.clone(), value);
        }
        Ok(bindings)
    }

    fn select_bound_output(
        request: &AgentTaskRequest,
        name: &str,
        binding: &AgentTaskOutputBinding,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
    ) -> Result<Value, String> {
        let Some(outcome) = completed_by_task.get(&binding.task_id) else {
            return Err(format!(
                "task '{}' skipped because output binding '{}' waited for missing task '{}'",
                request.task_id, name, binding.task_id
            ));
        };

        if let Some(artifact_binding) = &binding.artifact {
            let Some(artifact) = outcome.artifacts.iter().find(|artifact| {
                artifact.kind == artifact_binding.kind
                    && artifact_binding
                        .artifact_id
                        .as_ref()
                        .map(|artifact_id| artifact.id == *artifact_id)
                        .unwrap_or(true)
                    && artifact_binding
                        .schema
                        .as_ref()
                        .map(|schema| {
                            artifact
                                .metadata
                                .get("payload_schema")
                                .and_then(Value::as_str)
                                == Some(schema.as_str())
                        })
                        .unwrap_or(true)
            }) else {
                if !binding.default.is_null() {
                    return Ok(binding.default.clone());
                }
                if binding.required {
                    return Err(format!(
                        "task '{}' skipped because required artifact binding '{}' with kind '{}' was missing from task '{}'",
                        request.task_id, name, artifact_binding.kind, binding.task_id
                    ));
                }
                return Ok(Value::String(String::new()));
            };

            let artifact_value = serde_json::to_value(artifact).unwrap_or(Value::Null);
            if let Some(payload_path) = &artifact_binding.payload_path {
                if let Some(value) = artifact
                    .metadata
                    .get("payload")
                    .and_then(|payload| payload.pointer(payload_path))
                    .or_else(|| artifact_value.pointer(payload_path))
                {
                    return Ok(value.clone());
                }
                if !binding.default.is_null() {
                    return Ok(binding.default.clone());
                }
                if binding.required {
                    return Err(format!(
                        "task '{}' skipped because required artifact binding '{}' payload was missing at '{}' from task '{}'",
                        request.task_id, name, payload_path, binding.task_id
                    ));
                }
                return Ok(Value::String(String::new()));
            }

            return Ok(artifact_value);
        }

        let outcome_value = serde_json::to_value(outcome).unwrap_or(Value::Null);
        if let Some(value) = outcome_value.pointer(&binding.path) {
            return Ok(value.clone());
        }
        if !binding.default.is_null() {
            return Ok(binding.default.clone());
        }
        if binding.required {
            return Err(format!(
                "task '{}' skipped because required output binding '{}' was missing at '{}' from task '{}'",
                request.task_id, name, binding.path, binding.task_id
            ));
        }
        Ok(Value::String(String::new()))
    }

    fn skipped_output_dependency_outcome(
        request: &AgentTaskRequest,
        summary: String,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(summary.clone()),
            failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
            artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/output-dependency-skipped".to_string(),
                label: Some("scheduler output dependency skip".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "output_dependency_missing".to_string(),
                message: summary,
                data: Value::Null,
            }],
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: serde_json::json!({ "skipped": true, "skip_reason": "output_dependency_missing" }),
        }
    }

    fn artifact_lineage(
        outcomes: &[AgentTaskOutcome],
        declarations_by_task: &HashMap<String, Vec<AgentTaskArtifactOutputDeclaration>>,
    ) -> Vec<AgentTaskArtifactLineage> {
        let mut lineage = Vec::new();
        for outcome in outcomes {
            let Some(declarations) = declarations_by_task.get(&outcome.task_id) else {
                continue;
            };
            for declaration in declarations {
                if let Some(artifact) = outcome.artifacts.iter().find(|artifact| {
                    artifact.kind == declaration.kind
                        && declaration
                            .artifact_id
                            .as_ref()
                            .map(|artifact_id| artifact.id == *artifact_id)
                            .unwrap_or(true)
                }) {
                    let payload = declaration
                        .payload_path
                        .as_ref()
                        .and_then(|payload_path| select_artifact_payload(artifact, payload_path))
                        .unwrap_or(Value::Null);

                    lineage.push(AgentTaskArtifactLineage {
                        task_id: outcome.task_id.clone(),
                        name: declaration.name.clone(),
                        kind: artifact.kind.clone(),
                        schema: declaration.schema.clone().or_else(|| {
                            artifact
                                .metadata
                                .get("payload_schema")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        }),
                        artifact_id: Some(artifact.id.clone()),
                        path: artifact.path.clone(),
                        url: artifact.url.clone(),
                        sha256: artifact.sha256.clone(),
                        payload,
                    });
                }
            }
        }
        lineage
    }

    fn backpressure_kind(
        queued: &VecDeque<ScheduledTask>,
        running: &[RunningTask],
        per_executor_concurrency: &HashMap<String, usize>,
        per_model_concurrency: &HashMap<String, usize>,
        resource_budget: &AgentTaskResourceBudget,
    ) -> &'static str {
        let Some(task) = queued.front() else {
            return "scheduler_capacity";
        };
        let executor_key = executor_key(&task.request);
        let executor_limit = per_executor_concurrency
            .get(&executor_key)
            .copied()
            .unwrap_or(usize::MAX)
            .max(1);
        let running_for_executor = running
            .iter()
            .filter(|running| running.executor_key == executor_key)
            .count();
        if running_for_executor >= executor_limit {
            return "per_executor_concurrency";
        }

        if let Some(model_key) = model_key(&task.request) {
            let model_limit = per_model_concurrency
                .get(&model_key)
                .copied()
                .unwrap_or(usize::MAX)
                .max(1);
            let running_for_model = running
                .iter()
                .filter(|running| running.model_key.as_ref() == Some(&model_key))
                .count();
            if running_for_model >= model_limit {
                return "per_model_concurrency";
            }
        }

        if !resource_capacity_available(&task.request, running, resource_budget) {
            return "resource_budget";
        }

        "scheduler_capacity"
    }

    fn cancel_queued(
        queued: &mut VecDeque<ScheduledTask>,
        outcomes: &mut Vec<AgentTaskOutcome>,
        events: &mut Vec<AgentTaskProgressEvent>,
    ) {
        while let Some(task) = queued.pop_front() {
            events.push(event(
                &task.request.task_id,
                AgentTaskState::Cancelled,
                task.attempt,
                Some("cancelled before execution".to_string()),
            ));
            outcomes.push(Self::cancelled_outcome(
                task.request.task_id,
                "cancelled before execution".to_string(),
            ));
        }
    }

    fn expire_timed_out_tasks<E>(
        running: &mut Vec<RunningTask>,
        outcomes: &mut Vec<AgentTaskOutcome>,
        events: &mut Vec<AgentTaskProgressEvent>,
        executor: &E,
    ) where
        E: AgentTaskExecutorAdapter,
    {
        let mut index = 0;
        while index < running.len() {
            let timed_out = running[index]
                .timeout_ms
                .map(|timeout_ms| {
                    running[index].started_at.elapsed() > timeout_with_grace(timeout_ms)
                })
                .unwrap_or(false);

            if !timed_out {
                index += 1;
                continue;
            }

            let task = running.remove(index);
            executor.cancel(&task.task_id);
            let outcome = Self::timeout_outcome(
                task.task_id.clone(),
                task.timeout_ms.unwrap_or_default(),
                Some(&task.request),
                "scheduler_timeout",
            );
            events.push(event(
                &task.task_id,
                Self::state_for_outcome(&outcome),
                task.attempt,
                outcome.summary.clone(),
            ));
            outcomes.push(outcome);
        }
    }

    fn normalize_outcome(
        mut outcome: AgentTaskOutcome,
        running: Option<&RunningTask>,
    ) -> AgentTaskOutcome {
        if let Some(running) = running {
            if let Some(timeout_ms) = running.timeout_ms {
                if running.started_at.elapsed() > Duration::from_millis(timeout_ms) {
                    outcome.status = AgentTaskOutcomeStatus::Timeout;
                    outcome.failure_classification = Some(AgentTaskFailureClassification::Timeout);
                    outcome.diagnostics.push(AgentTaskDiagnostic {
                        class: "timeout".to_string(),
                        message: format!("task exceeded timeout_ms={timeout_ms}"),
                        data: Value::Null,
                    });
                }
            }

            if outcome.status == AgentTaskOutcomeStatus::Timeout {
                Self::reconcile_timeout_artifacts(
                    &mut outcome,
                    &running.request,
                    "provider_timeout",
                );
            }
        }
        outcome
    }

    fn cancelled_outcome(task_id: String, summary: String) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id,
            status: AgentTaskOutcomeStatus::Cancelled,
            summary: Some(summary),
            failure_classification: None,
            artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/cancelled".to_string(),
                label: Some("scheduler cancellation".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }

    fn timeout_outcome(
        task_id: String,
        timeout_ms: u64,
        request: Option<&AgentTaskRequest>,
        timeout_kind: &str,
    ) -> AgentTaskOutcome {
        let mut outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id,
            status: AgentTaskOutcomeStatus::Timeout,
            summary: Some(format!("task exceeded timeout_ms={timeout_ms}")),
            failure_classification: Some(AgentTaskFailureClassification::Timeout),
            artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/timeout".to_string(),
                label: Some("scheduler timeout".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "timeout".to_string(),
                message: format!("task exceeded timeout_ms={timeout_ms}"),
                data: Value::Null,
            }],
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };

        if let Some(request) = request {
            Self::reconcile_timeout_artifacts(&mut outcome, request, timeout_kind);
        }

        outcome
    }

    fn reconcile_timeout_artifacts(
        outcome: &mut AgentTaskOutcome,
        request: &AgentTaskRequest,
        timeout_kind: &str,
    ) {
        let discovery = TimeoutArtifactDiscovery::discover(request);
        let has_runtime_evidence = discovery.has_runtime_evidence();
        outcome.diagnostics.extend(discovery.diagnostics);
        if !has_runtime_evidence {
            append_unique_artifacts(&mut outcome.artifacts, discovery.artifacts);
            append_unique_evidence_refs(&mut outcome.evidence_refs, discovery.evidence_refs);
            outcome.diagnostics.push(AgentTaskDiagnostic {
                class: timeout_kind.to_string(),
                message:
                    "no completed runtime artifacts were discovered before timeout finalization"
                        .to_string(),
                data: Value::Null,
            });
            return;
        }

        if let Some(discovered) = discovery.outcome {
            merge_timeout_outcome(outcome, discovered);
        }

        append_unique_artifacts(&mut outcome.artifacts, discovery.artifacts);
        append_unique_evidence_refs(&mut outcome.evidence_refs, discovery.evidence_refs);

        let actionable_patch = outcome.metadata.get("actionable").and_then(Value::as_bool)
            != Some(false)
            && outcome.artifacts.iter().any(is_actionable_patch_artifact);
        if actionable_patch {
            outcome.status = AgentTaskOutcomeStatus::Succeeded;
            outcome.failure_classification = None;
            outcome.summary = Some(
                "runtime completed with an actionable artifact before timeout finalization"
                    .to_string(),
            );
        } else if outcome.status == AgentTaskOutcomeStatus::Succeeded
            && outcome.artifacts.iter().any(is_empty_patch_artifact)
        {
            outcome.status = AgentTaskOutcomeStatus::NoOp;
            outcome.failure_classification = None;
            outcome.summary = Some(
                "runtime completed with an empty patch artifact before timeout finalization"
                    .to_string(),
            );
        }

        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "completed_runtime_late_provider_race".to_string(),
            message: if actionable_patch {
                format!(
                    "{timeout_kind} observed after runtime artifacts were already available; preserving actionable artifacts"
                )
            } else {
                format!(
                    "{timeout_kind} observed after runtime artifacts were already available; preserving discovered artifacts"
                )
            },
            data: serde_json::json!({
                "timeout_kind": timeout_kind,
                "artifact_count": outcome.artifacts.len(),
                "evidence_ref_count": outcome.evidence_refs.len(),
                "actionable_patch": actionable_patch,
            }),
        });
    }

    fn blocked_outcome(task_id: String, summary: String) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id,
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(summary.clone()),
            failure_classification: Some(AgentTaskFailureClassification::PolicyDenied),
            artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/backpressure".to_string(),
                label: Some("scheduler backpressure".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "backpressure".to_string(),
                message: summary,
                data: Value::Null,
            }],
            outputs: Value::Null,
            follow_up: None,
            metadata: Value::Null,
            workflow: None,
        }
    }

    fn should_retry(
        outcome: &AgentTaskOutcome,
        attempt: u32,
        max_attempts: u32,
        retry_budget_total: Option<u32>,
        retry_budget_used: u32,
        retryable_failure_classifications: &[AgentTaskFailureClassification],
    ) -> bool {
        attempt < max_attempts
            && retry_budget_total
                .map(|budget| retry_budget_used < budget)
                .unwrap_or(true)
            && (retryable_failure_classifications.is_empty()
                || outcome
                    .failure_classification
                    .map(|classification| {
                        retryable_failure_classifications.contains(&classification)
                    })
                    .unwrap_or(false))
            && !matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded
                    | AgentTaskOutcomeStatus::NoOp
                    | AgentTaskOutcomeStatus::Cancelled
                    | AgentTaskOutcomeStatus::Timeout
            )
    }

    fn remove_running(running: &mut Vec<RunningTask>, task_id: &str) -> Option<RunningTask> {
        let index = running.iter().position(|task| task.task_id == task_id)?;
        Some(running.remove(index))
    }

    fn state_for_outcome(outcome: &AgentTaskOutcome) -> AgentTaskState {
        match outcome.status {
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
                AgentTaskState::Succeeded
            }
            AgentTaskOutcomeStatus::Timeout => AgentTaskState::TimedOut,
            AgentTaskOutcomeStatus::Cancelled => AgentTaskState::Cancelled,
            _ => AgentTaskState::Failed,
        }
    }

    fn queue_status(
        max_concurrency: usize,
        max_tasks: Option<usize>,
        max_queue_depth: Option<usize>,
        blocked_count: usize,
        outcomes: &[AgentTaskOutcome],
        per_executor_concurrency: &HashMap<String, usize>,
        per_model_concurrency: &HashMap<String, usize>,
        resource_budget: &AgentTaskResourceBudget,
        adaptive_policy: Option<&AgentTaskAdaptiveConcurrencyPolicy>,
        adaptive_decisions: &[AgentTaskAdaptiveConcurrencyDecision],
        backpressure: &[AgentTaskBackpressureStatus],
        retry_budget_remaining: Option<u32>,
    ) -> AgentTaskQueueStatus {
        let per_executor_concurrency = per_executor_concurrency
            .iter()
            .map(|(executor, max_concurrency)| (executor.clone(), (*max_concurrency).max(1)))
            .collect();
        let per_model_concurrency = per_model_concurrency
            .iter()
            .map(|(model, max_concurrency)| (model.clone(), (*max_concurrency).max(1)))
            .collect();

        AgentTaskQueueStatus {
            max_concurrency,
            adaptive_concurrency: adaptive_policy.map(|policy| {
                let max_adaptive_concurrency = policy
                    .max_concurrency
                    .unwrap_or(max_concurrency)
                    .max(policy.min_concurrency.max(1));
                AgentTaskAdaptiveConcurrencyStatus {
                    configured_max_concurrency: max_concurrency,
                    effective_concurrency: adaptive_decisions
                        .last()
                        .map(|decision| decision.effective_concurrency)
                        .unwrap_or(max_concurrency.min(max_adaptive_concurrency)),
                    min_concurrency: policy.min_concurrency.max(1),
                    max_concurrency: max_adaptive_concurrency,
                    decisions: adaptive_decisions.to_vec(),
                }
            }),
            max_tasks,
            max_queue_depth,
            queued: 0,
            running: 0,
            blocked: blocked_count,
            completed: outcomes.len(),
            per_executor_concurrency,
            per_model_concurrency,
            resource_budget: AgentTaskResourceBudgetStatus {
                max_active_units: resource_budget.max_active_units,
                default_task_units: resource_budget.default_task_units.max(1),
                active_units: 0,
                per_executor_task_units: resource_budget.per_executor_task_units.clone(),
                per_model_task_units: resource_budget.per_model_task_units.clone(),
            },
            backpressure: backpressure.to_vec(),
            retry_budget_remaining,
        }
    }

    fn aggregate_status(outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateStatus {
        if outcomes
            .iter()
            .any(|outcome| outcome.status == AgentTaskOutcomeStatus::Cancelled)
        {
            return AgentTaskAggregateStatus::Cancelled;
        }

        let failed = outcomes.iter().any(|outcome| {
            !matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
            )
        });
        let succeeded = outcomes.iter().any(|outcome| {
            matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
            )
        });

        match (succeeded, failed) {
            (true, false) => AgentTaskAggregateStatus::Succeeded,
            (true, true) => AgentTaskAggregateStatus::PartialFailure,
            _ => AgentTaskAggregateStatus::Failed,
        }
    }

    fn totals(total_tasks: usize, outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateTotals {
        let mut totals = AgentTaskAggregateTotals {
            queued: total_tasks.saturating_sub(outcomes.len()),
            ..AgentTaskAggregateTotals::default()
        };

        for outcome in outcomes {
            if outcome
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.class == "output_dependency_missing")
            {
                totals.skipped += 1;
                continue;
            }

            match outcome.status {
                AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
                    totals.succeeded += 1
                }
                AgentTaskOutcomeStatus::Timeout => totals.timed_out += 1,
                AgentTaskOutcomeStatus::Cancelled => totals.cancelled += 1,
                AgentTaskOutcomeStatus::Failed
                    if outcome.failure_classification
                        == Some(AgentTaskFailureClassification::PolicyDenied)
                        && outcome
                            .diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.class == "backpressure") =>
                {
                    totals.blocked += 1
                }
                _ => totals.failed += 1,
            }
        }

        totals
    }
}

fn select_artifact_payload(artifact: &AgentTaskArtifact, payload_path: &str) -> Option<Value> {
    artifact
        .metadata
        .get("payload")
        .and_then(|payload| payload.pointer(payload_path))
        .cloned()
        .or_else(|| {
            serde_json::to_value(artifact)
                .ok()
                .and_then(|artifact_value| artifact_value.pointer(payload_path).cloned())
        })
}

fn executor_key(request: &AgentTaskRequest) -> String {
    match &request.executor.selector {
        Some(selector) => format!("{}:{selector}", request.executor.backend),
        None => request.executor.backend.clone(),
    }
}

fn model_key(request: &AgentTaskRequest) -> Option<String> {
    request
        .executor
        .model
        .as_ref()
        .map(|model| match &request.executor.selector {
            Some(selector) => format!("{}:{selector}:{model}", request.executor.backend),
            None => format!("{}:{model}", request.executor.backend),
        })
}

fn task_resource_units(request: &AgentTaskRequest, budget: &AgentTaskResourceBudget) -> u32 {
    model_key(request)
        .and_then(|key| budget.per_model_task_units.get(&key).copied())
        .or_else(|| {
            budget
                .per_executor_task_units
                .get(&executor_key(request))
                .copied()
        })
        .unwrap_or_else(|| budget.default_task_units.max(1))
        .max(1)
}

fn active_resource_units(running: &[RunningTask]) -> u32 {
    running
        .iter()
        .map(|task| task.resource_units)
        .fold(0, u32::saturating_add)
}

fn resource_capacity_available(
    request: &AgentTaskRequest,
    running: &[RunningTask],
    budget: &AgentTaskResourceBudget,
) -> bool {
    let Some(max_active_units) = budget.max_active_units else {
        return true;
    };
    active_resource_units(running).saturating_add(task_resource_units(request, budget))
        <= max_active_units
}

fn adaptive_concurrency_decision(
    policy: Option<&AgentTaskAdaptiveConcurrencyPolicy>,
    configured_max_concurrency: usize,
    queued: usize,
    running: usize,
    resource_budget: &AgentTaskResourceBudget,
    active_units: u32,
    previous_effective_concurrency: Option<usize>,
) -> Option<AgentTaskAdaptiveConcurrencyDecision> {
    let policy = policy?;
    let configured_max_concurrency = configured_max_concurrency.max(1);
    let min_concurrency = policy.min_concurrency.max(1);
    let policy_max_concurrency = policy
        .max_concurrency
        .unwrap_or(configured_max_concurrency)
        .max(min_concurrency);
    let mut effective_concurrency = policy_max_concurrency;
    let mut reason =
        format!("adaptive concurrency held at configured ceiling {policy_max_concurrency}");

    if let Some(runner_capacity) = policy.runner_capacity {
        let available_runner_slots = runner_capacity.saturating_sub(policy.active_leases);
        if available_runner_slots == 0 {
            effective_concurrency = 0;
            reason = format!(
                "paused because active_leases={} consume runner_capacity={runner_capacity}",
                policy.active_leases
            );
        } else if available_runner_slots < effective_concurrency {
            effective_concurrency = available_runner_slots;
            reason = format!(
                "scaled down to available runner slots {available_runner_slots} from runner_capacity={runner_capacity} active_leases={}",
                policy.active_leases
            );
        } else if available_runner_slots > configured_max_concurrency
            && policy_max_concurrency > configured_max_concurrency
        {
            reason = format!(
                "scaled up because runner slots are available: runner_capacity={runner_capacity} active_leases={}",
                policy.active_leases
            );
        }
    }

    if let Some(max_active_units) = resource_budget.max_active_units {
        let default_task_units = resource_budget.default_task_units.max(1);
        let available_units = max_active_units.saturating_sub(active_units);
        let resource_slots = (available_units / default_task_units) as usize;
        if resource_slots == 0 {
            effective_concurrency = 0;
            reason = format!(
                "paused because active_units={active_units} consume max_active_units={max_active_units}"
            );
        } else if resource_slots < effective_concurrency {
            effective_concurrency = resource_slots;
            reason = format!(
                "scaled down to resource slots {resource_slots} from max_active_units={max_active_units} active_units={active_units} default_task_units={default_task_units}"
            );
        }
    }

    if policy
        .pause_on_pressure
        .zip(policy.resource_pressure)
        .map(|(pause_on, pressure)| pressure >= pause_on)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because resource_pressure={:?} reached pause_on_pressure={:?}",
            policy.resource_pressure.expect("pressure checked"),
            policy.pause_on_pressure.expect("pause threshold checked")
        );
    }

    if policy
        .pause_after_recent_failures
        .map(|threshold| threshold > 0 && policy.recent_failures >= threshold)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because recent_failures={} reached pause_after_recent_failures={}",
            policy.recent_failures,
            policy.pause_after_recent_failures.unwrap_or_default()
        );
    }

    if policy
        .pause_after_recent_timeouts
        .map(|threshold| threshold > 0 && policy.recent_timeouts >= threshold)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because recent_timeouts={} reached pause_after_recent_timeouts={}",
            policy.recent_timeouts,
            policy.pause_after_recent_timeouts.unwrap_or_default()
        );
    }

    if effective_concurrency > 0 {
        effective_concurrency = effective_concurrency
            .max(min_concurrency)
            .min(policy_max_concurrency);
    }
    if queued == 0 && running == 0 && effective_concurrency > configured_max_concurrency {
        reason = format!(
            "held because no queued or running tasks need fan-out above configured max {configured_max_concurrency}"
        );
        effective_concurrency = configured_max_concurrency;
    }

    let action = match (previous_effective_concurrency, effective_concurrency) {
        (_, 0) => AgentTaskAdaptiveConcurrencyAction::Paused,
        (Some(previous), current) if current > previous => {
            AgentTaskAdaptiveConcurrencyAction::Increased
        }
        (Some(previous), current) if current < previous => {
            AgentTaskAdaptiveConcurrencyAction::Decreased
        }
        (None, current) if current > configured_max_concurrency => {
            AgentTaskAdaptiveConcurrencyAction::Increased
        }
        (None, current) if current < configured_max_concurrency => {
            AgentTaskAdaptiveConcurrencyAction::Decreased
        }
        _ => AgentTaskAdaptiveConcurrencyAction::Held,
    };

    Some(AgentTaskAdaptiveConcurrencyDecision {
        action,
        effective_concurrency,
        previous_effective_concurrency,
        reason,
        inputs: AgentTaskAdaptiveConcurrencyInputs {
            queued,
            running,
            configured_max_concurrency,
            runner_capacity: policy.runner_capacity,
            active_leases: policy.active_leases,
            queue_depth: policy.queue_depth,
            resource_pressure: policy.resource_pressure,
            max_active_units: resource_budget.max_active_units,
            active_units,
            default_task_units: resource_budget.default_task_units.max(1),
            recent_failures: policy.recent_failures,
            recent_timeouts: policy.recent_timeouts,
        },
    })
}

fn render_value_templates(value: &mut Value, bindings: &HashMap<String, Value>) {
    match value {
        Value::String(raw) => {
            if let Some(rendered) = render_template_value(raw, bindings) {
                *value = rendered;
            } else {
                *raw = render_template_string(raw, bindings);
            }
        }
        Value::Array(items) => {
            for item in items {
                render_value_templates(item, bindings);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                render_value_templates(value, bindings);
            }
        }
        _ => {}
    }
}

fn render_template_value(raw: &str, bindings: &HashMap<String, Value>) -> Option<Value> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("{{")?.strip_suffix("}}")?.trim();
    let name = inner.strip_prefix("outputs.")?.trim();
    bindings.get(name).cloned()
}

fn render_template_string(raw: &str, bindings: &HashMap<String, Value>) -> String {
    let mut rendered = raw.to_string();
    for (name, value) in bindings {
        let replacement = template_replacement(value);
        let compact = format!("{{{{outputs.{name}}}}}");
        let spaced = format!("{{{{ outputs.{name} }}}}");
        rendered = rendered.replace(&compact, &replacement);
        rendered = rendered.replace(&spaced, &replacement);
    }
    rendered
}

fn template_replacement(value: &Value) -> String {
    if value.is_null() {
        return String::new();
    }
    if let Some(value) = value.as_str() {
        return value.to_string();
    }
    if matches!(
        value_type_name(value),
        "number" | "bool" | "array" | "object"
    ) {
        return value.to_string();
    }
    String::new()
}

fn mark_generated_from_outputs(
    request: &mut AgentTaskRequest,
    dependencies: &AgentTaskOutputDependencies,
    bindings: &HashMap<String, Value>,
) {
    if !request.metadata.is_object() {
        request.metadata = Value::Object(Map::new());
    }
    let metadata = request.metadata.as_object_mut().expect("metadata object");
    metadata.insert("generated_from_outputs".to_string(), Value::Bool(true));
    metadata.insert(
        "depends_on".to_string(),
        Value::Array(
            dependencies
                .depends_on
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    metadata.insert(
        "resolved_output_bindings".to_string(),
        Value::Object(
            bindings
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        ),
    );
}

fn event(
    task_id: &str,
    state: AgentTaskState,
    attempt: u32,
    message: Option<String>,
) -> AgentTaskProgressEvent {
    AgentTaskProgressEvent {
        task_id: task_id.to_string(),
        state,
        attempt,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        expand_agent_task_matrix, AgentTaskArtifact, AgentTaskExecutor, AgentTaskLimits,
        AgentTaskMatrixAggregate, AgentTaskMatrixAxis, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_ARTIFACT_SCHEMA,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[test]
    fn schedules_tasks_with_bounded_concurrency_and_success_aggregate() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(4);
        plan.options.max_concurrency = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.queued, 0);
        assert_eq!(aggregate.totals.succeeded, 4);
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
        assert!(aggregate
            .events
            .iter()
            .any(|event| event.state == AgentTaskState::Running));
    }

    #[test]
    fn preserves_partial_failure_evidence() {
        let mut statuses = HashMap::new();
        statuses.insert("task-2".to_string(), AgentTaskOutcomeStatus::Failed);
        let scheduler =
            AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::PartialFailure);
        assert_eq!(aggregate.totals.queued, 0);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(aggregate.totals.failed, 1);
        let failed = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "task-2")
            .expect("failed task outcome");
        assert_eq!(failed.evidence_refs[0].kind, "log");
    }

    #[test]
    fn failed_single_task_is_not_also_counted_as_queued() {
        let mut statuses = HashMap::new();
        statuses.insert("task-1".to_string(), AgentTaskOutcomeStatus::Failed);
        let scheduler =
            AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));

        let aggregate = scheduler.run(plan_with_tasks(1));

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(aggregate.totals.queued, 0);
        assert_eq!(aggregate.queue.queued, 0);
    }

    #[test]
    fn normalizes_slow_task_to_timeout() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(25),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(aggregate.totals.timed_out, 1);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Timeout
        );
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
    }

    #[test]
    fn timeout_with_completed_runtime_artifacts_is_discoverable_and_promotable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("task-1-artifacts");
        fs::create_dir_all(&artifact_root).expect("artifact root");
        let patch_path = artifact_root.join("fix.patch");
        fs::write(&patch_path, "diff --git a/a.txt b/a.txt\n").expect("patch");
        fs::write(artifact_root.join("transcript.log"), "runtime completed").expect("log");
        let agent_result_path = artifact_root.join("agent-result.json");
        fs::write(
            &agent_result_path,
            serde_json::to_string(&AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "task-1".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("patch ready".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "fix".to_string(),
                    kind: "patch".to_string(),
                    name: Some("fix.patch".to_string()),
                    path: Some(patch_path.display().to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: None,
                    sha256: None,
                    metadata: json!({ "role": "patch" }),
                }],
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "runtime_bundle".to_string(),
                    uri: artifact_root.display().to_string(),
                    label: Some("runtime bundle".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: json!({}),
            })
            .expect("agent result json"),
        )
        .expect("agent result");

        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(250),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(aggregate.totals.timed_out, 0);
        assert!(aggregate
            .events
            .iter()
            .any(|event| event.task_id == "task-1" && event.state == AgentTaskState::Succeeded));
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert!(outcome.artifacts.iter().any(|artifact| {
            artifact.kind == "patch"
                && artifact.path.as_deref() == Some(&patch_path.to_string_lossy())
        }));
        assert!(outcome
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "transcript"));
        assert!(outcome
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "agent_result"
                && evidence.uri == agent_result_path.display().to_string()));
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "completed_runtime_late_provider_race"
                && diagnostic.data.get("timeout_kind").and_then(Value::as_str)
                    == Some("scheduler_timeout")
                && diagnostic
                    .data
                    .get("actionable_patch")
                    .and_then(Value::as_bool)
                    == Some(true)
        }));
    }

    #[test]
    fn timeout_with_empty_patch_artifacts_and_actionable_false_stays_timed_out() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("task-1-artifacts");
        fs::create_dir_all(&artifact_root).expect("artifact root");
        let patch_path = artifact_root.join("patch.diff");
        let mounted_patch_path = artifact_root.join("mount-5.patch");
        fs::write(&patch_path, "").expect("patch diff");
        fs::write(&mounted_patch_path, "").expect("mounted patch");
        fs::write(artifact_root.join("transcript.log"), "runtime completed").expect("log");
        fs::write(
            artifact_root.join("agent-result.json"),
            serde_json::to_string(&json!({
                "schema": AGENT_TASK_OUTCOME_SCHEMA,
                "task_id": "task-1",
                "status": "succeeded",
                "summary": "runtime produced no actionable patch",
                "actionable": false,
                "artifacts": [
                    {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "patch",
                        "kind": "patch",
                        "name": "patch.diff",
                        "path": patch_path.display().to_string(),
                        "mime": "text/x-diff",
                        "metadata": { "role": "patch" }
                    },
                    {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "mount-5",
                        "kind": "patch",
                        "name": "mount-5.patch",
                        "path": mounted_patch_path.display().to_string(),
                        "mime": "text/x-patch",
                        "metadata": { "role": "patch" }
                    }
                ],
                "evidence_refs": [],
                "diagnostics": [],
                "metadata": {}
            }))
            .expect("agent result json"),
        )
        .expect("agent result");

        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(250),
        ));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].limits.timeout_ms = Some(1);
        plan.tasks[0].metadata = json!({ "artifact_root": artifact_root });

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(aggregate.totals.succeeded, 0);
        assert_eq!(aggregate.totals.timed_out, 1);
        assert!(aggregate
            .events
            .iter()
            .any(|event| event.task_id == "task-1" && event.state == AgentTaskState::TimedOut));
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
        assert_eq!(
            outcome.failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
        assert!(outcome.artifacts.iter().any(|artifact| {
            artifact.kind == "patch"
                && artifact.path.as_deref() == Some(&patch_path.to_string_lossy())
        }));
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "completed_runtime_late_provider_race"
                && diagnostic
                    .data
                    .get("actionable_patch")
                    .and_then(Value::as_bool)
                    == Some(false)
        }));
    }

    #[test]
    fn retries_failed_tasks_until_success() {
        let executor = RetryOnceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.retry.max_attempts = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-1" && event.state == AgentTaskState::Queued && event.attempt == 2
        }));
    }

    #[test]
    fn blocks_tasks_over_queue_depth_and_reports_backpressure() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let mut plan = plan_with_tasks(3);
        plan.options.max_queue_depth = Some(2);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::PartialFailure);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(aggregate.totals.blocked, 1);
        assert_eq!(aggregate.queue.blocked, 1);
        assert_eq!(aggregate.queue.max_queue_depth, Some(2));
        assert!(aggregate
            .queue
            .backpressure
            .iter()
            .any(|status| status.kind == "queue_depth"));
        assert!(aggregate
            .events
            .iter()
            .any(|event| { event.task_id == "task-3" && event.state == AgentTaskState::Blocked }));
    }

    #[test]
    fn applies_per_executor_concurrency_below_global_limit() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 3;
        plan.options
            .per_executor_concurrency
            .insert("test".to_string(), 1);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert!(max_seen.load(Ordering::SeqCst) <= 1);
        assert_eq!(
            aggregate.queue.per_executor_concurrency.get("test"),
            Some(&1)
        );
    }

    #[test]
    fn resource_budget_limits_concurrent_task_cost() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(4);
        plan.options.max_concurrency = 4;
        plan.options.resource_budget.max_active_units = Some(2);
        plan.options.resource_budget.default_task_units = 1;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 4);
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
        assert_eq!(aggregate.queue.resource_budget.max_active_units, Some(2));
        assert_eq!(aggregate.queue.resource_budget.default_task_units, 1);
    }

    #[test]
    fn resource_budget_blocks_task_that_cannot_fit() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let mut plan = plan_with_tasks(1);
        plan.options.resource_budget.max_active_units = Some(2);
        plan.options.resource_budget.default_task_units = 3;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(aggregate.totals.blocked, 1);
        assert_eq!(aggregate.queue.blocked, 1);
        assert!(aggregate
            .queue
            .backpressure
            .iter()
            .any(|status| status.kind == "resource_budget"));
    }

    #[test]
    fn adaptive_concurrency_scales_up_when_runner_slots_are_available() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(4);
        plan.options.max_concurrency = 1;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
            max_concurrency: Some(3),
            runner_capacity: Some(3),
            ..AgentTaskAdaptiveConcurrencyPolicy::default()
        });

        let aggregate = scheduler.run(plan);
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert!(max_seen.load(Ordering::SeqCst) > 1);
        assert!(max_seen.load(Ordering::SeqCst) <= 3);
        assert_eq!(adaptive.configured_max_concurrency, 1);
        assert_eq!(adaptive.max_concurrency, 3);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Increased
                && decision.effective_concurrency == 3
                && decision.reason.contains("runner slots are available")
        }));
    }

    #[test]
    fn adaptive_concurrency_scales_down_under_runner_pressure() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 4;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
            max_concurrency: Some(4),
            runner_capacity: Some(3),
            active_leases: 2,
            ..AgentTaskAdaptiveConcurrencyPolicy::default()
        });

        let aggregate = scheduler.run(plan);
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert!(max_seen.load(Ordering::SeqCst) <= 1);
        assert_eq!(adaptive.effective_concurrency, 1);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Decreased
                && decision.reason.contains("available runner slots 1")
        }));
    }

    #[test]
    fn adaptive_concurrency_pauses_and_blocks_when_runner_capacity_is_unavailable() {
        let executor = RecordingExecutor {
            statuses: HashMap::new(),
            delay: Duration::from_millis(0),
            running: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
            cancel_calls: Arc::new(Mutex::new(Vec::new())),
        };
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(2);
        plan.options.max_concurrency = 2;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy {
            runner_capacity: Some(1),
            active_leases: 1,
            ..AgentTaskAdaptiveConcurrencyPolicy::default()
        });

        let aggregate = scheduler.run(plan);
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(aggregate.totals.blocked, 2);
        assert_eq!(max_seen.load(Ordering::SeqCst), 0);
        assert_eq!(adaptive.effective_concurrency, 0);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Paused
                && decision.reason.contains("consume runner_capacity=1")
        }));
        assert!(aggregate
            .queue
            .backpressure
            .iter()
            .any(|status| status.kind == "adaptive_concurrency"));
    }

    #[test]
    fn adaptive_concurrency_status_records_held_decision() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let mut plan = plan_with_tasks(1);
        plan.options.max_concurrency = 2;
        plan.options.adaptive_concurrency = Some(AgentTaskAdaptiveConcurrencyPolicy::default());

        let aggregate = scheduler.run(plan);
        let adaptive = aggregate
            .queue
            .adaptive_concurrency
            .expect("adaptive status");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(adaptive.effective_concurrency, 2);
        assert!(adaptive.decisions.iter().any(|decision| {
            decision.action == AgentTaskAdaptiveConcurrencyAction::Held
                && decision.reason.contains("configured ceiling")
        }));
    }

    #[test]
    fn applies_per_model_concurrency_below_global_limit() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(25));
        let max_seen = Arc::clone(&executor.max_seen);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        for task in &mut plan.tasks {
            task.executor.model = Some("model-a".to_string());
        }
        plan.options.max_concurrency = 3;
        plan.options
            .per_model_concurrency
            .insert("test:model-a".to_string(), 1);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert!(max_seen.load(Ordering::SeqCst) <= 1);
        assert_eq!(
            aggregate.queue.per_model_concurrency.get("test:model-a"),
            Some(&1)
        );
    }

    #[test]
    fn runs_matrix_cells_through_generic_scheduler_and_preserves_axes() {
        let mut statuses = HashMap::new();
        statuses.insert(
            "fanout/site-smoke[model=gpt-5.5,prompt=site-b]".to_string(),
            AgentTaskOutcomeStatus::Failed,
        );
        let scheduler =
            AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));
        let matrix_plan = expand_agent_task_matrix(
            "fanout/site-smoke",
            vec![
                AgentTaskMatrixAxis {
                    name: "model".to_string(),
                    values: vec!["gpt-5.5".to_string(), "claude".to_string()],
                },
                AgentTaskMatrixAxis {
                    name: "prompt".to_string(),
                    values: vec!["site-a".to_string(), "site-b".to_string()],
                },
            ],
            request("template"),
        )
        .expect("matrix expands");
        let mut schedule_plan = AgentTaskPlan::new(
            matrix_plan.plan_id.clone(),
            matrix_plan
                .cells
                .iter()
                .map(|cell| cell.task.clone())
                .collect(),
        );
        schedule_plan.options.max_concurrency = 2;

        let schedule = scheduler.run(schedule_plan);
        let matrix = AgentTaskMatrixAggregate::from_outcomes(&matrix_plan, &schedule.outcomes);

        assert_eq!(schedule.plan_id, "fanout/site-smoke");
        assert_eq!(schedule.totals.succeeded, 3);
        assert_eq!(schedule.totals.failed, 1);
        assert_eq!(matrix.cells.len(), 4);
        assert!(!matrix.passed);
        let failed = matrix
            .cells
            .iter()
            .find(|cell| cell.status == Some(AgentTaskOutcomeStatus::Failed))
            .expect("failed matrix cell");
        assert_eq!(failed.axes["model"], "gpt-5.5");
        assert_eq!(failed.axes["prompt"], "site-b");
        assert_eq!(failed.evidence_refs[0].kind, "log");
    }

    #[test]
    fn retry_budget_and_failure_classifications_gate_retries() {
        let executor = RetryOnceExecutor::default();
        let attempts = Arc::clone(&executor.attempts);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.retry.max_attempts = 3;
        plan.options.retry.max_retries_total = Some(0);
        plan.options.retry.retryable_failure_classifications =
            vec![AgentTaskFailureClassification::Provider];

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(aggregate.queue.retry_budget_remaining, Some(0));
    }

    #[test]
    fn templates_prior_output_into_downstream_task_request() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let mut plan =
            AgentTaskPlan::new("plan-output-dag", vec![request("idea"), request("design")]);
        plan.options.max_concurrency = 2;
        plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
        plan.tasks[1].executor.config = json!({
            "github_issue": "{{outputs.issue_number}}",
            "instructions": "Use issue {{ outputs.issue_number }}"
        });
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(design.instructions, "Build design for issue #3447");
        assert_eq!(design.executor.config["github_issue"], json!(3447));
        assert_eq!(design.executor.config["instructions"], "Use issue 3447");
        assert_eq!(design.metadata["generated_from_outputs"], json!(true));
        assert_eq!(
            design.metadata["resolved_output_bindings"]["issue_number"],
            json!(3447)
        );
        let idea_succeeded_index = aggregate
            .events
            .iter()
            .position(|event| event.task_id == "idea" && event.state == AgentTaskState::Succeeded)
            .expect("idea succeeded event");
        let design_running_index = aggregate
            .events
            .iter()
            .position(|event| event.task_id == "design" && event.state == AgentTaskState::Running)
            .expect("design running event");
        assert!(idea_succeeded_index < design_running_index);
    }

    #[test]
    fn binds_typed_artifact_payload_into_downstream_task_request() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-dag",
            vec![request("idea"), request("design")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[1].inputs = json!({ "packet": "{{outputs.concept_packet}}" });
        plan.artifact_outputs.insert(
            "idea".to_string(),
            vec![AgentTaskArtifactOutputDeclaration {
                name: "concept_packet".to_string(),
                kind: "concept_packet".to_string(),
                schema: Some("example/concept-packet/v1".to_string()),
                artifact_id: None,
                payload_path: Some("/title".to_string()),
            }],
        );
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "concept_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "concept_packet".to_string(),
                            schema: Some("example/concept-packet/v1".to_string()),
                            artifact_id: Some("concept".to_string()),
                            payload_path: Some("/title".to_string()),
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(design.inputs["packet"], json!("Demo concept"));
        assert_eq!(aggregate.artifact_lineage.len(), 1);
        assert_eq!(aggregate.artifact_lineage[0].name, "concept_packet");
        assert_eq!(aggregate.artifact_lineage[0].payload, json!("Demo concept"));
    }

    #[test]
    fn skips_required_typed_artifact_binding_when_artifact_is_missing() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-skip",
            vec![request("idea"), request("design")],
        );
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "finding_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "finding_packet".to_string(),
                            schema: None,
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::PartialFailure);
        assert!(observed.iter().all(|request| request.task_id != "design"));
        let skipped = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "design")
            .expect("skipped outcome");
        assert!(skipped.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "output_dependency_missing"
                && diagnostic.message.contains("required artifact binding")
        }));
    }

    #[test]
    fn optional_typed_artifact_binding_uses_default() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-artifact-default",
            vec![request("idea"), request("design")],
        );
        plan.tasks[1].inputs = json!({ "packet": "{{outputs.finding_packet}}" });
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "finding_packet".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: String::new(),
                        artifact: Some(AgentTaskArtifactBinding {
                            kind: "finding_packet".to_string(),
                            schema: None,
                            artifact_id: None,
                            payload_path: None,
                        }),
                        required: false,
                        default: json!({ "findings": [] }),
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(design.inputs["packet"], json!({ "findings": [] }));
    }

    #[test]
    fn skips_downstream_task_when_required_output_is_missing() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: false,
        });
        let mut plan =
            AgentTaskPlan::new("plan-output-skip", vec![request("idea"), request("design")]);
        plan.options.max_concurrency = 2;
        plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::PartialFailure);
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(aggregate.totals.skipped, 1);
        assert_eq!(aggregate.totals.failed, 0);
        assert!(observed.iter().all(|request| request.task_id != "design"));
        assert!(aggregate
            .events
            .iter()
            .any(|event| { event.task_id == "design" && event.state == AgentTaskState::Skipped }));
        let skipped = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "design")
            .expect("skipped outcome");
        assert_eq!(skipped.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(
            skipped.failure_classification,
            Some(AgentTaskFailureClassification::InvalidInput)
        );
        assert!(skipped.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "output_dependency_missing"
                && diagnostic.message.contains("required output binding")
        }));
    }

    #[test]
    fn static_batch_plans_remain_compatible_without_output_dependencies() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let plan_json = serde_json::to_string(&plan_with_tasks(2)).expect("plan json");
        let plan: AgentTaskPlan = serde_json::from_str(&plan_json).expect("static plan decodes");

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(aggregate.totals.skipped, 0);
        assert_eq!(aggregate.queue.max_concurrency, 1);
        assert!(aggregate.queue.adaptive_concurrency.is_none());
    }

    #[test]
    fn cancellation_stops_queued_tasks_and_notifies_running_executor() {
        let executor = RecordingExecutor::new(HashMap::new(), Duration::from_millis(100));
        let cancel_calls = Arc::clone(&executor.cancel_calls);
        let running = Arc::clone(&executor.running);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 1;
        let token = AgentTaskCancellationToken::default();
        let worker_token = token.clone();

        let handle = thread::spawn(move || scheduler.run_with_cancellation(plan, worker_token));
        while running.load(Ordering::SeqCst) == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        token.cancel();
        let aggregate = handle.join().expect("scheduler thread");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Cancelled);
        assert!(aggregate.totals.cancelled >= 2);
        assert!(cancel_calls
            .lock()
            .expect("cancel calls")
            .contains(&"task-1".to_string()));
    }

    #[derive(Default)]
    struct RetryOnceExecutor {
        attempts: Arc<AtomicUsize>,
    }

    impl AgentTaskExecutorAdapter for RetryOnceExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let status = if attempt == 1 {
                AgentTaskOutcomeStatus::Failed
            } else {
                AgentTaskOutcomeStatus::Succeeded
            };

            outcome(request.task_id, status)
        }
    }

    struct RecordingExecutor {
        statuses: HashMap<String, AgentTaskOutcomeStatus>,
        delay: Duration,
        running: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
        cancel_calls: Arc<Mutex<Vec<String>>>,
    }

    struct OutputTemplateExecutor {
        observed: Arc<Mutex<Vec<AgentTaskRequest>>>,
        include_issue_number: bool,
    }

    impl AgentTaskExecutorAdapter for OutputTemplateExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.observed
                .lock()
                .expect("observed requests")
                .push(request.clone());
            let metadata = if request.task_id == "idea" && self.include_issue_number {
                json!({ "github": { "issue_number": 3447 } })
            } else {
                json!({})
            };
            let outputs = if request.task_id == "idea" && self.include_issue_number {
                json!({ "issue_number": 3447 })
            } else {
                Value::Null
            };

            let artifacts = if request.task_id == "idea" && self.include_issue_number {
                vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "concept".to_string(),
                    kind: "concept_packet".to_string(),
                    name: Some("concept.json".to_string()),
                    path: Some("artifacts/concept.json".to_string()),
                    url: None,
                    mime: Some("application/json".to_string()),
                    size_bytes: None,
                    sha256: Some("sha256:concept".to_string()),
                    metadata: json!({
                        "payload_schema": "example/concept-packet/v1",
                        "payload": { "title": "Demo concept" }
                    }),
                }]
            } else {
                Vec::new()
            };

            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts,
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs,
                workflow: None,
                follow_up: None,
                metadata,
            }
        }
    }

    impl RecordingExecutor {
        fn new(statuses: HashMap<String, AgentTaskOutcomeStatus>, delay: Duration) -> Self {
            Self {
                statuses,
                delay,
                running: Arc::new(AtomicUsize::new(0)),
                max_seen: Arc::new(AtomicUsize::new(0)),
                cancel_calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl AgentTaskExecutorAdapter for RecordingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(running, Ordering::SeqCst);

            let deadline = Instant::now() + self.delay;
            while Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }

            self.running.fetch_sub(1, Ordering::SeqCst);
            outcome(
                request.task_id.clone(),
                *self
                    .statuses
                    .get(&request.task_id)
                    .unwrap_or(&AgentTaskOutcomeStatus::Succeeded),
            )
        }

        fn cancel(&self, task_id: &str) {
            self.cancel_calls
                .lock()
                .expect("cancel calls")
                .push(task_id.to_string());
        }
    }

    fn plan_with_tasks(count: usize) -> AgentTaskPlan {
        let mut tasks = Vec::new();
        for index in 1..=count {
            tasks.push(request(&format!("task-{index}")));
        }
        AgentTaskPlan::new("plan-1", tasks)
    }

    fn request(task_id: &str) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: crate::core::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            group_key: None,
            parent_plan_id: Some("plan-1".to_string()),
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "do the task".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            metadata: Value::Null,
        }
    }

    fn outcome(task_id: String, status: AgentTaskOutcomeStatus) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id,
            status,
            summary: Some(format!("{status:?}")),
            failure_classification: match status {
                AgentTaskOutcomeStatus::Failed => {
                    Some(AgentTaskFailureClassification::ExecutionFailed)
                }
                _ => None,
            },
            artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "log".to_string(),
                uri: "artifact://task/log".to_string(),
                label: Some("task log".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: json!({}),
        }
    }
}

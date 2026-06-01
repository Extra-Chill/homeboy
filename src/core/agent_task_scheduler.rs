use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task::{
    AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification, AgentTaskOutcome,
    AgentTaskOutcomeStatus, AgentTaskRequest, AGENT_TASK_OUTCOME_SCHEMA,
};

pub const AGENT_TASK_PLAN_SCHEMA: &str = "homeboy/agent-task-plan/v1";
pub const AGENT_TASK_AGGREGATE_SCHEMA: &str = "homeboy/agent-task-aggregate/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPlan {
    #[serde(default = "plan_schema")]
    pub schema: String,
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    pub tasks: Vec<AgentTaskRequest>,
    #[serde(default)]
    pub options: AgentTaskScheduleOptions,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

impl AgentTaskPlan {
    pub fn new(plan_id: impl Into<String>, tasks: Vec<AgentTaskRequest>) -> Self {
        Self {
            schema: AGENT_TASK_PLAN_SCHEMA.to_string(),
            plan_id: plan_id.into(),
            group_key: None,
            tasks,
            options: AgentTaskScheduleOptions::default(),
            metadata: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskScheduleOptions {
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tasks: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_queue_depth: Option<usize>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_executor_concurrency: HashMap<String, usize>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_model_concurrency: HashMap<String, usize>,
    #[serde(default)]
    pub resource_budget: AgentTaskResourceBudget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub retry: AgentTaskRetryPolicy,
}

impl Default for AgentTaskScheduleOptions {
    fn default() -> Self {
        Self {
            max_concurrency: default_max_concurrency(),
            max_tasks: None,
            max_queue_depth: None,
            per_executor_concurrency: HashMap::new(),
            per_model_concurrency: HashMap::new(),
            resource_budget: AgentTaskResourceBudget::default(),
            timeout_ms: None,
            retry: AgentTaskRetryPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskResourceBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_units: Option<u32>,
    #[serde(default = "default_task_resource_units")]
    pub default_task_units: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_executor_task_units: HashMap<String, u32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_model_task_units: HashMap<String, u32>,
}

impl Default for AgentTaskResourceBudget {
    fn default() -> Self {
        Self {
            max_active_units: None,
            default_task_units: default_task_resource_units(),
            per_executor_task_units: HashMap::new(),
            per_model_task_units: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRetryPolicy {
    #[serde(default)]
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retryable_failure_classifications: Vec<AgentTaskFailureClassification>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskAggregate {
    #[serde(default = "aggregate_schema")]
    pub schema: String,
    pub plan_id: String,
    pub status: AgentTaskAggregateStatus,
    pub totals: AgentTaskAggregateTotals,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<AgentTaskOutcome>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentTaskProgressEvent>,
    #[serde(default)]
    pub queue: AgentTaskQueueStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskAggregateStatus {
    Succeeded,
    PartialFailure,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAggregateTotals {
    pub queued: usize,
    pub running: usize,
    pub blocked: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub timed_out: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskProgressEvent {
    pub task_id: String,
    pub state: AgentTaskState,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskState {
    Queued,
    Blocked,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskQueueStatus {
    pub max_concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tasks: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_queue_depth: Option<usize>,
    pub queued: usize,
    pub running: usize,
    pub blocked: usize,
    pub completed: usize,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_executor_concurrency: HashMap<String, usize>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_model_concurrency: HashMap<String, usize>,
    #[serde(default)]
    pub resource_budget: AgentTaskResourceBudgetStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backpressure: Vec<AgentTaskBackpressureStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_budget_remaining: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskBackpressureStatus {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskResourceBudgetStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_units: Option<u32>,
    pub default_task_units: u32,
    pub active_units: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_executor_task_units: HashMap<String, u32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_model_task_units: HashMap<String, u32>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentTaskCancellationToken {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl AgentTaskCancellationToken {
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
pub struct AgentTaskExecutionContext {
    pub plan_id: String,
    pub attempt: u32,
    pub cancellation: AgentTaskCancellationToken,
}

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
        let mut events = Vec::new();
        let mut cancellation_notified = false;
        let max_attempts = plan.options.retry.max_attempts.max(1);
        let (tx, rx) = mpsc::channel();

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

            while running.len() < max_concurrency
                && !queued.is_empty()
                && !cancellation.is_cancelled()
            {
                let Some(next_index) = AgentTaskScheduleSupport::next_dispatchable_index(
                    &queued,
                    &running,
                    &plan.options.per_executor_concurrency,
                    &plan.options.per_model_concurrency,
                    &plan.options.resource_budget,
                ) else {
                    if running.is_empty() {
                        if let Some(task) = queued.pop_front() {
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
                            backpressure.push(AgentTaskBackpressureStatus {
                                kind: "resource_budget".to_string(),
                                message: message.clone(),
                                task_id: Some(task.request.task_id.clone()),
                            });
                            events.push(event(
                                &task.request.task_id,
                                AgentTaskState::Blocked,
                                task.attempt,
                                Some(message.clone()),
                            ));
                            outcomes.push(AgentTaskScheduleSupport::blocked_outcome(
                                task.request.task_id,
                                message,
                            ));
                            blocked_count += 1;
                            continue;
                        }
                        break;
                    }
                    backpressure.push(AgentTaskBackpressureStatus {
                        kind: AgentTaskScheduleSupport::backpressure_kind(
                            &queued,
                            &running,
                            &plan.options.per_executor_concurrency,
                            &plan.options.per_model_concurrency,
                            &plan.options.resource_budget,
                        )
                        .to_string(),
                        message: "queued tasks are waiting for scheduler capacity".to_string(),
                        task_id: queued.front().map(|task| task.request.task_id.clone()),
                    });
                    break;
                };
                let scheduled = queued.remove(next_index).expect("queued task");
                let request = scheduled.request;
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
                    outcomes.push(outcome);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

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
                &backpressure,
                retry_budget_total.map(|budget| budget.saturating_sub(retry_budget_used)),
            ),
            outcomes,
            events,
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
        per_executor_concurrency: &HashMap<String, usize>,
        per_model_concurrency: &HashMap<String, usize>,
        resource_budget: &AgentTaskResourceBudget,
    ) -> Option<usize> {
        queued.iter().position(|task| {
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
                    running[index].started_at.elapsed() > Duration::from_millis(timeout_ms)
                })
                .unwrap_or(false);

            if !timed_out {
                index += 1;
                continue;
            }

            let task = running.remove(index);
            executor.cancel(&task.task_id);
            let outcome =
                Self::timeout_outcome(task.task_id.clone(), task.timeout_ms.unwrap_or_default());
            events.push(event(
                &task.task_id,
                AgentTaskState::TimedOut,
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
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }

    fn timeout_outcome(task_id: String, timeout_ms: u64) -> AgentTaskOutcome {
        AgentTaskOutcome {
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
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
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
            queued: total_tasks,
            ..AgentTaskAggregateTotals::default()
        };

        for outcome in outcomes {
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

fn plan_schema() -> String {
    AGENT_TASK_PLAN_SCHEMA.to_string()
}

fn aggregate_schema() -> String {
    AGENT_TASK_AGGREGATE_SCHEMA.to_string()
}

fn default_max_concurrency() -> usize {
    1
}

fn default_task_resource_units() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        expand_agent_task_matrix, AgentTaskExecutor, AgentTaskLimits, AgentTaskMatrixAggregate,
        AgentTaskMatrixAxis, AgentTaskPolicy, AgentTaskWorkspace,
    };
    use serde_json::json;
    use std::collections::HashMap;
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
            workflow: None,
            follow_up: None,
            metadata: json!({}),
        }
    }
}

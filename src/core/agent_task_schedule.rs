use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task::{AgentTaskFailureClassification, AgentTaskOutcome, AgentTaskRequest};

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

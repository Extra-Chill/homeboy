use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::core::agent_task::{
    AgentTaskComponentContract, AgentTaskFailureClassification, AgentTaskOutcome, AgentTaskRequest,
};
use crate::core::plan::{HomeboyPlan, PlanArtifact, PlanKind, PlanStep, PlanStepStatus};

pub const AGENT_TASK_PLAN_SCHEMA: &str = "homeboy/agent-task-plan/v1";
pub const AGENT_TASK_AGGREGATE_SCHEMA: &str = "homeboy/agent-task-aggregate/v1";

#[derive(Debug, Clone, PartialEq)]
pub struct AgentTaskPlan {
    pub schema: String,
    pub plan_id: String,
    pub group_key: Option<String>,
    pub tasks: Vec<AgentTaskRequest>,
    pub component_contracts: Vec<AgentTaskComponentContract>,
    pub output_dependencies: HashMap<String, AgentTaskOutputDependencies>,
    pub artifact_outputs: HashMap<String, Vec<AgentTaskArtifactOutputDeclaration>>,
    pub options: AgentTaskScheduleOptions,
    pub metadata: Value,
    pub homeboy_plan: HomeboyPlan,
}

impl AgentTaskPlan {
    pub fn new(plan_id: impl Into<String>, tasks: Vec<AgentTaskRequest>) -> Self {
        let mut plan = Self {
            schema: AGENT_TASK_PLAN_SCHEMA.to_string(),
            plan_id: plan_id.into(),
            group_key: None,
            tasks,
            component_contracts: Vec::new(),
            output_dependencies: HashMap::new(),
            artifact_outputs: HashMap::new(),
            options: AgentTaskScheduleOptions::default(),
            metadata: Value::Null,
            homeboy_plan: HomeboyPlan::default(),
        };
        plan.rebuild_homeboy_plan();
        plan
    }

    pub fn from_homeboy_plan(homeboy_plan: HomeboyPlan) -> Self {
        let schema = string_input(&homeboy_plan, "schema")
            .unwrap_or_else(|| AGENT_TASK_PLAN_SCHEMA.to_string());
        let plan_id =
            string_input(&homeboy_plan, "plan_id").unwrap_or_else(|| homeboy_plan.id.clone());
        let group_key = string_input(&homeboy_plan, "group_key");
        let tasks = homeboy_plan
            .steps
            .iter()
            .filter_map(|step| value_as(&step.inputs, "agent_task_request"))
            .collect();
        let component_contracts = homeboy_plan
            .inputs
            .get("component_contracts")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        let output_dependencies = homeboy_plan
            .steps
            .iter()
            .filter_map(|step| {
                let dependencies = value_as::<AgentTaskOutputDependencies>(
                    &step.inputs,
                    "agent_task_output_dependencies",
                )
                .unwrap_or_else(|| AgentTaskOutputDependencies {
                    depends_on: step.needs.clone(),
                    bindings: HashMap::new(),
                });
                (!dependencies.depends_on.is_empty() || !dependencies.bindings.is_empty())
                    .then(|| (step.id.clone(), dependencies))
            })
            .collect();
        let mut artifact_outputs: HashMap<String, Vec<AgentTaskArtifactOutputDeclaration>> =
            HashMap::new();
        for artifact in &homeboy_plan.artifacts {
            let Some(task_id) = string_data(artifact, "task_id") else {
                continue;
            };
            let Some(declaration) = artifact
                .data
                .get("agent_task_artifact_output")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
            else {
                continue;
            };
            artifact_outputs
                .entry(task_id)
                .or_default()
                .push(declaration);
        }
        let options = homeboy_plan
            .policy
            .get("agent_task_schedule_options")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        let metadata = homeboy_plan
            .inputs
            .get("metadata")
            .cloned()
            .unwrap_or(Value::Null);

        Self {
            schema,
            plan_id,
            group_key,
            tasks,
            component_contracts,
            output_dependencies,
            artifact_outputs,
            options,
            metadata,
            homeboy_plan,
        }
    }

    pub fn rebuild_homeboy_plan(&mut self) {
        self.homeboy_plan = self.to_homeboy_plan();
    }

    pub fn canonicalize(mut self) -> Self {
        self.rebuild_homeboy_plan();
        Self::from_homeboy_plan(self.homeboy_plan)
    }

    fn to_homeboy_plan(&self) -> HomeboyPlan {
        let mut plan = HomeboyPlan::for_description(PlanKind::AgentTask, self.plan_id.clone());
        plan.id = self.plan_id.clone();
        plan.inputs
            .insert("schema".to_string(), Value::String(self.schema.clone()));
        plan.inputs
            .insert("plan_id".to_string(), Value::String(self.plan_id.clone()));
        if let Some(group_key) = &self.group_key {
            plan.inputs
                .insert("group_key".to_string(), Value::String(group_key.clone()));
        }
        if !self.metadata.is_null() {
            plan.inputs
                .insert("metadata".to_string(), self.metadata.clone());
        }
        if !self.component_contracts.is_empty() {
            plan.inputs.insert(
                "component_contracts".to_string(),
                serde_json::to_value(&self.component_contracts).unwrap_or(Value::Null),
            );
        }
        plan.policy.insert(
            "agent_task_schedule_options".to_string(),
            serde_json::to_value(&self.options).unwrap_or(Value::Null),
        );
        plan.steps = self
            .tasks
            .iter()
            .map(|request| {
                let dependencies = self.output_dependencies.get(&request.task_id).cloned();
                let mut inputs = HashMap::from([(
                    "agent_task_request".to_string(),
                    serde_json::to_value(request).unwrap_or(Value::Null),
                )]);
                if let Some(dependencies) = &dependencies {
                    inputs.insert(
                        "agent_task_output_dependencies".to_string(),
                        serde_json::to_value(dependencies).unwrap_or(Value::Null),
                    );
                }
                PlanStep {
                    id: request.task_id.clone(),
                    kind: "agent_task".to_string(),
                    label: Some(request.task_id.clone()),
                    blocking: true,
                    scope: Vec::new(),
                    needs: dependencies
                        .as_ref()
                        .map(|dependencies| dependencies.depends_on.clone())
                        .unwrap_or_default(),
                    status: PlanStepStatus::Ready,
                    inputs,
                    outputs: HashMap::new(),
                    skip_reason: None,
                    policy: HashMap::new(),
                    missing: Vec::new(),
                }
            })
            .collect();
        for (task_id, declarations) in &self.artifact_outputs {
            for declaration in declarations {
                let mut data = HashMap::new();
                data.insert("task_id".to_string(), Value::String(task_id.clone()));
                data.insert("name".to_string(), Value::String(declaration.name.clone()));
                data.insert(
                    "agent_task_artifact_output".to_string(),
                    serde_json::to_value(declaration).unwrap_or(Value::Null),
                );
                plan.artifacts.push(PlanArtifact {
                    id: format!("{task_id}:{}", declaration.name),
                    path: declaration.payload_path.clone(),
                    artifact_type: Some(declaration.kind.clone()),
                    data,
                });
            }
        }
        plan
    }
}

impl Serialize for AgentTaskPlan {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        AgentTaskPlanJson::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AgentTaskPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let json = AgentTaskPlanJson::deserialize(deserializer)?;
        Ok(json.into_plan())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentTaskPlanJson {
    #[serde(default = "plan_schema")]
    schema: String,
    plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_key: Option<String>,
    tasks: Vec<AgentTaskRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    component_contracts: Vec<AgentTaskComponentContract>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    output_dependencies: HashMap<String, AgentTaskOutputDependencies>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    artifact_outputs: HashMap<String, Vec<AgentTaskArtifactOutputDeclaration>>,
    #[serde(default)]
    options: AgentTaskScheduleOptions,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    metadata: Value,
}

impl AgentTaskPlanJson {
    fn into_plan(self) -> AgentTaskPlan {
        let mut tasks = self.tasks;
        if !self.component_contracts.is_empty() {
            for task in &mut tasks {
                if task.component_contracts.is_empty() {
                    task.component_contracts = self.component_contracts.clone();
                }
            }
        }

        let mut plan = AgentTaskPlan {
            schema: self.schema,
            plan_id: self.plan_id,
            group_key: self.group_key,
            tasks,
            component_contracts: self.component_contracts,
            output_dependencies: self.output_dependencies,
            artifact_outputs: self.artifact_outputs,
            options: self.options,
            metadata: self.metadata,
            homeboy_plan: HomeboyPlan::default(),
        };
        plan.rebuild_homeboy_plan();
        plan
    }
}

impl From<&AgentTaskPlan> for AgentTaskPlanJson {
    fn from(plan: &AgentTaskPlan) -> Self {
        Self {
            schema: plan.schema.clone(),
            plan_id: plan.plan_id.clone(),
            group_key: plan.group_key.clone(),
            tasks: plan.tasks.clone(),
            component_contracts: plan.component_contracts.clone(),
            output_dependencies: plan.output_dependencies.clone(),
            artifact_outputs: plan.artifact_outputs.clone(),
            options: plan.options.clone(),
            metadata: plan.metadata.clone(),
        }
    }
}

fn string_input(plan: &HomeboyPlan, key: &str) -> Option<String> {
    plan.inputs
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn string_data(artifact: &PlanArtifact, key: &str) -> Option<String> {
    artifact
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn value_as<T: serde::de::DeserializeOwned>(
    values: &HashMap<String, Value>,
    key: &str,
) -> Option<T> {
    values
        .get(key)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskOutputDependencies {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub bindings: HashMap<String, AgentTaskOutputBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskOutputBinding {
    pub task_id: String,
    /// JSON Pointer into the prior `homeboy/agent-task-outcome/v1` object.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<AgentTaskArtifactBinding>,
    #[serde(default = "default_required_output")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub default: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskArtifactOutputDeclaration {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskArtifactBinding {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_path: Option<String>,
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
    pub adaptive_concurrency: Option<AgentTaskAdaptiveConcurrencyPolicy>,
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
            adaptive_concurrency: None,
            timeout_ms: None,
            retry: AgentTaskRetryPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAdaptiveConcurrencyPolicy {
    #[serde(default = "default_adaptive_min_concurrency")]
    pub min_concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_capacity: Option<usize>,
    #[serde(default)]
    pub active_leases: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_depth: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_pressure: Option<AgentTaskResourcePressure>,
    #[serde(default)]
    pub recent_failures: usize,
    #[serde(default)]
    pub recent_timeouts: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_on_pressure: Option<AgentTaskResourcePressure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_recent_failures: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_recent_timeouts: Option<usize>,
}

impl Default for AgentTaskAdaptiveConcurrencyPolicy {
    fn default() -> Self {
        Self {
            min_concurrency: default_adaptive_min_concurrency(),
            max_concurrency: None,
            runner_capacity: None,
            active_leases: 0,
            queue_depth: None,
            resource_pressure: None,
            recent_failures: 0,
            recent_timeouts: 0,
            pause_on_pressure: None,
            pause_after_recent_failures: None,
            pause_after_recent_timeouts: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskResourcePressure {
    Low,
    Normal,
    High,
    Critical,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_lineage: Vec<AgentTaskArtifactLineage>,
    #[serde(default)]
    pub queue: AgentTaskQueueStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskArtifactLineage {
    pub task_id: String,
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
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
    #[serde(default)]
    pub queued: usize,
    #[serde(default)]
    pub running: usize,
    #[serde(default)]
    pub blocked: usize,
    #[serde(default)]
    pub skipped: usize,
    #[serde(default)]
    pub succeeded: usize,
    #[serde(default)]
    pub failed: usize,
    #[serde(default)]
    pub cancelled: usize,
    #[serde(default)]
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
    Skipped,
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
    pub adaptive_concurrency: Option<AgentTaskAdaptiveConcurrencyStatus>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAdaptiveConcurrencyStatus {
    pub configured_max_concurrency: usize,
    pub effective_concurrency: usize,
    pub min_concurrency: usize,
    pub max_concurrency: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<AgentTaskAdaptiveConcurrencyDecision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAdaptiveConcurrencyDecision {
    pub action: AgentTaskAdaptiveConcurrencyAction,
    pub effective_concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_effective_concurrency: Option<usize>,
    pub reason: String,
    pub inputs: AgentTaskAdaptiveConcurrencyInputs,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskAdaptiveConcurrencyAction {
    Increased,
    Decreased,
    Held,
    Paused,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAdaptiveConcurrencyInputs {
    pub queued: usize,
    pub running: usize,
    pub configured_max_concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_capacity: Option<usize>,
    pub active_leases: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_depth: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_pressure: Option<AgentTaskResourcePressure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_units: Option<u32>,
    pub active_units: u32,
    pub default_task_units: u32,
    pub recent_failures: usize,
    pub recent_timeouts: usize,
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

fn default_adaptive_min_concurrency() -> usize {
    1
}

fn default_task_resource_units() -> u32 {
    1
}

fn default_required_output() -> bool {
    true
}

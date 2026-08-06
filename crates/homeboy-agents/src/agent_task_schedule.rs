use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::agent_task::{AgentTaskComponentContract, AgentTaskOutcome, AgentTaskRequest};
use homeboy_core::plan::{
    HomeboyPlan, PlanArtifact, PlanKind, PlanStep, PlanStepDependencyKind, PlanStepStatus,
};
use homeboy_core::workspace_claim::{WorkspaceIdentity, WorkspaceOwnerLease};

mod plan {
    use super::*;

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
        pub postprocess_steps: Vec<AgentTaskArtifactPostprocessStep>,
        pub services: Vec<AgentTaskManagedService>,
        pub options: AgentTaskScheduleOptions,
        pub metadata: Value,
        pub workspace_identity: Option<WorkspaceIdentity>,
        pub workspace_lifecycle_revision: u64,
        pub workspace_owner_lease: Option<WorkspaceOwnerLease>,
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
                postprocess_steps: Vec::new(),
                services: Vec::new(),
                options: AgentTaskScheduleOptions::default(),
                metadata: Value::Null,
                workspace_identity: None,
                workspace_lifecycle_revision: 0,
                workspace_owner_lease: None,
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
            let postprocess_steps = homeboy_plan
                .steps
                .iter()
                .filter(|step| step.kind == "artifact_postprocess")
                .filter_map(|step| {
                    value_as::<homeboy_core::artifacts::ArtifactPostprocessPlan>(
                        &step.inputs,
                        "artifact_postprocess_plan",
                    )
                    .map(|plan| AgentTaskArtifactPostprocessStep {
                        id: step.id.clone(),
                        depends_on: step.needs.clone(),
                        required: step.blocking,
                        plan,
                    })
                })
                .collect();
            let metadata = homeboy_plan
                .inputs
                .get("metadata")
                .cloned()
                .unwrap_or(Value::Null);
            let services = homeboy_plan
                .inputs
                .get("managed_services")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();

            Self {
                schema,
                plan_id,
                group_key,
                tasks,
                component_contracts,
                output_dependencies,
                artifact_outputs,
                postprocess_steps,
                services,
                options,
                metadata,
                workspace_identity: None,
                workspace_lifecycle_revision: 0,
                workspace_owner_lease: None,
                homeboy_plan,
            }
        }

        pub fn rebuild_homeboy_plan(&mut self) {
            self.homeboy_plan = self.to_homeboy_plan();
        }

        /// Validate the portable managed-service declarations before an
        /// execution host or Lab handoff can admit the plan.
        pub fn validate_managed_services(&self) -> Result<(), String> {
            for service in &self.services {
                service.validate_cleanup_deadline()?;
            }
            Ok(())
        }

        pub fn canonicalize(mut self) -> Self {
            for task in &mut self.tasks {
                if task.limits.timeout_ms.is_none() {
                    task.limits.timeout_ms = self.options.timeout_ms;
                }
            }
            self.rebuild_homeboy_plan();
            let mut canonical = Self::from_homeboy_plan(self.homeboy_plan);
            // Workspace authority is plan-level durable state, not a step input.
            // Keep it through the generic Homeboy-plan normalization projection.
            canonical.workspace_identity = self.workspace_identity;
            canonical.workspace_lifecycle_revision = self.workspace_lifecycle_revision;
            canonical.workspace_owner_lease = self.workspace_owner_lease;
            canonical
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
            if !self.services.is_empty() {
                plan.inputs.insert(
                    "managed_services".to_string(),
                    serde_json::to_value(&self.services).unwrap_or(Value::Null),
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
                        needs_kind: PlanStepDependencyKind::Execution,
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
            plan.steps
                .extend(self.postprocess_steps.iter().map(|step| PlanStep {
                    id: step.id.clone(),
                    kind: "artifact_postprocess".to_string(),
                    label: Some(step.id.clone()),
                    blocking: step.required,
                    scope: Vec::new(),
                    needs: step.depends_on.clone(),
                    needs_kind: PlanStepDependencyKind::Execution,
                    status: PlanStepStatus::Ready,
                    inputs: HashMap::from([(
                        "artifact_postprocess_plan".to_string(),
                        serde_json::to_value(&step.plan).unwrap_or(Value::Null),
                    )]),
                    outputs: HashMap::new(),
                    skip_reason: None,
                    policy: HashMap::new(),
                    missing: Vec::new(),
                }));
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
        #[serde(default = "defaults::plan_schema")]
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        postprocess_steps: Vec<AgentTaskArtifactPostprocessStep>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        services: Vec<AgentTaskManagedService>,
        #[serde(default)]
        options: AgentTaskScheduleOptions,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        metadata: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_identity: Option<WorkspaceIdentity>,
        #[serde(default)]
        workspace_lifecycle_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_owner_lease: Option<WorkspaceOwnerLease>,
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
                postprocess_steps: self.postprocess_steps,
                services: self.services,
                options: self.options,
                metadata: self.metadata,
                workspace_identity: self.workspace_identity,
                workspace_lifecycle_revision: self.workspace_lifecycle_revision,
                workspace_owner_lease: self.workspace_owner_lease,
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
                postprocess_steps: plan.postprocess_steps.clone(),
                services: plan.services.clone(),
                options: plan.options.clone(),
                metadata: plan.metadata.clone(),
                workspace_identity: plan.workspace_identity.clone(),
                workspace_lifecycle_revision: plan.workspace_lifecycle_revision,
                workspace_owner_lease: plan.workspace_owner_lease.clone(),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct AgentTaskArtifactPostprocessStep {
        pub id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub depends_on: Vec<String>,
        #[serde(default = "default_artifact_postprocess_step_required")]
        pub required: bool,
        pub plan: homeboy_core::artifacts::ArtifactPostprocessPlan,
    }

    fn default_artifact_postprocess_step_required() -> bool {
        true
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
}

mod aggregate {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct AgentTaskAggregate {
        #[serde(default = "defaults::aggregate_schema")]
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub child_runs: Vec<AgentTaskChildRun>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub artifact_bindings: Vec<AgentTaskArtifactRunBinding>,
        #[serde(default)]
        pub queue: AgentTaskQueueStatus,
    }

    impl AgentTaskAggregate {
        /// Candidate aggregates record their controller decision on the winner.
        /// Consumers must use this rather than outcome order, which is completion
        /// order and can include cancelled or deferred siblings.
        pub fn selected_outcome(&self) -> Option<&AgentTaskOutcome> {
            let task_id = self.outcomes.iter().find_map(|outcome| {
                outcome.metadata["candidate_selection"]["selected_task_id"].as_str()
            })?;
            self.outcomes
                .iter()
                .find(|outcome| outcome.task_id == task_id)
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct AgentTaskChildRun {
        pub task_id: String,
        pub run_id: String,
        pub state: AgentTaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub provider: Option<String>,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        pub metadata: Value,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct AgentTaskArtifactRunBinding {
        pub task_id: String,
        pub run_id: String,
        pub artifact_id: String,
        pub kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub sha256: Option<String>,
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
        /// Backward-compatible aggregate projection for older durable records.
        CandidateRecoverable,
        PartialRecoverable,
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
        pub skipped: usize,
        #[serde(default)]
        pub succeeded: usize,
        #[serde(default)]
        pub candidate_recoverable: usize,
        #[serde(default)]
        pub recoverable_candidates: usize,
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
        CandidateRecoverable,
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
}

mod cancellation {
    use super::*;

    #[derive(Clone)]
    pub struct AgentTaskCancellationToken {
        inner: Arc<AgentTaskCancellationInner>,
    }

    struct AgentTaskCancellationInner {
        cancelled: AtomicBool,
        callbacks: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl Default for AgentTaskCancellationToken {
        fn default() -> Self {
            Self {
                inner: Arc::new(AgentTaskCancellationInner {
                    cancelled: AtomicBool::new(false),
                    callbacks: Mutex::new(Vec::new()),
                }),
            }
        }
    }

    impl fmt::Debug for AgentTaskCancellationToken {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("AgentTaskCancellationToken")
                .field("cancelled", &self.is_cancelled())
                .finish_non_exhaustive()
        }
    }

    impl AgentTaskCancellationToken {
        pub fn cancel(&self) {
            if self.inner.cancelled.swap(true, Ordering::SeqCst) {
                return;
            }

            let callbacks = self
                .inner
                .callbacks
                .lock()
                .expect("cancellation callbacks")
                .clone();
            for callback in callbacks {
                callback();
            }
        }

        pub(crate) fn is_cancelled(&self) -> bool {
            self.inner.cancelled.load(Ordering::SeqCst)
        }

        pub(crate) fn on_cancel(&self, callback: Arc<dyn Fn() + Send + Sync>) {
            let mut callbacks = self.inner.callbacks.lock().expect("cancellation callbacks");
            if self.is_cancelled() {
                drop(callbacks);
                callback();
                return;
            }

            callbacks.push(callback);
        }
    }

    #[derive(Debug, Clone)]
    pub struct AgentTaskExecutionContext {
        pub plan_id: String,
        pub run_id: Option<String>,
        pub attempt: u32,
        pub cancellation: AgentTaskCancellationToken,
    }
}

mod defaults {
    use super::*;

    pub fn plan_schema() -> String {
        AGENT_TASK_PLAN_SCHEMA.to_string()
    }

    pub fn aggregate_schema() -> String {
        AGENT_TASK_AGGREGATE_SCHEMA.to_string()
    }
}

pub use aggregate::*;
pub use cancellation::*;
pub use defaults::*;
pub use homeboy_core::agent_task_config::*;
pub use plan::*;

#[cfg(test)]
mod managed_service_plan_tests {
    use super::*;

    #[test]
    fn task_only_plans_remain_byte_compatible_and_services_round_trip_through_homeboy_plan() {
        let task_only = AgentTaskPlan::new("task-only", Vec::new());
        assert!(serde_json::to_value(&task_only)
            .unwrap()
            .get("services")
            .is_none());

        let mut plan = AgentTaskPlan::new("services", Vec::new());
        plan.services.push(AgentTaskManagedService {
            version: AgentTaskManagedService::VERSION,
            id: "preview".to_string(),
            command: vec!["server".to_string()],
            cwd: None,
            env: HashMap::new(),
            env_allowlist: Vec::new(),
            secret_env: vec!["TOKEN".to_string()],
            secret_env_plan: None,
            host: "127.0.0.1".to_string(),
            port: Some(3000),
            port_env: None,
            socket_handoff: false,
            readiness: None,
            cleanup_deadline_ms: AgentTaskManagedService::DEFAULT_CLEANUP_DEADLINE_MS,
            public_url: Some("https://preview.example.test".to_string()),
            browser_origin_probe: None,
            lifecycle: AgentTaskManagedServiceLifecycle::Plan,
            target: None,
        });
        plan.rebuild_homeboy_plan();
        let round_trip = AgentTaskPlan::from_homeboy_plan(plan.homeboy_plan.clone());
        assert_eq!(round_trip.services, plan.services);
    }

    #[test]
    fn managed_services_and_postprocess_steps_round_trip_together() {
        let mut plan = AgentTaskPlan::new("composed", Vec::new());
        plan.services.push(AgentTaskManagedService {
            version: AgentTaskManagedService::VERSION,
            id: "preview".to_string(),
            command: vec!["server".to_string()],
            cwd: None,
            env: HashMap::new(),
            env_allowlist: Vec::new(),
            secret_env: Vec::new(),
            secret_env_plan: None,
            host: "127.0.0.1".to_string(),
            port: Some(3000),
            port_env: None,
            socket_handoff: false,
            readiness: None,
            cleanup_deadline_ms: AgentTaskManagedService::DEFAULT_CLEANUP_DEADLINE_MS,
            public_url: None,
            browser_origin_probe: None,
            lifecycle: AgentTaskManagedServiceLifecycle::Plan,
            target: None,
        });
        plan.postprocess_steps
            .push(AgentTaskArtifactPostprocessStep {
                id: "postprocess".to_string(),
                depends_on: Vec::new(),
                required: true,
                plan: homeboy_core::artifacts::ArtifactPostprocessPlan {
                    schema: "homeboy/artifact-postprocess/v1".to_string(),
                    plan_id: "postprocess".to_string(),
                    artifact_roots: Vec::new(),
                    actions: Vec::new(),
                    reviewer_refs: Vec::new(),
                    metadata: Value::Null,
                },
            });

        plan.rebuild_homeboy_plan();
        let round_trip = AgentTaskPlan::from_homeboy_plan(plan.homeboy_plan.clone());
        assert_eq!(round_trip.services, plan.services);
        assert_eq!(round_trip.postprocess_steps, plan.postprocess_steps);
    }
}

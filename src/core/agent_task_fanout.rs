use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

use crate::core::agent_task::{AgentTaskAggregateReport, AgentTaskRequest};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskOutputDependencies, AgentTaskPlan,
    AgentTaskScheduleOptions, AgentTaskScheduler,
};
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep};

pub const AGENT_TASK_FANOUT_PLAN_SCHEMA: &str = "homeboy/agent-task-fanout-plan/v1";
pub const AGENT_TASK_FANOUT_AGGREGATE_SCHEMA: &str = "homeboy/agent-task-fanout-aggregate/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskFanoutPlan {
    #[serde(default = "fanout_plan_schema")]
    pub schema: String,
    pub fanout_id: String,
    pub plane: AgentTaskFanoutPlane,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    pub tasks: Vec<AgentTaskRequest>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub output_dependencies: HashMap<String, AgentTaskOutputDependencies>,
    #[serde(default)]
    pub options: AgentTaskScheduleOptions,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFanoutPlane {
    /// Many isolated execution units scheduled under one Homeboy fanout id.
    IsolatedTasks,
    /// A workflow of dependent tasks inside one logical execution unit.
    Workflow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskFanoutAggregate {
    #[serde(default = "fanout_aggregate_schema")]
    pub schema: String,
    pub fanout_id: String,
    pub plane: AgentTaskFanoutPlane,
    pub schedule: AgentTaskAggregate,
    pub reconciliation: AgentTaskAggregateReport,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

pub struct AgentTaskFanoutScheduler<E> {
    scheduler: AgentTaskScheduler<E>,
}

impl AgentTaskFanoutPlan {
    pub fn new(
        fanout_id: impl Into<String>,
        plane: AgentTaskFanoutPlane,
        tasks: Vec<AgentTaskRequest>,
    ) -> Self {
        Self {
            schema: AGENT_TASK_FANOUT_PLAN_SCHEMA.to_string(),
            fanout_id: fanout_id.into(),
            plane,
            group_key: None,
            tasks,
            output_dependencies: HashMap::new(),
            options: AgentTaskScheduleOptions::default(),
            metadata: Value::Null,
        }
    }

    pub fn to_schedule_plan(&self) -> AgentTaskPlan {
        let mut plan = AgentTaskPlan::new(
            self.fanout_id.clone(),
            self.tasks
                .iter()
                .cloned()
                .map(|mut request| {
                    request
                        .parent_plan_id
                        .get_or_insert_with(|| self.fanout_id.clone());
                    request.group_key.get_or_insert_with(|| {
                        self.group_key
                            .clone()
                            .unwrap_or_else(|| self.fanout_id.clone())
                    });
                    request.metadata =
                        metadata_with_fanout(request.metadata, &self.fanout_id, self.plane);
                    request
                })
                .collect(),
        );
        plan.group_key = self.group_key.clone();
        plan.output_dependencies = self.output_dependencies.clone();
        plan.options = self.options.clone();
        plan.metadata = metadata_with_fanout(self.metadata.clone(), &self.fanout_id, self.plane);
        plan
    }

    pub fn to_homeboy_plan(&self) -> HomeboyPlan {
        let mut plan =
            HomeboyPlan::for_description(PlanKind::AgentTaskFanout, self.fanout_id.clone());
        plan.id = self.fanout_id.clone();
        plan.mode = Some("agent_task_fanout".to_string());
        plan.inputs
            .insert("schema".to_string(), Value::String(self.schema.clone()));
        plan.inputs.insert(
            "fanout_id".to_string(),
            Value::String(self.fanout_id.clone()),
        );
        plan.inputs.insert(
            "plane".to_string(),
            serde_json::to_value(self.plane).expect("fanout plane serializes"),
        );
        if let Some(group_key) = &self.group_key {
            plan.inputs
                .insert("group_key".to_string(), Value::String(group_key.clone()));
        }
        if !self.metadata.is_null() {
            plan.inputs
                .insert("metadata".to_string(), self.metadata.clone());
        }
        plan.policy.insert(
            "options".to_string(),
            serde_json::to_value(&self.options).expect("fanout options serialize"),
        );
        plan.steps = self
            .tasks
            .iter()
            .map(|request| {
                let mut builder =
                    PlanStep::ready(request.task_id.clone(), "agent_task.fanout.task")
                        .label(request.task_id.clone())
                        .input_value(
                            "request",
                            serde_json::to_value(request).expect("agent task request serializes"),
                        );
                if let Some(dependencies) = self.output_dependencies.get(&request.task_id) {
                    builder = builder
                        .needs(fanout_dependency_step_ids(dependencies))
                        .input_value(
                            "output_dependencies",
                            serde_json::to_value(dependencies)
                                .expect("agent task output dependencies serialize"),
                        );
                }
                builder.build()
            })
            .collect();
        plan
    }

    pub fn from_homeboy_plan(plan: &HomeboyPlan) -> Result<Self, String> {
        let fanout_id = plan
            .inputs
            .get("fanout_id")
            .and_then(Value::as_str)
            .unwrap_or(plan.id.as_str())
            .to_string();
        let plane = plan
            .inputs
            .get("plane")
            .cloned()
            .ok_or_else(|| "HomeboyPlan fanout projection is missing plane".to_string())
            .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
        let tasks = plan
            .steps
            .iter()
            .map(|step| {
                step.inputs
                    .get("request")
                    .cloned()
                    .ok_or_else(|| format!("fanout step '{}' is missing request", step.id))
                    .and_then(|value| {
                        serde_json::from_value(value).map_err(|error| error.to_string())
                    })
            })
            .collect::<Result<Vec<AgentTaskRequest>, String>>()?;
        let output_dependencies = plan
            .steps
            .iter()
            .filter_map(|step| {
                step.inputs
                    .get("output_dependencies")
                    .cloned()
                    .map(|value| {
                        serde_json::from_value(value)
                            .map(|dependencies| (step.id.clone(), dependencies))
                            .map_err(|error| error.to_string())
                    })
            })
            .collect::<Result<HashMap<String, AgentTaskOutputDependencies>, String>>()?;
        let options = plan
            .policy
            .get("options")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();

        Ok(Self {
            schema: plan
                .inputs
                .get("schema")
                .and_then(Value::as_str)
                .unwrap_or(AGENT_TASK_FANOUT_PLAN_SCHEMA)
                .to_string(),
            fanout_id,
            plane,
            group_key: plan
                .inputs
                .get("group_key")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            tasks,
            output_dependencies,
            options,
            metadata: plan.inputs.get("metadata").cloned().unwrap_or(Value::Null),
        })
    }
}

fn fanout_dependency_step_ids(dependencies: &AgentTaskOutputDependencies) -> Vec<String> {
    let mut needs = dependencies.depends_on.clone();
    for binding in dependencies.bindings.values() {
        if !needs.contains(&binding.task_id) {
            needs.push(binding.task_id.clone());
        }
    }
    needs
}

impl AgentTaskFanoutAggregate {
    pub fn from_schedule(plan: &AgentTaskFanoutPlan, schedule: AgentTaskAggregate) -> Self {
        let reconciliation = AgentTaskAggregateReport::from(schedule.outcomes.as_slice());
        Self {
            schema: AGENT_TASK_FANOUT_AGGREGATE_SCHEMA.to_string(),
            fanout_id: plan.fanout_id.clone(),
            plane: plan.plane,
            schedule,
            reconciliation,
            metadata: metadata_with_fanout(plan.metadata.clone(), &plan.fanout_id, plan.plane),
        }
    }
}

impl<E> AgentTaskFanoutScheduler<E>
where
    E: AgentTaskExecutorAdapter,
{
    pub fn new(executor: E) -> Self {
        Self {
            scheduler: AgentTaskScheduler::new(executor),
        }
    }

    pub fn run(&self, plan: AgentTaskFanoutPlan) -> AgentTaskFanoutAggregate {
        let schedule_plan = plan.to_schedule_plan();
        let schedule = self.scheduler.run(schedule_plan);
        AgentTaskFanoutAggregate::from_schedule(&plan, schedule)
    }
}

fn metadata_with_fanout(metadata: Value, fanout_id: &str, plane: AgentTaskFanoutPlane) -> Value {
    let mut object = match metadata {
        Value::Object(object) => object,
        Value::Null => Map::new(),
        other => {
            let mut object = Map::new();
            object.insert("base".to_string(), other);
            object
        }
    };
    object.insert(
        "fanout".to_string(),
        serde_json::json!({
            "id": fanout_id,
            "plane": plane,
        }),
    );
    Value::Object(object)
}

fn fanout_plan_schema() -> String {
    AGENT_TASK_FANOUT_PLAN_SCHEMA.to_string()
}

fn fanout_aggregate_schema() -> String {
    AGENT_TASK_FANOUT_AGGREGATE_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy,
        AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskOutputBinding};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[test]
    fn isolated_fanout_stamps_plan_metadata_and_aggregates_reconciliation() {
        let scheduler = AgentTaskFanoutScheduler::new(RecordingExecutor::default());
        let mut plan = AgentTaskFanoutPlan::new(
            "fanout/audit-batch",
            AgentTaskFanoutPlane::IsolatedTasks,
            vec![request("finding-1"), request("finding-2")],
        );
        plan.group_key = Some("audit-batch".to_string());
        plan.options.max_concurrency = 2;

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.schema, AGENT_TASK_FANOUT_AGGREGATE_SCHEMA);
        assert_eq!(aggregate.fanout_id, "fanout/audit-batch");
        assert_eq!(aggregate.plane, AgentTaskFanoutPlane::IsolatedTasks);
        assert_eq!(aggregate.schedule.totals.succeeded, 2);
        assert_eq!(aggregate.reconciliation.summary.total, 2);
        assert_eq!(aggregate.reconciliation.summary.review_candidates, 2);
        assert!(aggregate.schedule.outcomes.iter().all(|outcome| {
            outcome.metadata["fanout"]["id"] == json!("fanout/audit-batch")
                && outcome.metadata["fanout"]["plane"] == json!("isolated_tasks")
        }));
    }

    #[test]
    fn workflow_fanout_preserves_output_dependencies_inside_one_plane() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskFanoutScheduler::new(RecordingExecutor {
            observed: Arc::clone(&observed),
        });
        let mut plan = AgentTaskFanoutPlan::new(
            "fanout/site-workflow",
            AgentTaskFanoutPlane::Workflow,
            vec![request("generate"), request("diagnose")],
        );
        plan.tasks[1].instructions = "Diagnose artifact {{outputs.artifact_id}}".to_string();
        plan.output_dependencies.insert(
            "diagnose".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "artifact_id".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "generate".to_string(),
                        path: "/outputs/artifact_id".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let diagnose = observed
            .iter()
            .find(|request| request.task_id == "diagnose")
            .expect("diagnose request dispatched");

        assert_eq!(aggregate.plane, AgentTaskFanoutPlane::Workflow);
        assert_eq!(aggregate.schedule.totals.succeeded, 2);
        assert_eq!(diagnose.instructions, "Diagnose artifact artifact-123");
        assert_eq!(
            diagnose.parent_plan_id.as_deref(),
            Some("fanout/site-workflow")
        );
        assert_eq!(diagnose.metadata["fanout"]["plane"], json!("workflow"));
        assert_eq!(diagnose.metadata["generated_from_outputs"], json!(true));
    }

    #[test]
    fn fanout_homeboy_plan_projection_preserves_schedule_compatibility() {
        let mut plan = AgentTaskFanoutPlan::new(
            "fanout/site-workflow",
            AgentTaskFanoutPlane::Workflow,
            vec![request("generate"), request("diagnose")],
        );
        plan.group_key = Some("site-workflow".to_string());
        plan.options.max_concurrency = 3;
        plan.metadata = json!({ "source": "compat-test" });
        plan.output_dependencies.insert(
            "diagnose".to_string(),
            AgentTaskOutputDependencies {
                depends_on: Vec::new(),
                bindings: HashMap::from([(
                    "artifact_id".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "generate".to_string(),
                        path: "/outputs/artifact_id".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );

        let homeboy_plan = plan.to_homeboy_plan();
        let projected =
            AgentTaskFanoutPlan::from_homeboy_plan(&homeboy_plan).expect("project fanout plan");

        assert_eq!(projected, plan);
        assert_eq!(homeboy_plan.id, "fanout/site-workflow");
        assert_eq!(homeboy_plan.kind, PlanKind::AgentTaskFanout);
        assert_eq!(homeboy_plan.steps.len(), 2);
        assert_eq!(homeboy_plan.steps[1].needs, vec!["generate".to_string()]);

        let schedule = projected.to_schedule_plan();
        assert_eq!(schedule.plan_id, "fanout/site-workflow");
        assert_eq!(schedule.group_key.as_deref(), Some("site-workflow"));
        assert_eq!(schedule.options.max_concurrency, 3);
        assert_eq!(schedule.output_dependencies, plan.output_dependencies);
        assert_eq!(
            schedule.tasks[0].parent_plan_id.as_deref(),
            Some("fanout/site-workflow")
        );
        assert_eq!(
            schedule.tasks[0].metadata["fanout"]["plane"],
            json!("workflow")
        );
    }

    #[derive(Default)]
    struct RecordingExecutor {
        observed: Arc<Mutex<Vec<AgentTaskRequest>>>,
    }

    impl AgentTaskExecutorAdapter for RecordingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.observed
                .lock()
                .expect("observed requests")
                .push(request.clone());
            let outputs = if request.task_id == "generate" {
                json!({ "artifact_id": "artifact-123" })
            } else {
                Value::Null
            };
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs,
                workflow: None,
                follow_up: None,
                metadata: request.metadata,
            }
        }
    }

    fn request(task_id: &str) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "do the task".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: Default::default(),
            expected_artifacts: Vec::new(),
            metadata: json!({}),
        }
    }
}

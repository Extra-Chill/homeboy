use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use super::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskOutcome,
    AgentTaskOutcomeStatus, AgentTaskRequest, AGENT_TASK_MATRIX_AGGREGATE_SCHEMA,
    AGENT_TASK_MATRIX_PLAN_SCHEMA,
};
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskMatrixAxis {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskMatrixPlan {
    #[serde(default = "matrix_plan_schema")]
    pub schema: String,
    pub plan_id: String,
    pub axes: Vec<AgentTaskMatrixAxis>,
    pub cells: Vec<AgentTaskMatrixCell>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskMatrixCell {
    pub cell_id: String,
    pub axes: BTreeMap<String, String>,
    pub task: AgentTaskRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskMatrixAggregate {
    #[serde(default = "matrix_aggregate_schema")]
    pub schema: String,
    pub plan_id: String,
    pub passed: bool,
    pub cells: Vec<AgentTaskMatrixAggregateCell>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskMatrixAggregateCell {
    pub cell_id: String,
    pub task_id: String,
    pub axes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentTaskOutcomeStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<AgentTaskArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<AgentTaskEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskDiagnostic>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskMatrixError {
    pub message: String,
}

impl AgentTaskMatrixPlan {
    pub fn to_homeboy_plan(&self) -> HomeboyPlan {
        let mut plan =
            HomeboyPlan::for_description(PlanKind::AgentTaskMatrix, self.plan_id.clone());
        plan.id = self.plan_id.clone();
        plan.mode = Some("agent_task_matrix".to_string());
        plan.inputs
            .insert("schema".to_string(), Value::String(self.schema.clone()));
        plan.inputs
            .insert("plan_id".to_string(), Value::String(self.plan_id.clone()));
        plan.inputs.insert(
            "axes".to_string(),
            serde_json::to_value(&self.axes).expect("matrix axes serialize"),
        );
        plan.steps = self
            .cells
            .iter()
            .map(|cell| {
                PlanStep::ready(cell.cell_id.clone(), "agent_task.matrix.cell")
                    .label(cell.cell_id.clone())
                    .input_value("cell_id", Value::String(cell.cell_id.clone()))
                    .input_value(
                        "axes",
                        serde_json::to_value(&cell.axes).expect("matrix cell axes serialize"),
                    )
                    .input_value(
                        "request",
                        serde_json::to_value(&cell.task).expect("agent task request serializes"),
                    )
                    .build()
            })
            .collect();
        plan
    }

    pub fn from_homeboy_plan(plan: &HomeboyPlan) -> Result<Self, AgentTaskMatrixError> {
        let plan_id = plan
            .inputs
            .get("plan_id")
            .and_then(Value::as_str)
            .unwrap_or(plan.id.as_str())
            .to_string();
        let axes = plan
            .inputs
            .get("axes")
            .cloned()
            .ok_or_else(|| {
                AgentTaskMatrixError::new("HomeboyPlan matrix projection is missing axes")
            })
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|error| AgentTaskMatrixError::new(error.to_string()))
            })?;
        let cells = plan
            .steps
            .iter()
            .map(|step| {
                let cell_id = step
                    .inputs
                    .get("cell_id")
                    .and_then(Value::as_str)
                    .unwrap_or(step.id.as_str())
                    .to_string();
                let axes = step
                    .inputs
                    .get("axes")
                    .cloned()
                    .ok_or_else(|| {
                        AgentTaskMatrixError::new(format!(
                            "matrix step '{}' is missing axes",
                            step.id
                        ))
                    })
                    .and_then(|value| {
                        serde_json::from_value(value)
                            .map_err(|error| AgentTaskMatrixError::new(error.to_string()))
                    })?;
                let task = step
                    .inputs
                    .get("request")
                    .cloned()
                    .ok_or_else(|| {
                        AgentTaskMatrixError::new(format!(
                            "matrix step '{}' is missing request",
                            step.id
                        ))
                    })
                    .and_then(|value| {
                        serde_json::from_value(value)
                            .map_err(|error| AgentTaskMatrixError::new(error.to_string()))
                    })?;
                Ok(AgentTaskMatrixCell {
                    cell_id,
                    axes,
                    task,
                })
            })
            .collect::<Result<Vec<AgentTaskMatrixCell>, AgentTaskMatrixError>>()?;

        Ok(Self {
            schema: plan
                .inputs
                .get("schema")
                .and_then(Value::as_str)
                .unwrap_or(AGENT_TASK_MATRIX_PLAN_SCHEMA)
                .to_string(),
            plan_id,
            axes,
            cells,
        })
    }
}

impl AgentTaskMatrixError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AgentTaskMatrixError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentTaskMatrixError {}

pub(crate) fn expand_agent_task_matrix(
    plan_id: impl Into<String>,
    axes: Vec<AgentTaskMatrixAxis>,
    template: AgentTaskRequest,
) -> Result<AgentTaskMatrixPlan, AgentTaskMatrixError> {
    let plan_id = plan_id.into();
    validate_matrix_id("plan id", &plan_id)?;
    validate_axes(&axes)?;

    let mut combinations = Vec::new();
    collect_matrix_combinations(&axes, 0, &mut BTreeMap::new(), &mut combinations);

    let cells = combinations
        .into_iter()
        .map(|selection| {
            let cell_id = matrix_cell_id(&plan_id, &axes, &selection);
            let mut task = template.clone();
            task.task_id = cell_id.clone();
            task.parent_plan_id = Some(plan_id.clone());
            task.group_key.get_or_insert_with(|| plan_id.clone());
            task.metadata = metadata_with_matrix(task.metadata, &cell_id, &selection);
            AgentTaskMatrixCell {
                cell_id,
                axes: selection,
                task,
            }
        })
        .collect();

    Ok(AgentTaskMatrixPlan {
        schema: AGENT_TASK_MATRIX_PLAN_SCHEMA.to_string(),
        plan_id,
        axes,
        cells,
    })
}

impl AgentTaskMatrixAggregate {
    pub(crate) fn from_outcomes(plan: &AgentTaskMatrixPlan, outcomes: &[AgentTaskOutcome]) -> Self {
        let outcomes_by_task: BTreeMap<&str, &AgentTaskOutcome> = outcomes
            .iter()
            .map(|outcome| (outcome.task_id.as_str(), outcome))
            .collect();
        let mut passed = true;
        let cells = plan
            .cells
            .iter()
            .map(|cell| {
                let outcome = outcomes_by_task.get(cell.task.task_id.as_str()).copied();
                if !matches!(
                    outcome.map(|outcome| outcome.status),
                    Some(AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp)
                ) {
                    passed = false;
                }
                AgentTaskMatrixAggregateCell {
                    cell_id: cell.cell_id.clone(),
                    task_id: cell.task.task_id.clone(),
                    axes: cell.axes.clone(),
                    status: outcome.map(|outcome| outcome.status),
                    artifacts: outcome
                        .map(|outcome| outcome.artifacts.clone())
                        .unwrap_or_default(),
                    evidence_refs: outcome
                        .map(|outcome| outcome.evidence_refs.clone())
                        .unwrap_or_default(),
                    diagnostics: outcome
                        .map(|outcome| outcome.diagnostics.clone())
                        .unwrap_or_default(),
                    metadata: outcome
                        .map(|outcome| outcome.metadata.clone())
                        .unwrap_or(Value::Null),
                }
            })
            .collect();

        Self {
            schema: AGENT_TASK_MATRIX_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            passed,
            cells,
        }
    }
}

fn matrix_plan_schema() -> String {
    AGENT_TASK_MATRIX_PLAN_SCHEMA.to_string()
}

fn matrix_aggregate_schema() -> String {
    AGENT_TASK_MATRIX_AGGREGATE_SCHEMA.to_string()
}

fn validate_axes(axes: &[AgentTaskMatrixAxis]) -> Result<(), AgentTaskMatrixError> {
    if axes.is_empty() {
        return Err(AgentTaskMatrixError::new(
            "matrix expansion requires at least one axis",
        ));
    }

    let mut names = BTreeSet::new();
    for axis in axes {
        validate_matrix_id("axis", &axis.name)?;
        if !names.insert(axis.name.as_str()) {
            return Err(AgentTaskMatrixError::new(format!(
                "duplicate matrix axis '{}'",
                axis.name
            )));
        }
        if axis.values.is_empty() {
            return Err(AgentTaskMatrixError::new(format!(
                "matrix axis '{}' requires at least one value",
                axis.name
            )));
        }
        let mut values = BTreeSet::new();
        for value in &axis.values {
            validate_matrix_id("axis value", value)?;
            if !values.insert(value.as_str()) {
                return Err(AgentTaskMatrixError::new(format!(
                    "duplicate value '{}' for matrix axis '{}'",
                    value, axis.name
                )));
            }
        }
    }

    Ok(())
}

fn validate_matrix_id(label: &str, value: &str) -> Result<(), AgentTaskMatrixError> {
    if value.is_empty() {
        return Err(AgentTaskMatrixError::new(format!(
            "matrix {label} cannot be empty"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
    {
        return Err(AgentTaskMatrixError::new(format!(
            "matrix {label} '{value}' must contain only ASCII letters, numbers, '.', '_', '-', or '/'"
        )));
    }

    Ok(())
}

fn collect_matrix_combinations(
    axes: &[AgentTaskMatrixAxis],
    index: usize,
    current: &mut BTreeMap<String, String>,
    combinations: &mut Vec<BTreeMap<String, String>>,
) {
    if index == axes.len() {
        combinations.push(current.clone());
        return;
    }

    let axis = &axes[index];
    for value in &axis.values {
        current.insert(axis.name.clone(), value.clone());
        collect_matrix_combinations(axes, index + 1, current, combinations);
    }
    current.remove(&axis.name);
}

fn matrix_cell_id(
    plan_id: &str,
    axes: &[AgentTaskMatrixAxis],
    selection: &BTreeMap<String, String>,
) -> String {
    let pairs = axes
        .iter()
        .map(|axis| {
            let value = selection
                .get(&axis.name)
                .expect("selection populated from axis values");
            format!("{}={}", axis.name, value)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{plan_id}[{pairs}]")
}

fn metadata_with_matrix(
    metadata: Value,
    cell_id: &str,
    selection: &BTreeMap<String, String>,
) -> Value {
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
        "matrix_cell_id".to_string(),
        Value::String(cell_id.to_string()),
    );
    object.insert(
        "matrix".to_string(),
        serde_json::to_value(selection).expect("matrix selection serializes"),
    );
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskFailureClassification, AgentTaskLimits, AgentTaskPolicy,
        AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use serde_json::json;

    #[test]
    fn matrix_expansion_builds_cartesian_agent_task_plan() {
        let plan = expand_agent_task_matrix(
            "bench/studio-web",
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
            template_request(),
        )
        .expect("expand matrix");

        assert_eq!(plan.schema, AGENT_TASK_MATRIX_PLAN_SCHEMA);
        assert_eq!(plan.cells.len(), 4);
        assert_eq!(
            plan.cells
                .iter()
                .map(|cell| cell.cell_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "bench/studio-web[model=gpt-5.5,prompt=site-a]",
                "bench/studio-web[model=gpt-5.5,prompt=site-b]",
                "bench/studio-web[model=claude,prompt=site-a]",
                "bench/studio-web[model=claude,prompt=site-b]",
            ]
        );
        assert!(plan
            .cells
            .iter()
            .all(|cell| cell.task.parent_plan_id.as_deref() == Some("bench/studio-web")));
        assert_eq!(plan.cells[2].task.metadata["matrix"]["model"], "claude");
        assert_eq!(plan.cells[2].task.metadata["matrix"]["prompt"], "site-a");
    }

    #[test]
    fn matrix_homeboy_plan_projection_preserves_expansion_compatibility() {
        let plan = expand_agent_task_matrix(
            "bench/studio-web",
            vec![
                AgentTaskMatrixAxis {
                    name: "model".to_string(),
                    values: vec!["gpt".to_string(), "claude".to_string()],
                },
                AgentTaskMatrixAxis {
                    name: "prompt".to_string(),
                    values: vec!["site".to_string()],
                },
            ],
            template_request(),
        )
        .expect("expand matrix");

        let homeboy_plan = plan.to_homeboy_plan();
        let projected =
            AgentTaskMatrixPlan::from_homeboy_plan(&homeboy_plan).expect("project matrix plan");

        assert_eq!(projected, plan);
        assert_eq!(homeboy_plan.id, "bench/studio-web");
        assert_eq!(homeboy_plan.kind, PlanKind::AgentTaskMatrix);
        assert_eq!(homeboy_plan.steps.len(), 2);
        assert_eq!(homeboy_plan.steps[0].kind, "agent_task.matrix.cell");
        assert_eq!(homeboy_plan.steps[0].inputs["axes"]["model"], json!("gpt"));
        assert_eq!(
            projected.cells[1].task.parent_plan_id.as_deref(),
            Some("bench/studio-web")
        );
        assert_eq!(
            projected.cells[1].task.metadata["matrix"]["model"],
            json!("claude")
        );
    }

    #[test]
    fn matrix_expansion_rejects_invalid_axes() {
        let error = expand_agent_task_matrix(
            "bench-plan",
            vec![AgentTaskMatrixAxis {
                name: "bad,axis".to_string(),
                values: vec!["site-a".to_string()],
            }],
            template_request(),
        )
        .expect_err("invalid axis fails");

        assert!(error.message.contains("bad,axis"));

        let error = expand_agent_task_matrix(
            "bench-plan",
            vec![AgentTaskMatrixAxis {
                name: "prompt".to_string(),
                values: Vec::new(),
            }],
            template_request(),
        )
        .expect_err("empty values fail");

        assert!(error.message.contains("requires at least one value"));
    }

    #[test]
    fn matrix_cell_ids_are_stable_across_input_order_noise() {
        let first = expand_agent_task_matrix(
            "bench-plan",
            vec![
                AgentTaskMatrixAxis {
                    name: "model".to_string(),
                    values: vec!["gpt".to_string(), "claude".to_string()],
                },
                AgentTaskMatrixAxis {
                    name: "prompt".to_string(),
                    values: vec!["site".to_string()],
                },
            ],
            template_request(),
        )
        .expect("expand first");
        let second = expand_agent_task_matrix(
            "bench-plan",
            vec![
                AgentTaskMatrixAxis {
                    name: "model".to_string(),
                    values: vec!["gpt".to_string(), "claude".to_string()],
                },
                AgentTaskMatrixAxis {
                    name: "prompt".to_string(),
                    values: vec!["site".to_string()],
                },
            ],
            template_request(),
        )
        .expect("expand second");

        assert_eq!(first, second);
    }

    #[test]
    fn matrix_aggregate_preserves_partial_task_failure() {
        let plan = expand_agent_task_matrix(
            "bench-plan",
            vec![AgentTaskMatrixAxis {
                name: "model".to_string(),
                values: vec!["gpt".to_string(), "claude".to_string()],
            }],
            template_request(),
        )
        .expect("expand matrix");

        let aggregate = AgentTaskMatrixAggregate::from_outcomes(
            &plan,
            &[
                AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: plan.cells[0].task.task_id.clone(),
                    status: AgentTaskOutcomeStatus::Succeeded,
                    summary: Some("ok".to_string()),
                    failure_classification: None,
                    artifacts: vec![AgentTaskArtifact {
                        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                        id: "metrics".to_string(),
                        kind: "json".to_string(),
                        name: None,
                        path: Some("artifacts/metrics.json".to_string()),
                        url: None,
                        mime: Some("application/json".to_string()),
                        size_bytes: Some(64),
                        sha256: None,
                        metadata: json!({ "metric": "p95_ms" }),
                    }],
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: Value::Null,
                    workflow: None,
                    follow_up: None,
                    metadata: json!({ "p95_ms": 1200 }),
                },
                AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: plan.cells[1].task.task_id.clone(),
                    status: AgentTaskOutcomeStatus::Failed,
                    summary: Some("runner failed".to_string()),
                    failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
                    artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: vec![AgentTaskDiagnostic {
                        class: "runner".to_string(),
                        message: "exit 1".to_string(),
                        data: json!({}),
                    }],
                    outputs: Value::Null,
                    workflow: None,
                    follow_up: None,
                    metadata: json!({}),
                },
            ],
        );

        assert_eq!(aggregate.schema, AGENT_TASK_MATRIX_AGGREGATE_SCHEMA);
        assert!(!aggregate.passed);
        assert_eq!(
            aggregate.cells[0].status,
            Some(AgentTaskOutcomeStatus::Succeeded)
        );
        assert_eq!(aggregate.cells[0].artifacts[0].id, "metrics");
        assert_eq!(
            aggregate.cells[1].status,
            Some(AgentTaskOutcomeStatus::Failed)
        );
        assert_eq!(aggregate.cells[1].axes["model"], "claude");
        assert_eq!(aggregate.cells[1].diagnostics[0].class, "runner");
    }

    fn template_request() -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "template".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "runner".to_string(),
                selector: Some("homeboy-lab".to_string()),
                required_capabilities: vec!["bench".to_string()],
                secret_env: Vec::new(),
                model: None,
                config: json!({}),
            },
            instructions: "Run the selected bench cell.".to_string(),
            inputs: json!({ "component": "studio-web" }),
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: vec!["bench-results".to_string()],
            metadata: json!({ "report": "side-by-side" }),
        }
    }
}

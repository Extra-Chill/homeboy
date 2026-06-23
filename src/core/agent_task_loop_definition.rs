use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task::{AgentTaskComponentContract, AgentTaskRequest};
use crate::core::agent_task_controller_service::AgentTaskRepoLoopSpec;
use crate::core::agent_task_repo_loop_compile::{
    compile_repo_loop_spec, merge_json_objects, merge_workflow_inputs,
    validate_repo_loop_artifact_references,
};
use crate::core::agent_task_schedule::{
    AgentTaskArtifactOutputDeclaration, AgentTaskOutputBinding, AgentTaskOutputDependencies,
    AgentTaskPlan, AgentTaskScheduleOptions,
};
use crate::core::{Error, Result};

pub const AGENT_TASK_LOOP_DEFINITION_SCHEMA: &str = "homeboy/agent-task-loop-definition/v1";
pub const AGENT_TASK_LOOP_SPEC_MATERIALIZATION_SCHEMA: &str =
    "homeboy/agent-task-loop-spec-materialization/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopDefinition {
    #[serde(default = "loop_definition_schema")]
    pub schema: String,
    pub loop_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_contracts: Vec<AgentTaskComponentContract>,
    #[serde(default)]
    pub options: AgentTaskScheduleOptions,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTaskLoopDefinitionTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopDefinitionTask {
    pub task_id: String,
    pub request: AgentTaskRequest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub bindings: HashMap<String, AgentTaskOutputBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_outputs: Vec<AgentTaskArtifactOutputDeclaration>,
}

#[derive(Debug, Clone)]
pub struct AgentTaskLoopSpecMaterializationRequest<'a> {
    pub spec: &'a AgentTaskRepoLoopSpec,
    pub run_inputs: &'a Value,
    pub policy_results: &'a [AgentTaskLoopPolicyResultMaterialization],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopPolicyResultMaterialization {
    pub policy_id: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub policy_inputs: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub policy_results: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provenance: Value,
}

impl AgentTaskLoopPolicyResultMaterialization {
    pub fn from_value(value: Value, source: impl Into<String>) -> Result<Self> {
        let source = source.into();
        if !value.is_object() {
            return Err(Error::validation_invalid_argument(
                "policy-result",
                "policy result must be a JSON object",
                Some(source),
                None,
            ));
        }
        let result: Self = serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "policy-result",
                error.to_string(),
                Some(source.clone()),
                None,
            )
        })?;
        result.validate(source)?;
        Ok(result)
    }

    fn validate(&self, source: String) -> Result<()> {
        if self.policy_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "policy-result.policy_id",
                "policy_id must not be empty",
                Some(source),
                None,
            ));
        }
        validate_optional_policy_object(
            &self.policy_inputs,
            "policy-result.policy_inputs",
            &self.policy_id,
        )?;
        validate_optional_policy_object(
            &self.policy_results,
            "policy-result.policy_results",
            &self.policy_id,
        )?;
        validate_optional_policy_object(
            &self.provenance,
            "policy-result.provenance",
            &self.policy_id,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLoopSpecMaterialization {
    #[serde(default = "loop_spec_materialization_schema")]
    pub schema: String,
    pub spec: AgentTaskRepoLoopSpec,
}

pub fn materialize_repo_loop_spec(
    request: AgentTaskLoopSpecMaterializationRequest<'_>,
) -> Result<AgentTaskLoopSpecMaterialization> {
    validate_repo_loop_artifact_references(request.spec)?;

    let mut spec = request.spec.clone();
    let explicit_inputs = request.run_inputs.get("inputs").or_else(|| {
        request
            .run_inputs
            .get("metadata")
            .is_none()
            .then_some(request.run_inputs)
    });
    if let Some(explicit_inputs) = explicit_inputs.filter(|value| !value.is_null()) {
        if let Some(loop_id) = explicit_inputs
            .get("loop_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|loop_id| !loop_id.is_empty())
        {
            spec.loop_id = loop_id.to_string();
        }
        for workflow in &mut spec.workflows {
            merge_workflow_inputs(&mut workflow.inputs, explicit_inputs);
        }
    }

    if let Some(metadata) = request
        .run_inputs
        .get("metadata")
        .filter(|value| value.is_object())
    {
        merge_json_objects(&mut spec.metadata, metadata);
    }

    materialize_policy_results(&mut spec, request.policy_results)?;

    Ok(AgentTaskLoopSpecMaterialization {
        schema: AGENT_TASK_LOOP_SPEC_MATERIALIZATION_SCHEMA.to_string(),
        spec,
    })
}

fn materialize_policy_results(
    spec: &mut AgentTaskRepoLoopSpec,
    policy_results: &[AgentTaskLoopPolicyResultMaterialization],
) -> Result<()> {
    let mut seen = HashSet::new();
    let mut policy_inputs = serde_json::Map::new();
    let mut policy_result_values = serde_json::Map::new();
    let mut policy_materialization = serde_json::Map::new();

    for policy_result in policy_results {
        if !seen.insert(policy_result.policy_id.clone()) {
            return Err(Error::validation_invalid_argument(
                "policy-result.policy_id",
                format!("duplicate policy_id {}", policy_result.policy_id),
                Some(spec.loop_id.clone()),
                None,
            ));
        }
        if !policy_result.policy_inputs.is_null() {
            policy_inputs.insert(
                policy_result.policy_id.clone(),
                policy_result.policy_inputs.clone(),
            );
        }
        if !policy_result.policy_results.is_null() {
            policy_result_values.insert(
                policy_result.policy_id.clone(),
                policy_result.policy_results.clone(),
            );
        }
        policy_materialization.insert(
            policy_result.policy_id.clone(),
            policy_result_metadata(policy_result),
        );
    }

    let mut workflow_inputs = serde_json::Map::new();
    if !policy_inputs.is_empty() {
        workflow_inputs.insert("policy_inputs".to_string(), Value::Object(policy_inputs));
    }
    if !policy_result_values.is_empty() {
        workflow_inputs.insert(
            "policy_results".to_string(),
            Value::Object(policy_result_values),
        );
    }
    let workflow_inputs = Value::Object(workflow_inputs);
    for workflow in &mut spec.workflows {
        merge_workflow_inputs(&mut workflow.inputs, &workflow_inputs);
    }

    if !policy_materialization.is_empty() {
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "policy_materialization".to_string(),
            Value::Object(policy_materialization),
        );
        merge_json_objects(&mut spec.metadata, &Value::Object(metadata));
    }
    Ok(())
}

fn policy_result_metadata(policy_result: &AgentTaskLoopPolicyResultMaterialization) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "policy_id".to_string(),
        Value::String(policy_result.policy_id.clone()),
    );
    if !policy_result.policy_inputs.is_null() {
        envelope.insert(
            "policy_inputs".to_string(),
            policy_result.policy_inputs.clone(),
        );
    }
    if !policy_result.policy_results.is_null() {
        envelope.insert(
            "policy_results".to_string(),
            policy_result.policy_results.clone(),
        );
    }
    if !policy_result.provenance.is_null() {
        envelope.insert("provenance".to_string(), policy_result.provenance.clone());
    }
    Value::Object(envelope)
}

fn validate_optional_policy_object(value: &Value, field: &str, policy_id: &str) -> Result<()> {
    if value.is_null() || value.is_object() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        field,
        "policy materialization fields must be JSON objects when present",
        Some(policy_id.to_string()),
        None,
    ))
}

pub fn compile_loop_definition(definition: AgentTaskLoopDefinition) -> Result<AgentTaskPlan> {
    validate_loop_definition(&definition)?;

    let plan_id = definition
        .plan_id
        .clone()
        .unwrap_or_else(|| definition.loop_id.clone());
    let mut plan = AgentTaskPlan::new(
        plan_id,
        definition
            .tasks
            .iter()
            .map(|task| task.request.clone())
            .collect(),
    );
    plan.group_key = definition.group_key.clone();
    plan.component_contracts = definition.component_contracts.clone();
    plan.options = definition.options.clone();
    plan.metadata = compile_metadata(&definition);

    for task in &definition.tasks {
        if !task.depends_on.is_empty() || !task.bindings.is_empty() {
            plan.output_dependencies.insert(
                task.task_id.clone(),
                AgentTaskOutputDependencies {
                    depends_on: task.depends_on.clone(),
                    bindings: task.bindings.clone(),
                },
            );
        }
        if !task.artifact_outputs.is_empty() {
            plan.artifact_outputs
                .insert(task.task_id.clone(), task.artifact_outputs.clone());
        }
    }

    plan.rebuild_homeboy_plan();
    Ok(plan)
}

pub fn compile_loop_spec_value(value: Value) -> Result<AgentTaskPlan> {
    if value.get("tasks").is_some_and(|tasks| tasks.is_array())
        && value
            .get("tasks")
            .and_then(Value::as_array)
            .is_some_and(|tasks| tasks.iter().any(|task| task.get("request").is_some()))
    {
        let definition: AgentTaskLoopDefinition =
            serde_json::from_value(value).map_err(|error| {
                Error::validation_invalid_argument(
                    "definition",
                    error.to_string(),
                    Some("agent-task loop definition".to_string()),
                    None,
                )
            })?;
        return compile_loop_definition(definition);
    }

    let spec: AgentTaskRepoLoopSpec = serde_json::from_value(value).map_err(|error| {
        Error::validation_invalid_argument(
            "definition",
            error.to_string(),
            Some("repo loop spec".to_string()),
            None,
        )
    })?;
    compile_repo_loop_spec(spec)
}

fn validate_loop_definition(definition: &AgentTaskLoopDefinition) -> Result<()> {
    if definition.schema != AGENT_TASK_LOOP_DEFINITION_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "expected {AGENT_TASK_LOOP_DEFINITION_SCHEMA}, got {}",
                definition.schema
            ),
            Some(definition.loop_id.clone()),
            None,
        ));
    }
    if definition.loop_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "loop_id",
            "loop_id must not be empty",
            None,
            None,
        ));
    }
    if definition.tasks.is_empty() {
        return Err(Error::validation_invalid_argument(
            "tasks",
            "loop definition must include at least one task",
            Some(definition.loop_id.clone()),
            None,
        ));
    }

    let mut task_ids = HashSet::new();
    for task in &definition.tasks {
        if task.task_id != task.request.task_id {
            return Err(Error::validation_invalid_argument(
                "tasks[].task_id",
                format!(
                    "task_id {} must match request.task_id {}",
                    task.task_id, task.request.task_id
                ),
                Some(definition.loop_id.clone()),
                None,
            ));
        }
        if !task_ids.insert(task.task_id.clone()) {
            return Err(Error::validation_invalid_argument(
                "tasks[].task_id",
                format!("duplicate task_id {}", task.task_id),
                Some(definition.loop_id.clone()),
                None,
            ));
        }
    }

    for task in &definition.tasks {
        for dependency in &task.depends_on {
            if !task_ids.contains(dependency) {
                return Err(Error::validation_invalid_argument(
                    "tasks[].depends_on",
                    format!("{} depends on unknown task {}", task.task_id, dependency),
                    Some(definition.loop_id.clone()),
                    None,
                ));
            }
        }
        for binding in task.bindings.values() {
            if !task_ids.contains(&binding.task_id) {
                return Err(Error::validation_invalid_argument(
                    "tasks[].bindings",
                    format!(
                        "{} binds output from unknown task {}",
                        task.task_id, binding.task_id
                    ),
                    Some(definition.loop_id.clone()),
                    None,
                ));
            }
        }
    }

    Ok(())
}

fn compile_metadata(definition: &AgentTaskLoopDefinition) -> Value {
    let mut metadata = match definition.metadata.clone() {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("definition_metadata".to_string(), other);
            map
        }
    };
    metadata.insert(
        "source_schema".to_string(),
        Value::String(definition.schema.clone()),
    );
    metadata.insert(
        "loop_id".to_string(),
        Value::String(definition.loop_id.clone()),
    );
    Value::Object(metadata)
}

fn loop_definition_schema() -> String {
    AGENT_TASK_LOOP_DEFINITION_SCHEMA.to_string()
}

fn loop_spec_materialization_schema() -> String {
    AGENT_TASK_LOOP_SPEC_MATERIALIZATION_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn compiles_definition_into_agent_task_plan() {
        let definition: AgentTaskLoopDefinition = serde_json::from_value(json!({
            "schema": AGENT_TASK_LOOP_DEFINITION_SCHEMA,
            "loop_id": "example/loop",
            "plan_id": "example-plan",
            "group_key": "example",
            "metadata": { "owner": "tests" },
            "options": { "max_concurrency": 2, "retry": { "max_attempts": 1 } },
            "tasks": [
                {
                    "task_id": "idea",
                    "request": request("idea"),
                    "artifact_outputs": [
                        { "name": "concept_packet", "kind": "example/ConceptPacket/v1", "payload_path": "/artifacts/ConceptPacket.json" }
                    ]
                },
                {
                    "task_id": "design",
                    "request": request("design"),
                    "depends_on": ["idea"],
                    "bindings": {
                        "concept_packet": { "task_id": "idea", "path": "/outputs/concept_packet" }
                    }
                }
            ]
        }))
        .expect("definition parses");

        let plan = compile_loop_definition(definition).expect("definition compiles");

        assert_eq!(plan.schema, "homeboy/agent-task-plan/v1");
        assert_eq!(plan.plan_id, "example-plan");
        assert_eq!(plan.group_key.as_deref(), Some("example"));
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.options.max_concurrency, 2);
        assert_eq!(
            plan.metadata["source_schema"],
            AGENT_TASK_LOOP_DEFINITION_SCHEMA
        );
        assert_eq!(plan.metadata["loop_id"], "example/loop");
        assert_eq!(plan.metadata["owner"], "tests");
        assert_eq!(
            plan.output_dependencies["design"].bindings["concept_packet"].task_id,
            "idea"
        );
        assert_eq!(
            plan.artifact_outputs["idea"][0].kind,
            "example/ConceptPacket/v1"
        );
    }

    #[test]
    fn materializes_repo_loop_id_from_run_inputs() {
        let spec: AgentTaskRepoLoopSpec = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-loop-spec/v1",
            "loop_id": "example/base-loop",
            "workflows": [
                { "workflow_id": "idea", "prompt": "Generate an idea." }
            ]
        }))
        .expect("spec parses");

        let materialized = materialize_repo_loop_spec(AgentTaskLoopSpecMaterializationRequest {
            spec: &spec,
            run_inputs: &json!({
                "inputs": {
                    "loop_id": "example/base-loop/rerun-41",
                    "run_id": "rerun-41"
                }
            }),
            policy_results: &[],
        })
        .expect("spec materializes");

        assert_eq!(materialized.spec.loop_id, "example/base-loop/rerun-41");
        assert_eq!(
            materialized.spec.workflows[0].inputs["run_id"],
            json!("rerun-41")
        );
    }

    #[test]
    fn compiles_repo_loop_entity_fanout_into_concrete_agent_tasks() {
        let spec: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/agent_task_loop/wpsg_controller_fanout.json"
        ))
        .expect("fixture parses");

        let plan = compile_loop_spec_value(spec).expect("fanout repo loop spec compiles");

        let task_ids: Vec<&str> = plan
            .tasks
            .iter()
            .map(|task| task.task_id.as_str())
            .collect();
        assert_eq!(
            task_ids,
            vec![
                "plan-page__home",
                "plan-page__about",
                "build-page__home",
                "build-page__about"
            ]
        );
        assert_eq!(
            plan.tasks[0].inputs["repo_loop"],
            json!({ "entity_id": "home", "workflow_id": "plan-page" })
        );
        assert_eq!(
            plan.output_dependencies["build-page__home"].depends_on,
            vec!["plan-page__home"]
        );
        assert_eq!(
            plan.output_dependencies["build-page__home"].bindings["page_spec"].task_id,
            "plan-page__home"
        );
        assert_eq!(
            plan.output_dependencies["build-page__about"].depends_on,
            vec!["plan-page__about"]
        );
        assert_eq!(
            plan.artifact_outputs["plan-page__home"][0].name,
            "page_spec"
        );
        assert_eq!(
            plan.artifact_outputs["build-page__about"][0].name,
            "page_blocks"
        );
    }

    #[test]
    fn compiles_repo_loop_artifact_graph_edges_into_output_dependencies() {
        let plan = compile_loop_spec_value(json!({
            "loop_id": "example/artifact-graph",
            "artifacts": {
                "site_plan": { "kind": "example/SitePlan/v1", "required": true }
            },
            "artifact_graph": {
                "edges": [
                    {
                        "artifact_id": "site_plan",
                        "from_workflow_id": "plan-site",
                        "to_workflow_id": "build-site",
                        "required": true
                    }
                ]
            },
            "workflows": [
                {
                    "workflow_id": "plan-site",
                    "prompt": "Plan the site.",
                    "emits": ["site_plan"]
                },
                {
                    "workflow_id": "build-site",
                    "prompt": "Build the site.",
                    "consumes": ["site_plan"]
                }
            ]
        }))
        .expect("artifact graph spec compiles");

        assert_eq!(plan.artifact_outputs["plan-site"][0].name, "site_plan");
        assert_eq!(
            plan.output_dependencies["build-site"].depends_on,
            vec!["plan-site"]
        );
        let binding = &plan.output_dependencies["build-site"].bindings["site_plan"];
        assert_eq!(binding.task_id, "plan-site");
        assert_eq!(binding.path, "/typed_artifacts/site_plan");
        assert_eq!(
            binding
                .artifact
                .as_ref()
                .and_then(|artifact| artifact.artifact_id.as_deref()),
            Some("site_plan")
        );
    }

    #[test]
    fn rejects_repo_loop_artifact_graph_undeclared_artifacts() {
        let error = compile_loop_spec_value(json!({
            "loop_id": "example/artifact-graph-missing-artifact",
            "artifact_graph": [
                {
                    "artifact_id": "missing_packet",
                    "from_workflow_id": "produce",
                    "to_workflow_id": "consume"
                }
            ],
            "workflows": [
                { "workflow_id": "produce", "prompt": "Produce.", "emits": ["missing_packet"] },
                { "workflow_id": "consume", "prompt": "Consume.", "consumes": ["missing_packet"] }
            ]
        }))
        .expect_err("undeclared artifact is rejected");

        assert!(error.message.contains("artifact_graph edges"));
        let tried = error.details["tried"]
            .as_array()
            .expect("diagnostics are tried values")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tried.contains(
            "artifact_graph[0].artifact_id references undeclared artifact 'missing_packet'"
        ));
    }

    #[test]
    fn rejects_repo_loop_artifact_graph_fanout_with_deterministic_diagnostic() {
        let error = compile_loop_spec_value(json!({
            "loop_id": "example/artifact-graph-fanout",
            "artifacts": {
                "page_plan": { "kind": "example/PagePlan/v1" }
            },
            "artifact_graph": [
                {
                    "artifact_id": "page_plan",
                    "from_workflow_id": "plan-page",
                    "to_workflow_id": "build-site"
                }
            ],
            "workflows": [
                {
                    "workflow_id": "plan-page",
                    "prompt": "Plan pages.",
                    "entity_ids": ["home", "about"],
                    "emits": ["page_plan"]
                },
                {
                    "workflow_id": "build-site",
                    "prompt": "Build the site.",
                    "consumes": ["page_plan"]
                }
            ]
        }))
        .expect_err("artifact graph fanout requires later compiler support");

        assert!(error.message.contains("artifact_graph edges"));
        let tried = error.details["tried"]
            .as_array()
            .expect("diagnostics are tried values")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tried.contains("producer workflow 'plan-page' uses fan-out"));
        assert!(tried.contains("only supports one task per graph edge"));
    }

    #[test]
    fn rejects_repo_loop_join_over_fanout_with_controller_diagnostic() {
        let error = compile_loop_spec_value(json!({
            "loop_id": "wpsg/join",
            "artifacts": {
                "page_blocks": { "kind": "wpsg/PageBlocks/v1" }
            },
            "workflows": [
                {
                    "workflow_id": "build-page",
                    "prompt": "Build blocks.",
                    "entity_ids": ["home", "about"],
                    "emits": ["page_blocks"]
                },
                {
                    "workflow_id": "assemble-site",
                    "prompt": "Assemble the site.",
                    "consumes": ["page_blocks"]
                }
            ]
        }))
        .expect_err("join over fanout requires controller path");

        assert!(error.message.contains("controller-only sections"));
        let tried = error.details["tried"]
            .as_array()
            .expect("diagnostics are tried values")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tried.contains("join over fan-out artifact 'page_blocks'"));
        assert!(tried.contains("requires the controller path"));
    }

    #[test]
    fn rejects_repo_loop_gates_with_controller_diagnostic() {
        let error = compile_loop_spec_value(json!({
            "loop_id": "wpsg/gated",
            "gates": {
                "visual-parity": { "description": "Check visual parity" }
            },
            "workflows": [
                {
                    "workflow_id": "build-page",
                    "prompt": "Build blocks.",
                    "gates": ["visual-parity"]
                }
            ]
        }))
        .expect_err("gates require controller path");

        assert!(error.message.contains("controller-only sections"));
        let tried = error.details["tried"]
            .as_array()
            .expect("diagnostics are tried values")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tried.contains("workflows[build-page].gates"));
        assert!(tried.contains("gate execution belongs to the controller path"));
    }

    #[test]
    fn rejects_unknown_dependencies() {
        let definition: AgentTaskLoopDefinition = serde_json::from_value(json!({
            "schema": AGENT_TASK_LOOP_DEFINITION_SCHEMA,
            "loop_id": "example/loop",
            "tasks": [
                { "task_id": "design", "request": request("design"), "depends_on": ["missing"] }
            ]
        }))
        .expect("definition parses");

        let error = compile_loop_definition(definition).expect_err("dependency is rejected");
        assert!(error.message.contains("unknown task missing"));
    }

    fn request(task_id: &str) -> Value {
        json!({
            "schema": "homeboy/agent-task-request/v1",
            "task_id": task_id,
            "executor": { "backend": "noop", "config": {} },
            "instructions": format!("Run {task_id}")
        })
    }
}

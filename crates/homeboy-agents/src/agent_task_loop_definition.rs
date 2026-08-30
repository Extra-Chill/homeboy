use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task::{AgentTaskComponentContract, AgentTaskRequest};
use crate::agent_task_controller_service::{validate_loop_spec, AgentTaskRepoLoopSpec};
use crate::agent_task_schedule::{
    AgentTaskArtifactOutputDeclaration, AgentTaskOutputBinding, AgentTaskOutputDependencies,
    AgentTaskPlan, AgentTaskScheduleOptions,
};
use homeboy_core::{Error, Result};

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
    validate_loop_spec(request.spec)?;

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

fn merge_workflow_inputs(target: &mut Value, explicit_inputs: &Value) {
    if !explicit_inputs.is_object() {
        return;
    }
    if !target.is_object() {
        let mut wrapped = serde_json::Map::new();
        if !target.is_null() {
            wrapped.insert("workflow_inputs".to_string(), target.clone());
        }
        *target = Value::Object(wrapped);
    }
    merge_json_objects(target, explicit_inputs);
}

fn merge_json_objects(target: &mut Value, source: &Value) {
    let Some(source) = source.as_object() else {
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    let target = target.as_object_mut().expect("target object");
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
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

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task::{
    AgentTaskArtifactDeclaration, AgentTaskComponentContract, AgentTaskExecutor, AgentTaskLimits,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AgentTaskWorkspaceMode,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_controller_service::{
    AgentTaskRepoLoopSpec, AgentTaskRepoLoopSpecArtifact, AgentTaskRepoLoopSpecWorkflow,
};
use crate::core::agent_task_schedule::{
    AgentTaskArtifactOutputDeclaration, AgentTaskOutputBinding, AgentTaskOutputDependencies,
    AgentTaskPlan, AgentTaskScheduleOptions,
};
use crate::core::{Error, Result};

pub const AGENT_TASK_LOOP_DEFINITION_SCHEMA: &str = "homeboy/agent-task-loop-definition/v1";

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

fn compile_repo_loop_spec(spec: AgentTaskRepoLoopSpec) -> Result<AgentTaskPlan> {
    validate_repo_loop_spec_for_agent_task_plan(&spec)?;

    let mut tasks = Vec::new();
    let mut output_dependencies: HashMap<String, AgentTaskOutputDependencies> = HashMap::new();
    let mut artifact_outputs: HashMap<String, Vec<AgentTaskArtifactOutputDeclaration>> =
        HashMap::new();

    for workflow in &spec.workflows {
        let request = repo_loop_workflow_request(&spec, workflow)?;
        let dependencies = repo_loop_workflow_output_dependencies(&spec, workflow);
        if !dependencies.depends_on.is_empty() || !dependencies.bindings.is_empty() {
            output_dependencies.insert(workflow.workflow_id.clone(), dependencies);
        }
        let outputs = repo_loop_workflow_artifact_outputs(&spec, workflow);
        if !outputs.is_empty() {
            artifact_outputs.insert(workflow.workflow_id.clone(), outputs);
        }
        tasks.push(request);
    }

    let mut plan = AgentTaskPlan::new(spec.loop_id.clone(), tasks);
    plan.group_key = optional_metadata_string(&spec.metadata, "group_key");
    plan.output_dependencies = output_dependencies;
    plan.artifact_outputs = artifact_outputs;
    plan.metadata = serde_json::json!({
        "source_schema": spec.schema,
        "loop_id": spec.loop_id,
        "config_version": spec.config_version,
        "source": "repo_loop_spec",
    });
    plan.rebuild_homeboy_plan();
    Ok(plan)
}

fn validate_repo_loop_spec_for_agent_task_plan(spec: &AgentTaskRepoLoopSpec) -> Result<()> {
    if spec.loop_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "loop_id",
            "repo loop spec requires a non-empty loop_id",
            None,
            None,
        ));
    }
    if spec.workflows.is_empty() {
        return Err(Error::validation_invalid_argument(
            "workflows",
            "repo loop spec must include at least one workflow to compile an agent-task plan",
            Some(spec.loop_id.clone()),
            None,
        ));
    }

    let unsupported = unsupported_repo_loop_spec_fields(spec);
    if !unsupported.is_empty() {
        return Err(Error::validation_invalid_argument(
            "definition",
            "repo loop spec contains controller-only sections that cannot be compiled into a deterministic agent-task plan",
            Some(spec.loop_id.clone()),
            Some(unsupported),
        ));
    }

    let mut workflow_ids = HashSet::new();
    for workflow in &spec.workflows {
        if workflow.workflow_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "workflows[].workflow_id",
                "repo loop spec workflow requires a non-empty workflow_id",
                Some(spec.loop_id.clone()),
                None,
            ));
        }
        if !workflow_ids.insert(workflow.workflow_id.clone()) {
            return Err(Error::validation_invalid_argument(
                "workflows[].workflow_id",
                format!("duplicate workflow_id {}", workflow.workflow_id),
                Some(spec.loop_id.clone()),
                None,
            ));
        }
        if workflow
            .prompt
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
            && workflow.tasks.is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "workflows[]",
                "repo loop spec workflow requires prompt or tasks",
                Some(workflow.workflow_id.clone()),
                None,
            ));
        }
    }

    Ok(())
}

fn unsupported_repo_loop_spec_fields(spec: &AgentTaskRepoLoopSpec) -> Vec<String> {
    let mut unsupported = Vec::new();
    if spec.policy.is_some() {
        unsupported.push(
            "policy: controller transition policies are not executable task records".to_string(),
        );
    }
    if !spec.phases.is_empty() {
        unsupported.push(
            "phases: controller phase transitions are not executable task records".to_string(),
        );
    }
    if !spec.actions.is_empty() {
        unsupported.push(
            "actions: arbitrary controller actions are not executable task records".to_string(),
        );
    }
    if spec.initial_event.is_some() {
        unsupported
            .push("initial_event: event evaluation belongs to agent-task controller".to_string());
    }
    for workflow in &spec.workflows {
        if !workflow.entity_ids.is_empty() {
            unsupported.push(format!(
                "workflows[{}].entity_ids: fan-out expansion needs explicit entity materialization before compile-loop",
                workflow.workflow_id
            ));
        }
    }
    unsupported
}

fn repo_loop_workflow_request(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<AgentTaskRequest> {
    let defaults = spec
        .metadata
        .get("dispatch_defaults")
        .and_then(Value::as_object);
    let backend = defaults
        .and_then(|defaults| defaults.get("backend"))
        .and_then(Value::as_str)
        .unwrap_or("agent-task")
        .to_string();
    let selector = defaults
        .and_then(|defaults| defaults.get("selector"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let model = defaults
        .and_then(|defaults| defaults.get("model"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let secret_env = defaults
        .and_then(|defaults| defaults.get("secret_env"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    let config = defaults
        .and_then(|defaults| defaults.get("provider_config"))
        .cloned()
        .unwrap_or(Value::Null);

    Ok(AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: workflow.workflow_id.clone(),
        group_key: optional_metadata_string(&spec.metadata, "group_key"),
        parent_plan_id: Some(spec.loop_id.clone()),
        executor: AgentTaskExecutor {
            backend,
            selector,
            runtime_selection: None,
            required_capabilities: repo_loop_required_capabilities(spec, workflow),
            secret_env,
            model,
            config,
        },
        instructions: repo_loop_workflow_instructions(workflow),
        inputs: workflow.inputs.clone(),
        source_refs: Vec::new(),
        workspace: repo_loop_workspace(spec),
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: repo_loop_workflow_artifact_declarations(spec, workflow),
        metadata: serde_json::json!({
            "source": "repo_loop_spec",
            "loop_id": spec.loop_id,
            "workflow_id": workflow.workflow_id,
            "agent_id": workflow.agent_id,
            "tools": workflow.tools,
            "abilities": workflow.abilities,
            "consumes": workflow.consumes,
            "emits": workflow.emits,
        }),
    })
}

fn repo_loop_workspace(spec: &AgentTaskRepoLoopSpec) -> AgentTaskWorkspace {
    let defaults = spec
        .metadata
        .get("dispatch_defaults")
        .and_then(Value::as_object);
    AgentTaskWorkspace {
        mode: if defaults.is_some() {
            AgentTaskWorkspaceMode::Existing
        } else {
            AgentTaskWorkspaceMode::Ephemeral
        },
        root: defaults
            .and_then(|defaults| defaults.get("cwd"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        slug: defaults
            .and_then(|defaults| defaults.get("repo"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        cleanup: Some("preserve".to_string()),
        ..AgentTaskWorkspace::default()
    }
}

fn repo_loop_workflow_instructions(workflow: &AgentTaskRepoLoopSpecWorkflow) -> String {
    if let Some(prompt) = workflow.prompt.as_ref().filter(|prompt| !prompt.is_empty()) {
        return prompt.clone();
    }
    workflow
        .tasks
        .iter()
        .map(|task| format!("- {task}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn repo_loop_required_capabilities(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<String> {
    let mut capabilities = Vec::new();
    if let Some(agent_id) = &workflow.agent_id {
        if let Some(agent) = spec.agents.iter().find(|agent| &agent.agent_id == agent_id) {
            for tool_id in &agent.tools {
                push_capability(&mut capabilities, "tool", tool_id);
            }
            for ability_id in &agent.abilities {
                push_capability(&mut capabilities, "ability", ability_id);
            }
        }
    }
    for tool_id in &workflow.tools {
        push_capability(&mut capabilities, "tool", tool_id);
    }
    for ability_id in &workflow.abilities {
        push_capability(&mut capabilities, "ability", ability_id);
    }
    capabilities
}

fn push_capability(capabilities: &mut Vec<String>, kind: &str, id: &str) {
    let capability = format!("{kind}:{id}");
    if !capabilities.contains(&capability) {
        capabilities.push(capability);
    }
}

fn repo_loop_workflow_artifact_declarations(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<AgentTaskArtifactDeclaration> {
    workflow
        .artifacts
        .iter()
        .chain(workflow.emits.iter())
        .filter_map(|artifact_id| repo_loop_artifact(spec, artifact_id))
        .map(|artifact| AgentTaskArtifactDeclaration {
            name: artifact.artifact_id.clone(),
            artifact_type: Some(artifact.kind.clone()),
            artifact_schema: None,
            path: None,
            required: artifact.required,
            description: artifact.description.clone(),
            metadata: Value::Null,
        })
        .collect()
}

fn repo_loop_workflow_artifact_outputs(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<AgentTaskArtifactOutputDeclaration> {
    workflow
        .artifacts
        .iter()
        .chain(workflow.emits.iter())
        .filter_map(|artifact_id| repo_loop_artifact(spec, artifact_id))
        .map(|artifact| AgentTaskArtifactOutputDeclaration {
            name: artifact.artifact_id.clone(),
            kind: artifact.kind.clone(),
            schema: None,
            artifact_id: Some(artifact.artifact_id.clone()),
            payload_path: None,
        })
        .collect()
}

fn repo_loop_workflow_output_dependencies(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> AgentTaskOutputDependencies {
    let mut depends_on = Vec::new();
    let mut bindings = HashMap::new();
    for artifact_id in workflow.consumes.iter().chain(workflow.dependencies.iter()) {
        for producer in repo_loop_artifact_producers(spec, workflow, artifact_id) {
            if !depends_on.contains(&producer.workflow_id) {
                depends_on.push(producer.workflow_id.clone());
            }
            bindings
                .entry(artifact_id.clone())
                .or_insert(AgentTaskOutputBinding {
                    task_id: producer.workflow_id.clone(),
                    path: format!("/typed_artifacts/{artifact_id}"),
                    artifact: repo_loop_artifact(spec, artifact_id).map(|artifact| {
                        crate::core::agent_task_schedule::AgentTaskArtifactBinding {
                            kind: artifact.kind.clone(),
                            schema: None,
                            artifact_id: Some(artifact.artifact_id.clone()),
                            payload_path: None,
                        }
                    }),
                    required: true,
                    default: Value::Null,
                });
        }
    }
    AgentTaskOutputDependencies {
        depends_on,
        bindings,
    }
}

fn repo_loop_artifact<'a>(
    spec: &'a AgentTaskRepoLoopSpec,
    artifact_id: &str,
) -> Option<&'a AgentTaskRepoLoopSpecArtifact> {
    spec.artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == artifact_id)
}

fn repo_loop_artifact_producers<'a>(
    spec: &'a AgentTaskRepoLoopSpec,
    consumer: &AgentTaskRepoLoopSpecWorkflow,
    artifact_id: &str,
) -> Vec<&'a AgentTaskRepoLoopSpecWorkflow> {
    spec.workflows
        .iter()
        .filter(|producer| producer.workflow_id != consumer.workflow_id)
        .filter(|producer| {
            producer.artifacts.iter().any(|id| id == artifact_id)
                || producer.emits.iter().any(|id| id == artifact_id)
        })
        .collect()
}

fn optional_metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
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

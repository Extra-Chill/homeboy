//! Repo-loop spec → agent-task plan compilation.
//!
//! Validates an `AgentTaskRepoLoopSpec`, expands each declared workflow (per
//! entity) into an `AgentTaskRequest`, wires artifact producer/consumer output
//! dependencies, and assembles the resulting `AgentTaskPlan`. The loop
//! definition module owns the loop-definition schema and high-level
//! materialization entry points and delegates the repo-loop expansion here.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::core::agent_task::{
    AgentTaskArtifactDeclaration, AgentTaskComponentContract, AgentTaskExecutor, AgentTaskLimits,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AgentTaskWorkspaceMode,
    AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_controller_service::{
    AgentTaskRepoLoopSpec, AgentTaskRepoLoopSpecArtifact, AgentTaskRepoLoopSpecArtifactGraphEdge,
    AgentTaskRepoLoopSpecWorkflow,
};
use crate::core::agent_task_schedule::{
    AgentTaskArtifactOutputDeclaration, AgentTaskOutputBinding, AgentTaskOutputDependencies,
    AgentTaskPlan,
};
use crate::core::{Error, Result};

pub(crate) fn compile_repo_loop_spec(spec: AgentTaskRepoLoopSpec) -> Result<AgentTaskPlan> {
    validate_repo_loop_spec_for_agent_task_plan(&spec)?;

    let mut tasks = Vec::new();
    let mut output_dependencies: HashMap<String, AgentTaskOutputDependencies> = HashMap::new();
    let mut artifact_outputs: HashMap<String, Vec<AgentTaskArtifactOutputDeclaration>> =
        HashMap::new();

    for workflow in &spec.workflows {
        for entity_id in repo_loop_workflow_entity_ids(workflow) {
            let request = repo_loop_workflow_request(&spec, workflow, entity_id)?;
            let task_id = request.task_id.clone();
            let dependencies = repo_loop_workflow_output_dependencies(&spec, workflow, entity_id);
            if !dependencies.depends_on.is_empty() || !dependencies.bindings.is_empty() {
                output_dependencies.insert(task_id.clone(), dependencies);
            }
            let outputs = repo_loop_workflow_artifact_outputs(&spec, workflow);
            if !outputs.is_empty() {
                artifact_outputs.insert(task_id, outputs);
            }
            tasks.push(request);
        }
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
    validate_repo_loop_artifact_references(spec)?;
    validate_repo_loop_artifact_graph(spec)?;

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
    let mut task_ids = HashSet::new();
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
        for entity_id in repo_loop_workflow_entity_ids(workflow) {
            if entity_id
                .is_some_and(|entity_id| sanitize_repo_loop_task_id_segment(entity_id).is_empty())
            {
                return Err(Error::validation_invalid_argument(
                    "workflows[].entity_ids",
                    "repo loop spec entity_id must contain at least one task-id-safe character",
                    Some(workflow.workflow_id.clone()),
                    entity_id.map(|entity_id| vec![entity_id.to_string()]),
                ));
            }
            let task_id = repo_loop_task_id(workflow, entity_id);
            if !task_ids.insert(task_id.clone()) {
                return Err(Error::validation_invalid_argument(
                    "workflows[].entity_ids",
                    format!("duplicate expanded task_id {task_id}"),
                    Some(workflow.workflow_id.clone()),
                    None,
                ));
            }
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
        if !workflow.gates.is_empty() {
            unsupported.push(format!(
                "workflows[{}].gates: gate execution belongs to the controller path; compile-loop only supports executable agent tasks",
                workflow.workflow_id
            ));
        }
        if !workflow.metrics.is_empty() {
            unsupported.push(format!(
                "workflows[{}].metrics: metric evaluation belongs to the controller path; compile-loop only supports executable agent tasks",
                workflow.workflow_id
            ));
        }
        for artifact_id in workflow.consumes.iter().chain(workflow.dependencies.iter()) {
            let fanout_producers: Vec<String> = repo_loop_artifact_producers(
                spec,
                workflow,
                artifact_id,
                RepoLoopProducerScope::All,
            )
            .into_iter()
            .filter(|(producer, _task_id)| !producer.entity_ids.is_empty())
            .map(|(producer, _task_id)| producer.workflow_id.clone())
            .collect();
            if workflow.entity_ids.is_empty() && !fanout_producers.is_empty() {
                unsupported.push(format!(
                    "workflows[{}].consumes: join over fan-out artifact '{}' from workflows [{}] requires the controller path",
                    workflow.workflow_id,
                    artifact_id,
                    fanout_producers.join(", ")
                ));
            }
        }
    }
    unsupported
}

fn repo_loop_workflow_entity_ids(workflow: &AgentTaskRepoLoopSpecWorkflow) -> Vec<Option<&str>> {
    if workflow.entity_ids.is_empty() {
        vec![None]
    } else {
        workflow
            .entity_ids
            .iter()
            .map(|entity_id| Some(entity_id.as_str()))
            .collect()
    }
}

fn repo_loop_workflow_request(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
    entity_id: Option<&str>,
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

    let task_id = repo_loop_task_id(workflow, entity_id);
    let inputs = repo_loop_workflow_inputs(workflow, entity_id);
    let component_contracts = repo_loop_workflow_component_contracts(&inputs)?;

    Ok(AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id,
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
        inputs,
        source_refs: Vec::new(),
        workspace: repo_loop_workspace(spec),
        component_contracts,
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
            "entity_id": entity_id,
        }),
    })
}

fn repo_loop_task_id(workflow: &AgentTaskRepoLoopSpecWorkflow, entity_id: Option<&str>) -> String {
    match entity_id {
        Some(entity_id) => format!(
            "{}__{}",
            workflow.workflow_id,
            sanitize_repo_loop_task_id_segment(entity_id)
        ),
        None => workflow.workflow_id.clone(),
    }
}

fn sanitize_repo_loop_task_id_segment(value: &str) -> String {
    let mut segment = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    while segment.contains("--") {
        segment = segment.replace("--", "-");
    }
    segment.trim_matches('-').to_string()
}

fn repo_loop_workflow_inputs(
    workflow: &AgentTaskRepoLoopSpecWorkflow,
    entity_id: Option<&str>,
) -> Value {
    let mut inputs = match workflow.inputs.clone() {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("workflow_inputs".to_string(), other);
            map
        }
    };

    if let Some(mut runtime_task) =
        runtime_task_from_workflow_execution(&workflow.runtime_execution)
    {
        apply_runtime_config_to_runtime_task(&mut runtime_task, inputs.get("runtime_config"));
        inputs
            .entry("runtime_task".to_string())
            .or_insert(runtime_task);
    }

    let Some(entity_id) = entity_id else {
        return Value::Object(inputs);
    };
    let mut repo_loop = inputs
        .remove("repo_loop")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    repo_loop.insert(
        "entity_id".to_string(),
        Value::String(entity_id.to_string()),
    );
    repo_loop.insert(
        "workflow_id".to_string(),
        Value::String(workflow.workflow_id.clone()),
    );
    inputs.insert("repo_loop".to_string(), Value::Object(repo_loop));
    Value::Object(inputs)
}

fn runtime_task_from_workflow_execution(runtime_execution: &Value) -> Option<Value> {
    let execution = runtime_execution.as_object()?;
    let ability = execution.get("ability")?.as_str()?.trim();
    if ability.is_empty() {
        return None;
    }

    let mut runtime_task = serde_json::Map::new();
    runtime_task.insert("ability".to_string(), Value::String(ability.to_string()));
    runtime_task.insert(
        "input".to_string(),
        execution
            .get("input")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
    );
    if let Some(kind) = execution
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
    {
        runtime_task.insert("kind".to_string(), Value::String(kind.to_string()));
    }
    Some(Value::Object(runtime_task))
}

fn apply_runtime_config_to_runtime_task(runtime_task: &mut Value, runtime_config: Option<&Value>) {
    let Some(runtime_config) = runtime_config.and_then(Value::as_object) else {
        return;
    };
    let Some(runtime_task) = runtime_task.as_object_mut() else {
        return;
    };
    let input = runtime_task
        .entry("input".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(input) = input.as_object_mut() else {
        return;
    };
    let options = input
        .entry("options".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(options) = options.as_object_mut() else {
        return;
    };

    for key in ["provider", "model"] {
        if let Some(value) = runtime_config
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            options
                .entry(key.to_string())
                .or_insert_with(|| Value::String(value.to_string()));
        }
    }
}

fn repo_loop_workflow_component_contracts(
    inputs: &Value,
) -> Result<Vec<AgentTaskComponentContract>> {
    let Some(raw) = inputs
        .get("runtime_config")
        .and_then(|runtime_config| runtime_config.get("component_contracts"))
    else {
        return Ok(Vec::new());
    };

    serde_json::from_value(raw.clone()).map_err(|error| {
        Error::validation_invalid_argument(
            "workflows[].inputs.runtime_config.component_contracts",
            format!(
                "repo loop workflow runtime_config.component_contracts must be an array of component contracts: {error}"
            ),
            Some(raw.to_string()),
            None,
        )
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
    let _ = (spec, workflow);
    Vec::new()
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
    let mut ids = workflow
        .artifacts
        .iter()
        .chain(workflow.emits.iter())
        .cloned()
        .collect::<Vec<_>>();
    for edge in spec
        .artifact_graph
        .iter()
        .filter(|edge| edge.from_workflow_id == workflow.workflow_id)
    {
        if !ids.contains(&edge.artifact_id) {
            ids.push(edge.artifact_id.clone());
        }
    }
    ids.iter()
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
    entity_id: Option<&str>,
) -> AgentTaskOutputDependencies {
    let mut depends_on = Vec::new();
    let mut bindings = HashMap::new();
    let mut artifact_ids = workflow
        .consumes
        .iter()
        .chain(workflow.dependencies.iter())
        .cloned()
        .collect::<Vec<_>>();
    for edge in artifact_graph_consumer_edges(spec, workflow) {
        if !artifact_ids.contains(&edge.artifact_id) {
            artifact_ids.push(edge.artifact_id.clone());
        }
    }
    for artifact_id in &artifact_ids {
        for (_producer, producer_task_id) in repo_loop_artifact_producers(
            spec,
            workflow,
            artifact_id,
            RepoLoopProducerScope::MatchingEntity(entity_id),
        ) {
            if !depends_on.contains(&producer_task_id) {
                depends_on.push(producer_task_id.clone());
            }
            bindings
                .entry(artifact_id.clone())
                .or_insert(AgentTaskOutputBinding {
                    task_id: producer_task_id,
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

enum RepoLoopProducerScope<'a> {
    All,
    MatchingEntity(Option<&'a str>),
}

fn repo_loop_artifact_producers<'a>(
    spec: &'a AgentTaskRepoLoopSpec,
    consumer: &AgentTaskRepoLoopSpecWorkflow,
    artifact_id: &str,
    scope: RepoLoopProducerScope<'_>,
) -> Vec<(&'a AgentTaskRepoLoopSpecWorkflow, String)> {
    spec.workflows
        .iter()
        .filter(|producer| producer.workflow_id != consumer.workflow_id)
        .filter(|producer| {
            producer.artifacts.iter().any(|id| id == artifact_id)
                || producer.emits.iter().any(|id| id == artifact_id)
                || spec.artifact_graph.iter().any(|edge| {
                    edge.artifact_id == artifact_id
                        && edge.from_workflow_id == producer.workflow_id
                        && edge.to_workflow_id == consumer.workflow_id
                })
        })
        .flat_map(|producer| match scope {
            RepoLoopProducerScope::All if producer.entity_ids.is_empty() => {
                vec![(producer, repo_loop_task_id(producer, None))]
            }
            RepoLoopProducerScope::All => producer
                .entity_ids
                .iter()
                .map(|entity_id| (producer, repo_loop_task_id(producer, Some(entity_id))))
                .collect(),
            RepoLoopProducerScope::MatchingEntity(_) if producer.entity_ids.is_empty() => {
                vec![(producer, repo_loop_task_id(producer, None))]
            }
            RepoLoopProducerScope::MatchingEntity(Some(entity_id)) => producer
                .entity_ids
                .iter()
                .any(|producer_entity_id| producer_entity_id == entity_id)
                .then(|| (producer, repo_loop_task_id(producer, Some(entity_id))))
                .into_iter()
                .collect(),
            RepoLoopProducerScope::MatchingEntity(None) => Vec::new(),
        })
        .collect()
}

fn artifact_graph_consumer_edges<'a>(
    spec: &'a AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<&'a AgentTaskRepoLoopSpecArtifactGraphEdge> {
    spec.artifact_graph
        .iter()
        .filter(|edge| edge.to_workflow_id == workflow.workflow_id)
        .collect()
}

fn optional_metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(crate) fn merge_workflow_inputs(target: &mut Value, explicit_inputs: &Value) {
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

pub(crate) fn merge_json_objects(target: &mut Value, source: &Value) {
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

pub(crate) fn validate_repo_loop_artifact_references(spec: &AgentTaskRepoLoopSpec) -> Result<()> {
    let declared = spec
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact_id.as_str())
        .collect::<HashSet<_>>();
    if declared.is_empty() {
        return Ok(());
    }

    let mut diagnostics = Vec::new();
    for workflow in &spec.workflows {
        for (field, artifact_id) in workflow
            .artifacts
            .iter()
            .map(|artifact_id| ("artifacts", artifact_id))
            .chain(
                workflow
                    .emits
                    .iter()
                    .map(|artifact_id| ("emits", artifact_id)),
            )
            .chain(
                workflow
                    .consumes
                    .iter()
                    .map(|artifact_id| ("consumes", artifact_id)),
            )
        {
            if !declared.contains(artifact_id.as_str()) {
                diagnostics.push(format!(
                    "workflows[{}].{} references undeclared artifact '{}'",
                    workflow.workflow_id, field, artifact_id
                ));
            }
        }
    }

    if diagnostics.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "artifacts",
        "repo loop spec workflows reference artifacts that are not declared in artifacts",
        Some(spec.loop_id.clone()),
        Some(diagnostics),
    ))
}

fn validate_repo_loop_artifact_graph(spec: &AgentTaskRepoLoopSpec) -> Result<()> {
    let mut diagnostics = Vec::new();
    for (index, edge) in spec.artifact_graph.iter().enumerate() {
        if repo_loop_artifact(spec, &edge.artifact_id).is_none() {
            diagnostics.push(format!(
                "artifact_graph[{index}].artifact_id references undeclared artifact '{}'",
                edge.artifact_id
            ));
        }
        let producer = spec
            .workflows
            .iter()
            .find(|workflow| workflow.workflow_id == edge.from_workflow_id);
        let consumer = spec
            .workflows
            .iter()
            .find(|workflow| workflow.workflow_id == edge.to_workflow_id);
        match producer {
            Some(producer) => {
                if !producer.entity_ids.is_empty() {
                    diagnostics.push(format!(
                        "artifact_graph[{index}] producer workflow '{}' uses fan-out; graph compilation only supports one task per graph edge",
                        edge.from_workflow_id
                    ));
                }
                if !producer.artifacts.contains(&edge.artifact_id)
                    && !producer.emits.contains(&edge.artifact_id)
                {
                    diagnostics.push(format!(
                        "artifact_graph[{index}] producer workflow '{}' does not emit artifact '{}'",
                        edge.from_workflow_id, edge.artifact_id
                    ));
                }
            }
            None => diagnostics.push(format!(
                "artifact_graph[{index}].from_workflow_id references undeclared workflow '{}'",
                edge.from_workflow_id
            )),
        }
        match consumer {
            Some(consumer) => {
                if !consumer.entity_ids.is_empty() {
                    diagnostics.push(format!(
                        "artifact_graph[{index}] consumer workflow '{}' uses fan-out; graph compilation only supports one task per graph edge",
                        edge.to_workflow_id
                    ));
                }
                if !consumer.consumes.contains(&edge.artifact_id)
                    && !consumer.dependencies.contains(&edge.artifact_id)
                {
                    diagnostics.push(format!(
                        "artifact_graph[{index}] consumer workflow '{}' does not consume artifact '{}'",
                        edge.to_workflow_id, edge.artifact_id
                    ));
                }
            }
            None => diagnostics.push(format!(
                "artifact_graph[{index}].to_workflow_id references undeclared workflow '{}'",
                edge.to_workflow_id
            )),
        }
    }
    if diagnostics.is_empty() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "artifact_graph",
        "repo loop spec artifact_graph edges cannot be compiled into deterministic agent-task dependencies",
        Some(spec.loop_id.clone()),
        Some(diagnostics),
    ))
}

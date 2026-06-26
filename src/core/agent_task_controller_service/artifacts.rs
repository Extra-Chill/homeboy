//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;

const CONTROLLER_EVIDENCE_INDEX_SCHEMA: &str =
    "homeboy/agent-task-loop-controller-evidence-index/v1";

#[derive(Debug, Clone, Serialize)]
struct ControllerEvidenceIndex {
    schema: &'static str,
    run_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entries: Vec<ControllerEvidenceIndexEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct ControllerEvidenceIndexEntry {
    task_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifact_refs: Vec<AgentTaskLoopArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<AgentTaskArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence_refs: Vec<AgentTaskEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    typed_artifacts: Vec<AgentTaskTypedArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct RequiredWorkflowArtifact {
    artifact_id: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

pub(super) fn validate_required_action_artifacts(
    action: &AgentTaskLoopPolicyActionRecord,
    execution: &Value,
) -> Vec<RequiredWorkflowArtifact> {
    match &action.action {
        AgentTaskLoopPolicyAction::SpawnTask { request, .. }
        | AgentTaskLoopPolicyAction::RouteFinding {
            request_template: request,
            ..
        } => missing_required_artifacts_for_execution(request, execution, None),
        AgentTaskLoopPolicyAction::FanOut {
            request_template, ..
        } => execution
            .get("results")
            .and_then(Value::as_array)
            .map(|results| {
                results
                    .iter()
                    .enumerate()
                    .flat_map(|(index, result)| {
                        missing_required_artifacts_for_execution(
                            request_template,
                            result,
                            Some(format!("results[{index}]")),
                        )
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                required_workflow_artifacts(request_template)
                    .into_iter()
                    .map(|mut artifact| {
                        artifact.scope = Some("results".to_string());
                        artifact
                    })
                    .collect()
            }),
        _ => Vec::new(),
    }
}

pub(super) fn missing_required_artifacts_for_execution(
    request: &Value,
    execution: &Value,
    scope: Option<String>,
) -> Vec<RequiredWorkflowArtifact> {
    required_workflow_artifacts(request)
        .into_iter()
        .filter_map(|mut artifact| {
            if execution_contains_artifact(execution, &artifact.artifact_id, &artifact.kind) {
                None
            } else {
                artifact.scope = scope.clone();
                Some(artifact)
            }
        })
        .collect()
}

pub(super) fn required_workflow_artifacts(request: &Value) -> Vec<RequiredWorkflowArtifact> {
    let mut artifacts = Vec::new();
    collect_required_artifacts_from_plan(request.get("plan").unwrap_or(request), &mut artifacts);
    if let Some(dispatch) = request.get("dispatch") {
        collect_required_artifacts_from_client_context(dispatch, &mut artifacts);
    }
    collect_required_artifacts_from_client_context(request, &mut artifacts);
    artifacts
}

pub(super) fn collect_required_artifacts_from_client_context(
    value: &Value,
    artifacts: &mut Vec<RequiredWorkflowArtifact>,
) {
    let Some(context) = value.get("client_context") else {
        return;
    };
    let context = match context {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
        other => other.clone(),
    };
    collect_required_artifacts_from_plan(&context["plan"], artifacts);
    collect_required_artifacts_from_declarations(&context["artifacts"], artifacts);
}

pub(super) fn collect_required_artifacts_from_plan(
    value: &Value,
    artifacts: &mut Vec<RequiredWorkflowArtifact>,
) {
    collect_required_artifacts_from_declarations(&value["artifacts"], artifacts);
}

pub(super) fn collect_required_artifacts_from_declarations(
    value: &Value,
    artifacts: &mut Vec<RequiredWorkflowArtifact>,
) {
    let Some(declarations) = value.as_array() else {
        return;
    };
    // Resolve the first non-empty string field on a declaration from an ordered
    // list of candidate paths. Each path is either a single top-level key or a
    // `(parent, child)` pair resolving `declaration[parent][child]`.
    let first_non_empty_str = |declaration: &Value, paths: &[&[&str]]| -> Option<String> {
        paths
            .iter()
            .filter_map(|path| {
                let mut node = declaration;
                for segment in path.iter() {
                    node = node.get(segment)?;
                }
                node.as_str()
            })
            .find(|value| !value.is_empty())
            .map(str::to_string)
    };
    for declaration in declarations {
        let required = declaration
            .get("required")
            .and_then(Value::as_bool)
            .or_else(|| {
                declaration
                    .get("data")
                    .and_then(|data| data.get("required"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(false);
        if !required {
            continue;
        }
        let Some(artifact_id) = first_non_empty_str(declaration, &[&["artifact_id"], &["id"]])
        else {
            continue;
        };
        let Some(kind) = first_non_empty_str(
            declaration,
            &[&["kind"], &["artifact_type"], &["type"], &["data", "kind"]],
        ) else {
            continue;
        };
        if artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == artifact_id && artifact.kind == kind)
        {
            continue;
        }
        artifacts.push(RequiredWorkflowArtifact {
            artifact_id: artifact_id.to_string(),
            kind: kind.to_string(),
            scope: None,
        });
    }
}

pub(super) fn execution_contains_artifact(value: &Value, artifact_id: &str, kind: &str) -> bool {
    match value {
        Value::Object(object) => {
            let id_matches = object
                .get("id")
                .or_else(|| object.get("artifact_id"))
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
                == Some(artifact_id);
            let kind_matches = object
                .get("kind")
                .or_else(|| object.get("artifact_type"))
                .or_else(|| object.get("type"))
                .and_then(Value::as_str)
                == Some(kind);
            (id_matches && kind_matches)
                || object
                    .values()
                    .any(|value| execution_contains_artifact(value, artifact_id, kind))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| execution_contains_artifact(value, artifact_id, kind)),
        _ => false,
    }
}

pub(super) fn request_with_required_workflow_artifacts(
    record: &AgentTaskLoopControllerRecord,
    request: &Value,
) -> Value {
    let workflow_artifacts = matching_completed_workflow_artifacts(record, request);
    if workflow_artifacts.is_empty() {
        return request.clone();
    }

    let mut request = request.as_object().cloned().unwrap_or_default();
    merge_workflow_artifacts(&mut request, &workflow_artifacts);
    if let Some(dispatch) = request.get_mut("dispatch").and_then(Value::as_object_mut) {
        merge_workflow_artifacts(dispatch, &workflow_artifacts);
    }
    Value::Object(request)
}

pub(super) fn hydrate_consumed_artifacts(
    record: &AgentTaskLoopControllerRecord,
    request: &Value,
) -> Value {
    let consumed = consumed_artifact_ids(request);
    if consumed.is_empty() {
        return request.clone();
    }

    let mut artifacts = serde_json::Map::new();
    for artifact_id in consumed {
        if let Some(artifact) = find_controller_artifact(record, &artifact_id) {
            artifacts.insert(artifact_id, artifact);
        }
    }
    if artifacts.is_empty() {
        return request.clone();
    }

    let mut hydrated = request.clone();
    merge_request_input_artifacts(&mut hydrated, &artifacts);
    merge_runtime_execution_input_artifacts(&mut hydrated, &artifacts);
    merge_dispatch_context_artifacts(&mut hydrated, &artifacts);
    hydrated
}

fn consumed_artifact_ids(request: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    append_string_array(&mut ids, request.get("consumes"));
    append_string_array(
        &mut ids,
        request
            .get("inputs")
            .and_then(|inputs| inputs.get("consumes")),
    );
    if let Some(context) = request
        .get("dispatch")
        .and_then(|dispatch| dispatch.get("client_context"))
        .and_then(Value::as_str)
        .and_then(|context| serde_json::from_str::<Value>(context).ok())
    {
        append_string_array(&mut ids, context.get("consumes"));
        append_string_array(
            &mut ids,
            context
                .get("inputs")
                .and_then(|inputs| inputs.get("consumes")),
        );
    }
    ids.sort();
    ids.dedup();
    ids
}

fn append_string_array(ids: &mut Vec<String>, value: Option<&Value>) {
    let Some(values) = value.and_then(Value::as_array) else {
        return;
    };
    ids.extend(
        values
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    );
}

pub(super) fn find_controller_artifact(
    record: &AgentTaskLoopControllerRecord,
    artifact_id: &str,
) -> Option<Value> {
    for lineage in record.task_lineage.iter().rev() {
        if let Some(artifact) = artifact_from_outputs(&lineage.outputs, artifact_id) {
            return Some(artifact);
        }
    }
    for event in record.history.iter().rev() {
        if let Some(artifact) = artifact_from_history_payload(&event.payload, artifact_id) {
            return Some(artifact);
        }
    }
    None
}

fn artifact_from_outputs(outputs: &Value, artifact_id: &str) -> Option<Value> {
    outputs
        .get("artifacts")
        .and_then(|artifacts| artifacts.get(artifact_id))
        .or_else(|| {
            outputs
                .get("typed_artifacts")
                .and_then(|artifacts| artifacts.get(artifact_id))
        })
        .cloned()
}

fn artifact_from_history_payload(payload: &Value, artifact_id: &str) -> Option<Value> {
    let result = payload.get("execution")?.get("result")?;
    if let Some(artifact) = artifact_from_outputs(result, artifact_id) {
        return Some(artifact);
    }
    let outcomes = result
        .get("aggregate")
        .and_then(|aggregate| aggregate.get("outcomes"))
        .and_then(Value::as_array)?;
    for outcome in outcomes.iter().rev() {
        if let Some(artifact) =
            artifact_from_outputs(outcome.get("outputs").unwrap_or(&Value::Null), artifact_id)
        {
            return Some(artifact);
        }
        if let Some(artifact) = outcome
            .get("metadata")
            .and_then(|metadata| metadata.get("typed_artifacts"))
            .and_then(|artifacts| artifacts.get(artifact_id))
            .cloned()
        {
            return Some(artifact);
        }
    }
    None
}

fn merge_request_input_artifacts(request: &mut Value, artifacts: &serde_json::Map<String, Value>) {
    let Some(object) = request.as_object_mut() else {
        return;
    };
    let inputs = object
        .entry("inputs".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(inputs) = inputs.as_object_mut() else {
        return;
    };
    let artifact_inputs = inputs
        .entry("artifacts".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(artifact_inputs) = artifact_inputs.as_object_mut() else {
        return;
    };
    for (artifact_id, artifact) in artifacts {
        artifact_inputs.insert(artifact_id.clone(), artifact.clone());
    }
    insert_artifact_aliases(inputs, artifacts);
}

fn merge_runtime_execution_input_artifacts(
    request: &mut Value,
    artifacts: &serde_json::Map<String, Value>,
) {
    let Some(runtime_input) = request
        .get_mut("runtime_execution")
        .and_then(|runtime_execution| runtime_execution.get_mut("input"))
        .and_then(|input| input.get_mut("input"))
    else {
        return;
    };
    let Some(runtime_input) = runtime_input.as_object_mut() else {
        return;
    };
    let artifact_inputs = runtime_input
        .entry("artifacts".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(artifact_inputs) = artifact_inputs.as_object_mut() else {
        return;
    };
    for (artifact_id, artifact) in artifacts {
        artifact_inputs.insert(artifact_id.clone(), artifact.clone());
    }
    insert_artifact_aliases(runtime_input, artifacts);
}

fn merge_dispatch_context_artifacts(
    request: &mut Value,
    artifacts: &serde_json::Map<String, Value>,
) {
    let Some(context_value) = request
        .get_mut("dispatch")
        .and_then(|dispatch| dispatch.get_mut("client_context"))
    else {
        return;
    };
    let Some(context_string) = context_value.as_str() else {
        return;
    };
    let Ok(mut context) = serde_json::from_str::<Value>(context_string) else {
        return;
    };
    merge_request_input_artifacts(&mut context, artifacts);
    merge_runtime_execution_input_artifacts(&mut context, artifacts);
    *context_value = Value::String(context.to_string());
}

fn insert_artifact_aliases(
    inputs: &mut serde_json::Map<String, Value>,
    artifacts: &serde_json::Map<String, Value>,
) {
    for (artifact_id, artifact) in artifacts {
        let should_insert = inputs
            .get(artifact_id)
            .is_none_or(|value| value.is_null() || value.as_str() == Some(""));
        if should_insert {
            inputs.insert(
                artifact_id.clone(),
                artifact
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| artifact.clone()),
            );
        }
    }
}

pub(super) fn execution_with_request_workflow_artifacts(
    execution: Value,
    request: &Value,
) -> Value {
    let runtime_bundle = runtime_bundle_evidence(request, &execution);
    let workflow_artifacts = request
        .get("workflow_artifacts")
        .and_then(Value::as_array)
        .filter(|artifacts| !artifacts.is_empty());

    if workflow_artifacts.is_none() && runtime_bundle.is_none() {
        return execution;
    }

    let mut execution = execution.as_object().cloned().unwrap_or_default();
    if let Some(workflow_artifacts) = workflow_artifacts {
        merge_workflow_artifacts(&mut execution, workflow_artifacts);
    }
    if let Some(runtime_bundle) = runtime_bundle {
        execution.insert("runtime_bundle".to_string(), runtime_bundle);
    }
    Value::Object(execution)
}

fn runtime_bundle_evidence(request: &Value, execution: &Value) -> Option<Value> {
    let configured = runtime_bundle_configurations(request);
    let observed = runtime_bundle_observations(execution);
    if configured.is_empty() && observed.is_empty() {
        return None;
    }

    let mut evidence = serde_json::Map::new();
    if !configured.is_empty() {
        evidence.insert(
            "configured".to_string(),
            serde_json::json!({ "tasks": configured }),
        );
    }
    if !observed.is_empty() {
        evidence.insert(
            "observed".to_string(),
            serde_json::json!({ "results": observed }),
        );
    }
    Some(Value::Object(evidence))
}

fn runtime_bundle_configurations(value: &Value) -> Vec<Value> {
    let mut tasks = Vec::new();
    collect_runtime_bundle_configurations(value, &mut tasks, 0);
    tasks
}

fn collect_runtime_bundle_configurations(value: &Value, tasks: &mut Vec<Value>, depth: usize) {
    if depth > 12 {
        return;
    }
    match value {
        Value::Object(object) => {
            if let Some(runtime_task) = object.get("runtime_task") {
                push_runtime_bundle_configuration(runtime_task, tasks);
            }
            push_runtime_bundle_configuration(value, tasks);
            for child in object.values() {
                collect_runtime_bundle_configurations(child, tasks, depth + 1);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_runtime_bundle_configurations(child, tasks, depth + 1);
            }
        }
        Value::String(raw) => {
            let trimmed = raw.trim_start();
            if (trimmed.starts_with('{') || trimmed.starts_with('[')) && raw.len() < 128 * 1024 {
                if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                    collect_runtime_bundle_configurations(&parsed, tasks, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn push_runtime_bundle_configuration(runtime_task: &Value, tasks: &mut Vec<Value>) {
    let Some(object) = runtime_task.as_object() else {
        return;
    };
    let ability = object.get("ability").and_then(Value::as_str);
    let kind = object.get("kind").and_then(Value::as_str);
    let input = object.get("input");
    let is_bundle = kind == Some("bundle")
        || ability.is_some_and(|ability| ability.contains("runtime-package/"))
        || input.and_then(|input| input.get("package")).is_some();
    if !is_bundle {
        return;
    }

    let mut task = serde_json::Map::new();
    if let Some(ability) = ability {
        task.insert("ability".to_string(), Value::String(ability.to_string()));
    }
    if let Some(kind) = kind {
        task.insert("kind".to_string(), Value::String(kind.to_string()));
    }
    if let Some(input) = input {
        let budgets = runtime_budget_fields(input);
        if !budgets.is_empty() {
            task.insert("budgets".to_string(), Value::Object(budgets));
        }
    }
    let task = Value::Object(task);
    if !tasks.iter().any(|existing| existing == &task) {
        tasks.push(task);
    }
}

fn runtime_budget_fields(value: &Value) -> serde_json::Map<String, Value> {
    let mut fields = serde_json::Map::new();
    collect_runtime_budget_fields(value, &mut fields, 0);
    fields
}

fn collect_runtime_budget_fields(
    value: &Value,
    fields: &mut serde_json::Map<String, Value>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if is_runtime_budget_key(key) && !child.is_null() {
                    fields.entry(key.clone()).or_insert_with(|| child.clone());
                }
                collect_runtime_budget_fields(child, fields, depth + 1);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_runtime_budget_fields(child, fields, depth + 1);
            }
        }
        _ => {}
    }
}

fn is_runtime_budget_key(key: &str) -> bool {
    key == "time_budget_ms"
        || key == "step_budget"
        || key == "wait_budget_ms"
        || key == "drain_budget_ms"
        || key == "wait_timeout_ms"
        || key == "drain_timeout_ms"
}

fn runtime_bundle_observations(value: &Value) -> Vec<Value> {
    let mut observations = Vec::new();
    collect_runtime_bundle_observations(value, &mut observations, 0);
    observations
}

fn collect_runtime_bundle_observations(value: &Value, observations: &mut Vec<Value>, depth: usize) {
    if depth > 16 {
        return;
    }
    match value {
        Value::Object(object) => {
            let mut observed = serde_json::Map::new();
            for (key, child) in object {
                if is_runtime_observation_key(key) && !child.is_null() {
                    observed.insert(key.clone(), child.clone());
                }
            }
            if !observed.is_empty() {
                let observed = Value::Object(observed);
                if !observations.iter().any(|existing| existing == &observed) {
                    observations.push(observed);
                }
            }
            for child in object.values() {
                collect_runtime_bundle_observations(child, observations, depth + 1);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_runtime_bundle_observations(child, observations, depth + 1);
            }
        }
        _ => {}
    }
}

fn is_runtime_observation_key(key: &str) -> bool {
    matches!(
        key,
        "wait_result"
            | "job_status"
            | "steps_drained"
            | "actions_drained"
            | "drained_steps"
            | "drained_actions"
            | "elapsed_ms"
            | "elapsed_time_ms"
            | "wall_time_ms"
            | "wall_time"
            | "completion_outcome"
            | "completion_status"
            | "terminal_state"
            | "error_type"
            | "classification"
            | "error_classification"
    )
}

fn matching_completed_workflow_artifacts(
    record: &AgentTaskLoopControllerRecord,
    request: &Value,
) -> Vec<Value> {
    let required = required_workflow_artifacts(request);
    if required.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for lineage in &record.task_lineage {
        let Some(entries) = lineage.outputs["evidence_index"]["entries"].as_array() else {
            continue;
        };
        for artifact in entries
            .iter()
            .flat_map(|entry| entry["typed_artifacts"].as_array().into_iter().flatten())
        {
            if required.iter().any(|required| {
                execution_contains_artifact(artifact, &required.artifact_id, &required.kind)
            }) && !matches.iter().any(|existing| existing == artifact)
            {
                matches.push(artifact.clone());
            }
        }
    }
    matches
}

fn merge_workflow_artifacts(
    object: &mut serde_json::Map<String, Value>,
    workflow_artifacts: &[Value],
) {
    let entry = object
        .entry("workflow_artifacts".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !entry.is_array() {
        *entry = serde_json::json!([]);
    }
    let target = entry.as_array_mut().expect("workflow artifacts array");
    for artifact in workflow_artifacts {
        if !target.iter().any(|existing| existing == artifact) {
            target.push(artifact.clone());
        }
    }
}

pub(super) fn required_artifact_diagnostics(
    missing: &[RequiredWorkflowArtifact],
) -> Vec<AgentTaskLoopActionDiagnostic> {
    let labels = missing
        .iter()
        .map(|artifact| match &artifact.scope {
            Some(scope) => format!("{}:{} at {scope}", artifact.artifact_id, artifact.kind),
            None => format!("{}:{}", artifact.artifact_id, artifact.kind),
        })
        .collect::<Vec<_>>()
        .join(", ");
    vec![AgentTaskLoopActionDiagnostic {
        code: "required_workflow_artifacts_missing".to_string(),
        message: format!(
            "controller action missing required workflow artifact handoff(s): {labels}"
        ),
        runner: None,
        details: serde_json::json!({ "missing_artifacts": missing }),
    }]
}

pub(super) fn record_controller_spawn(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    run_id: &str,
    request: &Value,
) -> Result<()> {
    if let Some(dedupe) = record.dedupe_keys.get_mut(dedupe_key) {
        dedupe.run_id = Some(run_id.to_string());
    }
    if let Some(entity_id) = entity_id {
        if let Some(entity) = record.entities.get_mut(entity_id) {
            if !entity.run_refs.iter().any(|run| run.run_id == run_id) {
                entity.run_refs.push(AgentTaskLoopRunRef {
                    run_id: run_id.to_string(),
                    task_id: None,
                    role: Some("spawn_task".to_string()),
                });
            }
        }
    }
    if !record
        .task_lineage
        .iter()
        .any(|lineage| lineage.run_id == run_id)
    {
        record.task_lineage.push(AgentTaskLoopTaskLineage {
            run_id: run_id.to_string(),
            task_id: None,
            parent_run_id: None,
            parent_task_id: None,
            entity_id: entity_id.map(str::to_string),
            dedupe_key: Some(dedupe_key.to_string()),
            artifact_refs: Vec::new(),
            inputs: request.clone(),
            outputs: Value::Null,
        });
    }
    push_controller_history(
        record,
        "controller.action.spawned_run",
        entity_id.map(str::to_string),
        serde_json::json!({
            "action_id": action.action_id,
            "dedupe_key": dedupe_key,
            "run_id": run_id,
        }),
    );
    controller::write_controller(record)?;
    Ok(())
}

pub(super) fn record_controller_aggregate_evidence(
    record: &mut AgentTaskLoopControllerRecord,
    entity_id: Option<&str>,
    run_id: &str,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let entries = aggregate
        .outcomes
        .iter()
        .filter_map(|outcome| {
            evidence_index_entry(
                &outcome.task_id,
                outcome.artifacts.clone(),
                workflow_evidence_refs(outcome.workflow.as_ref())
                    .chain(outcome.evidence_refs.iter().cloned())
                    .collect(),
                outcome.typed_artifacts.clone(),
            )
        })
        .collect::<Vec<_>>();
    record_controller_evidence_index(
        record,
        entity_id,
        ControllerEvidenceIndex {
            schema: CONTROLLER_EVIDENCE_INDEX_SCHEMA,
            run_id: run_id.to_string(),
            entries,
        },
    )
}

pub(super) fn record_controller_result_evidence(
    record: &mut AgentTaskLoopControllerRecord,
    entity_id: Option<&str>,
    run_id: &str,
    result: &Value,
) -> Result<()> {
    let artifacts = parse_array::<AgentTaskArtifact>(&result["artifacts"])?;
    let evidence_refs = parse_array::<AgentTaskEvidenceRef>(&result["evidence_refs"])?;
    let typed_artifacts = parse_array::<AgentTaskTypedArtifact>(&result["typed_artifacts"])?;
    let entries = evidence_index_entry("result", artifacts, evidence_refs, typed_artifacts)
        .into_iter()
        .collect();
    record_controller_evidence_index(
        record,
        entity_id,
        ControllerEvidenceIndex {
            schema: CONTROLLER_EVIDENCE_INDEX_SCHEMA,
            run_id: run_id.to_string(),
            entries,
        },
    )
}

fn record_controller_evidence_index(
    record: &mut AgentTaskLoopControllerRecord,
    entity_id: Option<&str>,
    index: ControllerEvidenceIndex,
) -> Result<()> {
    if index.entries.is_empty() {
        return Ok(());
    }
    let artifact_refs = index
        .entries
        .iter()
        .flat_map(|entry| entry.artifact_refs.clone())
        .collect::<Vec<_>>();
    let index_value = serde_json::to_value(&index)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;

    if let Some(lineage) = record
        .task_lineage
        .iter_mut()
        .find(|lineage| lineage.run_id == index.run_id)
    {
        extend_artifact_refs(&mut lineage.artifact_refs, artifact_refs.clone());
        lineage.outputs =
            merge_controller_evidence_output(lineage.outputs.clone(), index_value.clone());
    }

    if let Some(entity_id) = entity_id {
        if let Some(entity) = record.entities.get_mut(entity_id) {
            extend_artifact_refs(&mut entity.artifact_refs, artifact_refs);
            entity.metadata = merge_entity_evidence_output(entity.metadata.clone(), index_value);
        }
    }

    Ok(())
}

fn evidence_index_entry(
    task_id: &str,
    artifacts: Vec<AgentTaskArtifact>,
    evidence_refs: Vec<AgentTaskEvidenceRef>,
    typed_artifacts: Vec<AgentTaskTypedArtifact>,
) -> Option<ControllerEvidenceIndexEntry> {
    let mut artifact_refs = Vec::new();
    for artifact in &artifacts {
        push_artifact_ref_once(&mut artifact_refs, artifact_ref_from_artifact(artifact));
    }
    for evidence_ref in &evidence_refs {
        push_artifact_ref_once(
            &mut artifact_refs,
            artifact_ref_from_evidence_ref(evidence_ref),
        );
    }
    for typed_artifact in &typed_artifacts {
        push_artifact_ref_once(
            &mut artifact_refs,
            artifact_ref_from_typed_artifact(task_id, typed_artifact),
        );
    }

    (!artifact_refs.is_empty()).then(|| ControllerEvidenceIndexEntry {
        task_id: task_id.to_string(),
        artifact_refs,
        artifacts,
        evidence_refs,
        typed_artifacts,
    })
}

fn workflow_evidence_refs(
    workflow: Option<&AgentTaskWorkflowEvidence>,
) -> impl Iterator<Item = AgentTaskEvidenceRef> + '_ {
    workflow.into_iter().flat_map(|workflow| {
        workflow
            .steps
            .iter()
            .flat_map(|step| step.artifact_refs.iter().cloned())
    })
}

fn artifact_ref_from_artifact(artifact: &AgentTaskArtifact) -> AgentTaskLoopArtifactRef {
    AgentTaskLoopArtifactRef {
        uri: artifact
            .url
            .clone()
            .or_else(|| artifact.path.clone())
            .unwrap_or_else(|| format!("artifact:{}", artifact.id)),
        kind: Some(artifact.kind.clone()),
        role: artifact.declared_role().map(str::to_string),
        label: artifact.display_label().map(str::to_string),
        semantic_key: artifact.declared_semantic_key().map(str::to_string),
    }
}

fn artifact_ref_from_evidence_ref(evidence_ref: &AgentTaskEvidenceRef) -> AgentTaskLoopArtifactRef {
    AgentTaskLoopArtifactRef {
        uri: evidence_ref.uri.clone(),
        kind: Some(evidence_ref.kind.clone()),
        role: None,
        label: evidence_ref.label.clone(),
        semantic_key: None,
    }
}

fn artifact_ref_from_typed_artifact(
    task_id: &str,
    typed_artifact: &AgentTaskTypedArtifact,
) -> AgentTaskLoopArtifactRef {
    typed_artifact
        .artifact
        .as_ref()
        .map(artifact_ref_from_artifact)
        .unwrap_or_else(|| AgentTaskLoopArtifactRef {
            uri: format!("typed-artifact:{task_id}:{}", typed_artifact.name),
            kind: typed_artifact.artifact_type.clone(),
            role: typed_artifact
                .metadata
                .get("role")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string),
            label: Some(typed_artifact.name.clone()),
            semantic_key: typed_artifact
                .metadata
                .get("semantic_key")
                .or_else(|| typed_artifact.metadata.get("semanticKey"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string),
        })
}

fn extend_artifact_refs(
    target: &mut Vec<AgentTaskLoopArtifactRef>,
    refs: Vec<AgentTaskLoopArtifactRef>,
) {
    for artifact_ref in refs {
        push_artifact_ref_once(target, artifact_ref);
    }
}

fn push_artifact_ref_once(
    target: &mut Vec<AgentTaskLoopArtifactRef>,
    artifact_ref: AgentTaskLoopArtifactRef,
) {
    if target.iter().any(|existing| {
        existing.uri == artifact_ref.uri
            && existing.kind == artifact_ref.kind
            && existing.role == artifact_ref.role
            && existing.label == artifact_ref.label
            && existing.semantic_key == artifact_ref.semantic_key
    }) {
        return;
    }
    target.push(artifact_ref);
}

fn merge_controller_evidence_output(outputs: Value, index: Value) -> Value {
    let mut object = outputs.as_object().cloned().unwrap_or_default();
    object.insert("evidence_index".to_string(), index);
    Value::Object(object)
}

fn merge_entity_evidence_output(metadata: Value, index: Value) -> Value {
    let mut metadata = metadata.as_object().cloned().unwrap_or_default();
    let outputs = metadata
        .entry("outputs".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !outputs.is_object() {
        *outputs = serde_json::json!({});
    }
    let indexes = outputs
        .as_object_mut()
        .expect("outputs object")
        .entry("evidence_indexes".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !indexes.is_array() {
        *indexes = serde_json::json!([]);
    }
    let indexes = indexes.as_array_mut().expect("evidence indexes array");
    let run_id = index.get("run_id").cloned();
    if let Some(position) = indexes
        .iter()
        .position(|existing| existing.get("run_id").cloned() == run_id)
    {
        indexes[position] = index;
    } else {
        indexes.push(index);
    }
    Value::Object(metadata)
}

fn parse_array<T>(value: &Value) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    if value.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(value.clone()).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("controller evidence array".to_string()),
            Some(value.to_string()),
        )
    })
}

pub(super) fn controller_action_runtime_evidence(
    execution: &Value,
) -> Option<ControllerActionRuntimeEvidence> {
    let mut evidence = ControllerActionRuntimeEvidence {
        runtime_invocation_id: None,
        provider_id: None,
        runtime_id: None,
        failure_classification: None,
        phase: None,
        artifact_refs: Vec::new(),
        transcript_refs: Vec::new(),
        result_refs: Vec::new(),
        diagnostics_refs: Vec::new(),
    };

    collect_runtime_evidence(execution, &mut evidence);

    (evidence.runtime_invocation_id.is_some()
        || evidence.provider_id.is_some()
        || evidence.runtime_id.is_some()
        || evidence.failure_classification.is_some()
        || evidence.phase.is_some()
        || !evidence.artifact_refs.is_empty()
        || !evidence.transcript_refs.is_empty()
        || !evidence.result_refs.is_empty()
        || !evidence.diagnostics_refs.is_empty())
    .then_some(evidence)
}

fn collect_runtime_evidence(value: &Value, evidence: &mut ControllerActionRuntimeEvidence) {
    match value {
        Value::Object(object) => {
            set_first_string(
                &mut evidence.runtime_invocation_id,
                object
                    .get("runtime_invocation_id")
                    .or_else(|| object.get("invocation_id"))
                    .and_then(Value::as_str),
            );
            set_first_string(
                &mut evidence.provider_id,
                object
                    .get("provider_id")
                    .or_else(|| object.get("provider"))
                    .and_then(Value::as_str),
            );
            set_first_string(
                &mut evidence.runtime_id,
                object.get("runtime_id").and_then(Value::as_str),
            );
            set_first_string(
                &mut evidence.failure_classification,
                object.get("failure_classification").and_then(Value::as_str),
            );
            set_first_string(
                &mut evidence.phase,
                object
                    .get("phase")
                    .or_else(|| object.get("failure_phase"))
                    .and_then(Value::as_str),
            );

            if let Some(refs) = object.get("refs").and_then(Value::as_object) {
                collect_ref_group(refs.get("artifact_bundles"), &mut evidence.artifact_refs);
                collect_ref_group(refs.get("artifacts"), &mut evidence.artifact_refs);
                collect_ref_group(refs.get("logs"), &mut evidence.artifact_refs);
                collect_ref_group(refs.get("transcripts"), &mut evidence.transcript_refs);
                collect_ref_group(refs.get("results"), &mut evidence.result_refs);
                collect_ref_group(refs.get("diagnostics"), &mut evidence.diagnostics_refs);
            }

            collect_ref_field(object.get("artifact_bundle"), &mut evidence.artifact_refs);
            collect_ref_field(
                object.get("artifact_bundle_ref"),
                &mut evidence.artifact_refs,
            );
            collect_ref_field(object.get("artifact_ref"), &mut evidence.artifact_refs);
            collect_ref_field(object.get("artifact_path"), &mut evidence.artifact_refs);
            collect_ref_field(object.get("transcript_ref"), &mut evidence.transcript_refs);
            collect_ref_field(object.get("result_ref"), &mut evidence.result_refs);
            collect_ref_field(
                object.get("diagnostics_ref"),
                &mut evidence.diagnostics_refs,
            );

            for child in object.values() {
                collect_runtime_evidence(child, evidence);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_runtime_evidence(item, evidence);
            }
        }
        _ => {}
    }
}

fn set_first_string(target: &mut Option<String>, value: Option<&str>) {
    if target.is_none() {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            *target = Some(value.to_string());
        }
    }
}

fn collect_ref_group(value: Option<&Value>, target: &mut Vec<Value>) {
    match value {
        Some(Value::Array(items)) => {
            for item in items {
                push_ref_once(target, item.clone());
            }
        }
        Some(value) => push_ref_once(target, value.clone()),
        None => {}
    }
}

fn collect_ref_field(value: Option<&Value>, target: &mut Vec<Value>) {
    if let Some(value) = value.filter(|value| !value.is_null()) {
        push_ref_once(target, value.clone());
    }
}

fn push_ref_once(target: &mut Vec<Value>, value: Value) {
    if !target.iter().any(|existing| existing == &value) {
        target.push(value);
    }
}

pub(super) fn first_pending_action_id(record: &AgentTaskLoopControllerRecord) -> Option<String> {
    if !matches!(record.state, AgentTaskLoopControllerState::Running) {
        return None;
    }
    record
        .next_actions
        .iter()
        .find(|action| action.status == AgentTaskLoopActionStatus::Pending)
        .map(|action| action.action_id.clone())
}

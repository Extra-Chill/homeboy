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
            &[&["kind"], &["artifact_type"], &["data", "kind"]],
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
                .and_then(Value::as_str)
                == Some(artifact_id);
            let kind_matches = object
                .get("kind")
                .or_else(|| object.get("artifact_type"))
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
        label: artifact.name.clone().or_else(|| Some(artifact.id.clone())),
    }
}

fn artifact_ref_from_evidence_ref(evidence_ref: &AgentTaskEvidenceRef) -> AgentTaskLoopArtifactRef {
    AgentTaskLoopArtifactRef {
        uri: evidence_ref.uri.clone(),
        kind: Some(evidence_ref.kind.clone()),
        label: evidence_ref.label.clone(),
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
            label: Some(typed_artifact.name.clone()),
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
            && existing.label == artifact_ref.label
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

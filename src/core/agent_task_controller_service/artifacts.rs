//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;

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

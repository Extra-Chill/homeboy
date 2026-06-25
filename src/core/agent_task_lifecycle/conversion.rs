use super::*;

pub(crate) fn aggregate_artifacts(
    aggregate: Option<&AgentTaskAggregate>,
) -> Vec<AgentTaskArtifact> {
    aggregate
        .map(|aggregate| {
            aggregate
                .outcomes
                .iter()
                .flat_map(|outcome| outcome.artifacts.clone())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn aggregate_evidence_refs(
    aggregate: Option<&AgentTaskAggregate>,
    latest_executor_evidence: Option<&AgentTaskLatestExecutorEvidence>,
) -> Vec<AgentTaskEvidenceRef> {
    let mut refs: Vec<AgentTaskEvidenceRef> = aggregate
        .map(|aggregate| {
            aggregate
                .outcomes
                .iter()
                .flat_map(evidence_refs_for_outcome)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    refs.extend(
        latest_executor_evidence
            .into_iter()
            .flat_map(AgentTaskLatestExecutorEvidence::refs),
    );
    dedup_evidence_refs(&mut refs);
    refs
}

pub(crate) fn latest_executor_evidence(
    run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Option<AgentTaskLatestExecutorEvidence> {
    let outcome = aggregate.outcomes.last()?;
    let request = plan
        .tasks
        .iter()
        .find(|task| task.task_id == outcome.task_id)?;
    let task_id = outcome.task_id.clone();
    let base = format!("homeboy://agent-task/run/{run_id}");
    let component_contracts = if request.component_contracts.is_empty() {
        plan.component_contracts.clone()
    } else {
        request.component_contracts.clone()
    };

    Some(AgentTaskLatestExecutorEvidence {
        task_id: task_id.clone(),
        backend: request.executor.backend.clone(),
        selector: request.executor.selector.clone(),
        model: request.executor.model.clone(),
        input_ref: AgentTaskEvidenceRef {
            kind: "executor-input".to_string(),
            uri: format!("{base}/plan#task={task_id}"),
            label: Some("Latest raw executor input".to_string()),
        },
        normalized_output_ref: AgentTaskEvidenceRef {
            kind: "executor-normalized-output".to_string(),
            uri: format!("{base}/aggregate#outcome={task_id}"),
            label: Some("Latest normalized executor output".to_string()),
        },
        outcome_ref: AgentTaskEvidenceRef {
            kind: "executor-outcome".to_string(),
            uri: format!("{base}/artifacts#task={task_id}"),
            label: Some("Latest executor outcome evidence".to_string()),
        },
        provider_run_id: first_non_empty_json_string_value([
            outcome.metadata.get("provider_run_id"),
            outcome.metadata.get("remote_run_id"),
            outcome.metadata.pointer("/provider_handle/provider_run_id"),
            outcome.outputs.pointer("/provider_run_result/run_id"),
            outcome.outputs.pointer("/provider_run_result/id"),
        ]),
        runtime_component_paths: runtime_component_paths(request),
        expected_artifacts: request.expected_artifacts.clone(),
        typed_artifact_expectations: typed_artifact_expectations(request),
        component_contracts,
    })
}

pub(crate) fn runtime_component_paths(request: &AgentTaskRequest) -> Vec<String> {
    let mut paths: Vec<String> = request
        .component_contracts
        .iter()
        .filter_map(|contract| contract.path.clone())
        .collect();
    for pointer in [
        "/runtime_component_paths",
        "/runtime/component_paths",
        "/runtime/components",
        "/component_paths",
    ] {
        if let Some(values) = request.metadata.pointer(pointer).and_then(Value::as_array) {
            paths.extend(values.iter().filter_map(Value::as_str).map(str::to_string));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

pub(crate) fn typed_artifact_expectations(request: &AgentTaskRequest) -> Vec<String> {
    request
        .artifact_declarations
        .iter()
        .map(|declaration| declaration.name.clone())
        .collect()
}

pub(crate) fn evidence_refs_for_outcome(outcome: &AgentTaskOutcome) -> Vec<AgentTaskEvidenceRef> {
    outcome
        .evidence_refs
        .iter()
        .cloned()
        .chain(workflow_evidence_refs(outcome.workflow.as_ref()))
        .collect()
}

pub(crate) fn workflow_evidence_refs(
    workflow: Option<&AgentTaskWorkflowEvidence>,
) -> impl Iterator<Item = AgentTaskEvidenceRef> + '_ {
    workflow.into_iter().flat_map(|workflow| {
        workflow
            .steps
            .iter()
            .flat_map(|step| step.artifact_refs.iter().cloned())
    })
}

pub(crate) fn queued_task(request: &crate::core::agent_task::AgentTaskRequest) -> AgentTaskRunTask {
    AgentTaskRunTask {
        task_id: request.task_id.clone(),
        state: AgentTaskState::Queued,
        backend: request.executor.backend.clone(),
        selector: request.executor.selector.clone(),
        model: request.executor.model.clone(),
        provider_ref: request
            .executor
            .selector
            .as_ref()
            .map(|selector| format!("{}:{selector}", request.executor.backend)),
    }
}

pub(crate) fn tasks_for_aggregate(
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Vec<AgentTaskRunTask> {
    plan.tasks
        .iter()
        .map(|request| {
            let mut task = queued_task(request);
            if let Some(event) = aggregate
                .events
                .iter()
                .rev()
                .find(|event| event.task_id == request.task_id)
            {
                task.state = event.state;
            } else if let Some(outcome) = aggregate
                .outcomes
                .iter()
                .find(|outcome| outcome.task_id == request.task_id)
            {
                task.state = task_state_for_outcome_status(outcome.status);
            }
            task
        })
        .collect()
}

pub(crate) fn task_state_for_outcome_status(status: AgentTaskOutcomeStatus) -> AgentTaskState {
    match status {
        AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
            AgentTaskState::Succeeded
        }
        AgentTaskOutcomeStatus::Timeout => AgentTaskState::TimedOut,
        AgentTaskOutcomeStatus::Cancelled => AgentTaskState::Cancelled,
        _ => AgentTaskState::Failed,
    }
}

pub(crate) fn events_for_outcomes(outcomes: &[AgentTaskOutcome]) -> Vec<AgentTaskProgressEvent> {
    outcomes
        .iter()
        .map(|outcome| AgentTaskProgressEvent {
            task_id: outcome.task_id.clone(),
            state: task_state_for_outcome_status(outcome.status),
            attempt: 1,
            message: outcome.summary.clone(),
        })
        .collect()
}

pub(crate) fn queued_events(tasks: &[AgentTaskRunTask]) -> Vec<AgentTaskProgressEvent> {
    tasks
        .iter()
        .map(|task| AgentTaskProgressEvent {
            task_id: task.task_id.clone(),
            state: task.state,
            attempt: 1,
            message: Some("task submitted".to_string()),
        })
        .collect()
}

pub(crate) fn artifact_refs_for_outcomes(
    outcomes: &[AgentTaskOutcome],
) -> Vec<AgentTaskArtifactRef> {
    let mut refs: Vec<AgentTaskArtifactRef> = outcomes
        .iter()
        .flat_map(|outcome| {
            let artifact_refs = outcome.artifacts.iter().filter_map(|artifact| {
                first_non_empty_uri([artifact.url.as_deref(), artifact.path.as_deref()]).map(
                    |uri| AgentTaskArtifactRef {
                        task_id: outcome.task_id.clone(),
                        kind: artifact.kind.clone(),
                        uri: uri.to_string(),
                        role: artifact.declared_role().map(str::to_string),
                        label: artifact.display_label().map(str::to_string),
                        semantic_key: artifact.declared_semantic_key().map(str::to_string),
                        size_bytes: artifact.size_bytes,
                    },
                )
            });
            let evidence_refs = outcome
                .evidence_refs
                .iter()
                .cloned()
                .chain(workflow_evidence_refs(outcome.workflow.as_ref()))
                .filter_map(|evidence| {
                    first_non_empty_uri([Some(evidence.uri.as_str())]).map(|uri| {
                        AgentTaskArtifactRef {
                            task_id: outcome.task_id.clone(),
                            kind: evidence.kind.clone(),
                            uri: uri.to_string(),
                            role: None,
                            label: evidence.label.clone(),
                            semantic_key: None,
                            size_bytes: None,
                        }
                    })
                });
            artifact_refs.chain(evidence_refs).collect::<Vec<_>>()
        })
        .collect();
    dedup_preserve_order(&mut refs);
    refs
}

/// Returns the first URI candidate that is non-empty after trimming, mirroring
/// the `url` → `path` precedence used for agent-task artifacts. Empty or
/// whitespace-only URIs are treated as unavailable so status output never
/// surfaces refs with a blank `uri`.
pub(crate) fn first_non_empty_uri<'a>(
    candidates: impl IntoIterator<Item = Option<&'a str>>,
) -> Option<&'a str> {
    candidates
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|uri| !uri.is_empty())
}

pub(crate) fn first_non_empty_json_string_value<'a>(
    values: impl IntoIterator<Item = Option<&'a Value>>,
) -> Option<String> {
    values.into_iter().flatten().find_map(|value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

/// Drops exact-duplicate refs, keeping the first occurrence of each so status
/// output is not noisy when an artifact surfaces through both `artifacts` and
/// `evidence_refs` (or workflow evidence).
pub(crate) fn dedup_preserve_order(refs: &mut Vec<AgentTaskArtifactRef>) {
    let mut seen = std::collections::HashSet::new();
    refs.retain(|item| seen.insert(item.clone()));
}

pub(crate) fn dedup_evidence_refs(refs: &mut Vec<AgentTaskEvidenceRef>) {
    let mut seen = std::collections::HashSet::new();
    refs.retain(|item| seen.insert((item.kind.clone(), item.uri.clone())));
}

pub(crate) fn provider_handles_for_outcomes(
    outcomes: &[AgentTaskOutcome],
) -> Vec<AgentTaskRunProviderHandle> {
    outcomes
        .iter()
        .flat_map(provider_handles_for_outcome)
        .collect()
}

pub(crate) fn provider_handles_for_outcome(
    outcome: &AgentTaskOutcome,
) -> Vec<AgentTaskRunProviderHandle> {
    let mut handles = Vec::new();
    if let Some(handle) = outcome
        .metadata
        .get("provider_handle")
        .and_then(provider_handle_from_value)
    {
        handles.push(run_provider_handle(outcome, handle));
    }
    if let Some(values) = outcome
        .metadata
        .get("provider_handles")
        .and_then(Value::as_array)
    {
        handles.extend(
            values
                .iter()
                .filter_map(provider_handle_from_value)
                .map(|handle| run_provider_handle(outcome, handle)),
        );
    }
    if handles.is_empty() {
        if let Some(handle) = provider_handle_from_outcome_metadata(outcome) {
            handles.push(handle);
        }
    }
    handles
}

pub(crate) fn provider_handle_from_outcome_metadata(
    outcome: &AgentTaskOutcome,
) -> Option<AgentTaskRunProviderHandle> {
    let provider = outcome.metadata.get("provider").and_then(Value::as_str)?;
    let role_aliases = role_aliases_for_provider(provider);
    let provider_run_id = outcome
        .metadata
        .get("remote_run_id")
        .or_else(|| outcome.metadata.get("provider_run_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            provider_run_result(outcome, &role_aliases)
                .and_then(|result| result.get("run_id").or_else(|| result.get("id")))
                .and_then(Value::as_str)
        })?;

    Some(AgentTaskRunProviderHandle {
        kind: AgentTaskExecutionHandleKind::ProviderRun,
        task_id: outcome.task_id.clone(),
        backend: provider.to_string(),
        provider_run_id: provider_run_id.to_string(),
        stream_uri: outcome
            .metadata
            .get("stream_uri")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        state: Some(task_state_for_outcome_status(outcome.status)),
        metadata: outcome.metadata.clone(),
    })
}

pub(crate) fn normalize_provider_run_result(outcome: &mut AgentTaskOutcome) {
    if outcome.outputs.get("provider_run_result").is_some() {
        return;
    }
    let role_aliases = outcome
        .metadata
        .get("provider")
        .and_then(Value::as_str)
        .map(role_aliases_for_provider)
        .unwrap_or_default();
    if let Some(result) = provider_run_result(outcome, &role_aliases).cloned() {
        let mut outputs = outcome.outputs.as_object().cloned().unwrap_or_default();
        outputs.insert("provider_run_result".to_string(), result);
        outcome.outputs = Value::Object(outputs);
    }
}

pub(crate) fn provider_run_result<'a>(
    outcome: &'a AgentTaskOutcome,
    role_aliases: &AgentTaskProviderRoleAliases,
) -> Option<&'a Value> {
    outcome
        .outputs
        .get("provider_run_result")
        .or_else(|| {
            role_aliases
                .output_aliases_for_role("provider_run_result")
                .into_iter()
                .find_map(|alias| outcome.outputs.get(alias))
        })
        .or_else(|| {
            role_aliases
                .metadata_aliases_for_role("provider_run_result")
                .into_iter()
                .find_map(|alias| outcome.metadata.get(alias))
        })
}

pub(crate) fn provider_handle_from_value(value: &Value) -> Option<AgentTaskExecutionHandle> {
    serde_json::from_value(value.clone()).ok()
}

pub(crate) fn run_provider_handle(
    outcome: &AgentTaskOutcome,
    handle: AgentTaskExecutionHandle,
) -> AgentTaskRunProviderHandle {
    AgentTaskRunProviderHandle {
        kind: handle.kind,
        task_id: handle.task_id,
        backend: handle.backend,
        provider_run_id: handle.run_id,
        stream_uri: handle.stream_uri,
        state: Some(match outcome.status {
            crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded
            | crate::core::agent_task::AgentTaskOutcomeStatus::NoOp => AgentTaskState::Succeeded,
            crate::core::agent_task::AgentTaskOutcomeStatus::Timeout => AgentTaskState::TimedOut,
            crate::core::agent_task::AgentTaskOutcomeStatus::Cancelled => AgentTaskState::Cancelled,
            _ => AgentTaskState::Failed,
        }),
        metadata: handle.metadata,
    }
}

pub(crate) fn run_state_for_aggregate(aggregate: &AgentTaskAggregate) -> AgentTaskRunState {
    match aggregate.status {
        crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded => {
            AgentTaskRunState::Succeeded
        }
        crate::core::agent_task_scheduler::AgentTaskAggregateStatus::PartialFailure => {
            AgentTaskRunState::PartialFailure
        }
        crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Failed => {
            AgentTaskRunState::Failed
        }
        crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Cancelled => {
            AgentTaskRunState::Cancelled
        }
    }
}

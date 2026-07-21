use super::*;

pub(crate) fn lifecycle_for_submitted_plan(plan: &AgentTaskPlan) -> RunLifecycleRecord {
    let timestamp = now_timestamp();
    let mut lifecycle = RunLifecycleRecord::with_execution_state(RunExecutionState::Queued);
    lifecycle.updated_at = Some(timestamp.clone());
    lifecycle.execution.updated_at = Some(timestamp.clone());
    lifecycle.cleanup = cleanup_lifecycle_for_plan(plan, Some(timestamp.clone()));
    lifecycle.artifact_retention = ArtifactRetentionLifecycle {
        status: ArtifactRetentionStatus::Pending,
        policy: Some("retain".to_string()),
        updated_at: Some(timestamp),
    };
    lifecycle
}

/// Single authoritative setter for a run's state. It writes both the
/// top-level `record.state` and the derived `lifecycle.execution.state`
/// projection together, so the two cannot silently diverge: every state
/// transition routes through here instead of assigning `record.state`
/// separately from refreshing the lifecycle projection.
pub(crate) fn set_run_state(record: &mut AgentTaskRunRecord, state: AgentTaskRunState) {
    record.state = state;
    let timestamp = record.updated_at.clone().unwrap_or_else(now_timestamp);
    record.lifecycle.execution.state = RunExecutionState::from(state);
    record.lifecycle.execution.updated_at = Some(timestamp.clone());
    if state == AgentTaskRunState::Running && record.lifecycle.execution.started_at.is_none() {
        record.lifecycle.execution.started_at = Some(timestamp.clone());
    }
    // A terminal run has finished executing, so stamp `finished_at`. Use the
    // canonical terminal set (`is_terminal`) rather than a hand-listed subset:
    // the previous inline list omitted `CandidateRecoverable`, so a run that
    // finished with a recoverable candidate never got a `finished_at` here —
    // while the legacy-record migration path (`health.rs`) stamps it for every
    // non-Queued/Running state. This aligns the live setter with that path and
    // with the single terminal definition.
    if state.is_terminal() {
        record.lifecycle.execution.finished_at = Some(timestamp.clone());
    }
    record.lifecycle.updated_at = Some(timestamp);
    // `set_run_state` is the single writer that keeps the authoritative run
    // state and its generic lifecycle projection in lockstep. Assert the
    // invariant the record-health check enforces (`ConflictingProjections`) so a
    // future edit to this setter that breaks the pairing fails fast in dev/tests
    // rather than silently emitting divergent records.
    debug_assert!(
        record.run_state_projections_agree(),
        "set_run_state must leave run-state projections in agreement"
    );
}

pub(crate) fn update_lifecycle_heartbeat(record: &mut AgentTaskRunRecord) {
    let timestamp = record.updated_at.clone().unwrap_or_else(now_timestamp);
    record.lifecycle.heartbeat = Some(RunHeartbeat {
        last_seen_at: timestamp,
        owner_pid: record.owner_pid().or_else(|| Some(std::process::id())),
        stale_after_seconds: None,
    });
}

pub(crate) fn update_lifecycle_from_record(record: &mut AgentTaskRunRecord, plan: &AgentTaskPlan) {
    set_run_state(record, record.state);
    record.lifecycle.cleanup = cleanup_lifecycle_for_plan(plan, record.updated_at.clone());
    let durable_task_ids: std::collections::HashSet<&str> = record.metadata["provider_executions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|execution| execution["task_id"].as_str())
        .collect();
    let mut provider_runtime: Vec<ProviderRuntimeLifecycle> = record
        .provider_handles
        .iter()
        .filter(|handle| !durable_task_ids.contains(handle.task_id.as_str()))
        .map(provider_runtime_for_handle)
        .collect();
    for task in &record.tasks {
        let durable_executions = provider_executions_for_task(record, &task.task_id);
        if !durable_executions.is_empty() {
            provider_runtime.extend(durable_executions);
            continue;
        }
        if record
            .provider_handles
            .iter()
            .any(|handle| handle.task_id == task.task_id)
            || matches!(
                task.state,
                AgentTaskState::Queued | AgentTaskState::Blocked | AgentTaskState::Skipped
            )
        {
            continue;
        }
        // A completed generic executor may only produce the canonical aggregate
        // outcome, without a provider-native run id. Preserve its terminal
        // evidence rather than treating the missing external id as no execution.
        provider_runtime.push(ProviderRuntimeLifecycle {
            task_id: task.task_id.clone(),
            backend: task.backend.clone(),
            state: provider_runtime_state_for_task_state(Some(task.state)),
            stream_uri: None,
            external_runtime_ids: Vec::new(),
            metadata: json!({
                "evidence_source": "canonical_executor_outcome",
                "executor": {
                    "backend": task.backend,
                    "selector": task.selector,
                    "model": task.model,
                },
                "model": task.model,
            }),
        });
    }
    record.lifecycle.provider_runtime = provider_runtime;
    record.lifecycle.external_runtime_ids = record
        .lifecycle
        .provider_runtime
        .iter()
        .flat_map(|runtime| runtime.external_runtime_ids.clone())
        .collect();
    record.lifecycle.artifact_retention = ArtifactRetentionLifecycle {
        status: if record.artifact_refs.is_empty() {
            ArtifactRetentionStatus::NotApplicable
        } else {
            ArtifactRetentionStatus::Retained
        },
        policy: Some("retain".to_string()),
        updated_at: record.updated_at.clone(),
    };
}

pub(crate) fn reconcile_terminal_provider_models(
    record: &mut AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> bool {
    let mut changed = false;
    for outcome in &aggregate.outcomes {
        let Some(model) = outcome
            .metadata
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
        else {
            continue;
        };

        if let Some(task) = record
            .tasks
            .iter_mut()
            .find(|task| task.task_id == outcome.task_id)
        {
            changed |= replace_model(&mut task.model, model);
        }
        for handle in record
            .provider_handles
            .iter_mut()
            .filter(|handle| handle.task_id == outcome.task_id)
        {
            changed |= replace_metadata_model(&mut handle.metadata, model);
        }
        for runtime in record
            .lifecycle
            .provider_runtime
            .iter_mut()
            .filter(|runtime| runtime.task_id == outcome.task_id)
        {
            changed |= replace_metadata_model(&mut runtime.metadata, model);
            if let Some(executor) = runtime
                .metadata
                .get_mut("executor")
                .and_then(Value::as_object_mut)
            {
                changed |= replace_json_model(executor, model);
            }
        }
        if let Some(executions) = record
            .metadata
            .get_mut("provider_executions")
            .and_then(Value::as_array_mut)
        {
            for execution in executions.iter_mut().filter(|execution| {
                execution["task_id"] == outcome.task_id && execution["state"] == "succeeded"
            }) {
                if let Some(metadata) = execution.as_object_mut() {
                    changed |= replace_json_model(metadata, model);
                }
            }
        }
    }
    if changed {
        let timestamp = now_timestamp();
        record.updated_at = Some(timestamp.clone());
        record.lifecycle.updated_at = Some(timestamp);
    }
    changed
}

fn replace_model(current: &mut Option<String>, model: &str) -> bool {
    if current.as_deref() == Some(model) {
        return false;
    }
    *current = Some(model.to_string());
    true
}

fn replace_metadata_model(metadata: &mut Value, model: &str) -> bool {
    if !metadata.is_object() {
        *metadata = json!({});
    }
    replace_json_model(
        metadata.as_object_mut().expect("provider metadata object"),
        model,
    )
}

fn replace_json_model(metadata: &mut serde_json::Map<String, Value>, model: &str) -> bool {
    if metadata.get("model").and_then(Value::as_str) == Some(model) {
        return false;
    }
    metadata.insert("model".to_string(), json!(model));
    true
}

fn provider_executions_for_task(
    record: &AgentTaskRunRecord,
    task_id: &str,
) -> Vec<ProviderRuntimeLifecycle> {
    record.metadata["provider_executions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|execution| execution["task_id"] == task_id)
        .filter_map(|execution| {
            let backend = execution["backend"].as_str()?.to_string();
            let state = match execution["state"].as_str() {
                Some("running") => ProviderRuntimeState::Running,
                Some("succeeded") => ProviderRuntimeState::Succeeded,
                Some("cancelled") => ProviderRuntimeState::Cancelled,
                Some("timed_out") | Some("candidate_recoverable") => ProviderRuntimeState::TimedOut,
                _ => ProviderRuntimeState::Failed,
            };
            Some(ProviderRuntimeLifecycle {
                task_id: task_id.to_string(),
                backend,
                state,
                stream_uri: None,
                external_runtime_ids: Vec::new(),
                metadata: json!({
                    "evidence_source": "durable_provider_execution",
                    "execution_key": execution["key"],
                    "attempt": execution["attempt"],
                    "model": execution["model"],
                }),
            })
        })
        .collect()
}

pub(crate) fn cleanup_lifecycle_for_plan(
    plan: &AgentTaskPlan,
    updated_at: Option<String>,
) -> CleanupLifecycle {
    let policies: Vec<String> = plan
        .tasks
        .iter()
        .filter_map(|task| task.workspace.cleanup.clone())
        .collect();
    let preserved = policies.iter().any(|policy| policy == "preserve");
    CleanupLifecycle {
        state: if preserved {
            CleanupState::Preserved
        } else if policies.is_empty() {
            CleanupState::Unknown
        } else {
            CleanupState::Pending
        },
        policy: (!policies.is_empty()).then(|| policies.join(",")),
        updated_at,
    }
}

pub(crate) fn provider_runtime_for_handle(
    handle: &AgentTaskRunProviderHandle,
) -> ProviderRuntimeLifecycle {
    ProviderRuntimeLifecycle {
        task_id: handle.task_id.clone(),
        backend: handle.backend.clone(),
        state: provider_runtime_state_for_task_state(handle.state),
        stream_uri: handle.stream_uri.clone(),
        external_runtime_ids: vec![ExternalRuntimeId {
            kind: "provider_run_id".to_string(),
            value: handle.provider_run_id.clone(),
            provider: Some(handle.backend.clone()),
            url: handle.stream_uri.clone(),
        }],
        metadata: handle.metadata.clone(),
    }
}

pub(crate) fn provider_runtime_state_for_task_state(
    state: Option<AgentTaskState>,
) -> ProviderRuntimeState {
    match state {
        None | Some(AgentTaskState::Queued | AgentTaskState::Blocked | AgentTaskState::Skipped) => {
            ProviderRuntimeState::NotStarted
        }
        Some(AgentTaskState::Running) => ProviderRuntimeState::Running,
        Some(AgentTaskState::Succeeded) => ProviderRuntimeState::Succeeded,
        Some(AgentTaskState::CandidateRecoverable) => ProviderRuntimeState::TimedOut,
        Some(AgentTaskState::Failed) => ProviderRuntimeState::Failed,
        Some(AgentTaskState::Cancelled) => ProviderRuntimeState::Cancelled,
        Some(AgentTaskState::TimedOut) => ProviderRuntimeState::TimedOut,
    }
}

pub(crate) fn default_run_id() -> String {
    format!("agent-task-{}", Uuid::new_v4())
}

pub fn cook_attempt_run_id(cook_id: &str, attempt: u32) -> String {
    let cook_id = sanitize_run_id(cook_id);
    let suffix = Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..8];
    format!("{cook_id}-attempt-{attempt}-{suffix}")
}

pub(crate) fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

pub(crate) fn sanitize_run_id(run_id: &str) -> String {
    let sanitized = paths::sanitize_path_segment(run_id);
    if sanitized.is_empty() {
        default_run_id()
    } else {
        sanitized
    }
}

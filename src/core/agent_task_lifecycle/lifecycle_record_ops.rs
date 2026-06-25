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

pub(crate) fn update_lifecycle_execution(
    record: &mut AgentTaskRunRecord,
    state: AgentTaskRunState,
) {
    let timestamp = record.updated_at.clone().unwrap_or_else(now_timestamp);
    record.lifecycle.execution.state = execution_state_for_run_state(state);
    record.lifecycle.execution.updated_at = Some(timestamp.clone());
    if state == AgentTaskRunState::Running && record.lifecycle.execution.started_at.is_none() {
        record.lifecycle.execution.started_at = Some(timestamp.clone());
    }
    if matches!(
        state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        record.lifecycle.execution.finished_at = Some(timestamp.clone());
    }
    record.lifecycle.updated_at = Some(timestamp);
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
    update_lifecycle_execution(record, record.state);
    record.lifecycle.cleanup = cleanup_lifecycle_for_plan(plan, record.updated_at.clone());
    record.lifecycle.provider_runtime = record
        .provider_handles
        .iter()
        .map(provider_runtime_for_handle)
        .collect();
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

pub(crate) fn execution_state_for_run_state(state: AgentTaskRunState) -> RunExecutionState {
    match state {
        AgentTaskRunState::Queued => RunExecutionState::Queued,
        AgentTaskRunState::Running => RunExecutionState::Running,
        AgentTaskRunState::Succeeded => RunExecutionState::Succeeded,
        AgentTaskRunState::PartialFailure => RunExecutionState::PartialFailure,
        AgentTaskRunState::Failed => RunExecutionState::Failed,
        AgentTaskRunState::Cancelled => RunExecutionState::Cancelled,
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
        Some(AgentTaskState::Failed) => ProviderRuntimeState::Failed,
        Some(AgentTaskState::Cancelled) => ProviderRuntimeState::Cancelled,
        Some(AgentTaskState::TimedOut) => ProviderRuntimeState::TimedOut,
    }
}

pub(crate) fn default_run_id() -> String {
    format!("agent-task-{}", Uuid::new_v4())
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

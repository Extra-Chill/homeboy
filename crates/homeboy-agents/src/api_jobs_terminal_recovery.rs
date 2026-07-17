//! Agent-task implementation of the api-jobs terminal-recovery hook.
//!
//! Resolves a durable agent-task run's aggregate result into a recovered
//! terminal job for core's job store, provided through the
//! `AgentTaskTerminalRecoveryProvider` hook so the job store does not depend on
//! the agent-task subsystem directly.

use homeboy_core::api_jobs::agent_task_terminal_recovery::{
    recovered_terminal_job, register_agent_task_terminal_recovery_provider,
    AgentTaskTerminalRecoveryProvider,
};
use homeboy_core::api_jobs::{JobArtifactMetadata, JobStatus, RecoveredTerminalJob};

use crate::agent_task_scheduler::AgentTaskAggregateStatus;
use crate::agent_task_service;

struct AgentTaskTerminalRecoveryProviderImpl;

impl AgentTaskTerminalRecoveryProvider for AgentTaskTerminalRecoveryProviderImpl {
    fn recovered_terminal_agent_task_job(&self, run_id: &str) -> Option<RecoveredTerminalJob> {
        let result = agent_task_service::terminal_run_result(run_id).ok()??;
        let status = match result.value.status {
            AgentTaskAggregateStatus::Succeeded
            | AgentTaskAggregateStatus::CandidateRecoverable => JobStatus::Succeeded,
            AgentTaskAggregateStatus::Cancelled => JobStatus::Cancelled,
            AgentTaskAggregateStatus::PartialRecoverable
            | AgentTaskAggregateStatus::PartialFailure
            | AgentTaskAggregateStatus::Failed => JobStatus::Failed,
        };
        let run_id = run_id.to_string();
        let artifacts = result
            .value
            .artifact_bindings
            .iter()
            .map(|binding| JobArtifactMetadata {
                id: binding.artifact_id.clone(),
                name: binding.name.clone(),
                path: binding.path.clone(),
                url: binding.url.clone(),
                mime: None,
                size_bytes: None,
                sha256: binding.sha256.clone(),
                content_base64: None,
                metadata: Some(serde_json::json!({
                    "kind": binding.kind,
                    "task_id": binding.task_id,
                    "durable_run_id": run_id,
                })),
            })
            .collect();
        let terminal_result = serde_json::json!({
            "kind": "agent_task_aggregate",
            "run_id": &run_id,
            "exit_code": result.exit_code,
            "aggregate": result.value,
        });
        Some(recovered_terminal_job(
            status,
            terminal_result,
            run_id,
            artifacts,
        ))
    }
}

/// Register the agent-task terminal-recovery provider so core's job store can
/// recover terminal jobs from durable agent-task runs without depending on the
/// agent-task subsystem.
pub fn register() {
    register_agent_task_terminal_recovery_provider(Box::new(AgentTaskTerminalRecoveryProviderImpl));
}

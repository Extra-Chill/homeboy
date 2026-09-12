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
use homeboy_core::api_jobs::{
    DaemonLinkedDurableRunState, JobArtifactMetadata, JobStatus, RecoveredTerminalJob,
};

use crate::agent_task_scheduler::AgentTaskAggregateStatus;
use crate::agent_task_service;

struct AgentTaskTerminalRecoveryProviderImpl;

impl AgentTaskTerminalRecoveryProvider for AgentTaskTerminalRecoveryProviderImpl {
    fn recovered_terminal_agent_task_job(&self, run_id: &str) -> Option<RecoveredTerminalJob> {
        let result = agent_task_service::persisted_terminal_run_result(run_id).ok()??;
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

    fn linked_durable_run_state(&self, run_id: &str) -> Option<DaemonLinkedDurableRunState> {
        let record = crate::agent_task_lifecycle::exact_record(run_id).ok()?;
        if record.state.is_terminal() {
            Some(DaemonLinkedDurableRunState::Terminal)
        } else {
            Some(DaemonLinkedDurableRunState::Active)
        }
    }
}

/// Register the agent-task terminal-recovery provider so core's job store can
/// recover terminal jobs from durable agent-task runs without depending on the
/// agent-task subsystem.
pub fn register() {
    register_agent_task_terminal_recovery_provider(Box::new(AgentTaskTerminalRecoveryProviderImpl));
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use homeboy_core::api_jobs::{Job, RunnerJobLogSnapshot};
    use homeboy_core::run_lifecycle_record::{RunExecutionState, RunLifecycleRecord};
    use serde_json::json;

    use super::*;
    use crate::agent_task_lifecycle::{
        AgentTaskLabHandoff, AgentTaskLabHandoffAuthority, AgentTaskLabHandoffState,
        AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState, RunnerContinuationProvider,
        RunnerContinuationSubmission, RunnerContinuationTestGuard,
    };
    use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskAggregateTotals};

    struct CountingRunnerProvider(Arc<AtomicUsize>);

    impl RunnerContinuationProvider for CountingRunnerProvider {
        fn runner_job_log_snapshot(
            &self,
            _runner_id: &str,
            _job_id: &str,
        ) -> homeboy_core::Result<RunnerJobLogSnapshot> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(homeboy_core::Error::internal_unexpected(
                "runner status must not be queried during terminal recovery",
            ))
        }

        fn is_runner_connected(&self, _runner_id: &str) -> bool {
            self.0.fetch_add(1, Ordering::SeqCst);
            true
        }

        fn run_continuation_exec(
            &self,
            _runner_id: &str,
            _cwd: &str,
            _command: &[String],
            _run_id: &str,
        ) -> homeboy_core::Result<i32> {
            Err(homeboy_core::Error::internal_unexpected("not used"))
        }

        fn submit_runner_api_request(
            &self,
            _runner_id: &str,
            _submission: RunnerContinuationSubmission,
        ) -> homeboy_core::Result<Job> {
            Err(homeboy_core::Error::internal_unexpected("not used"))
        }
    }

    #[test]
    fn terminal_recovery_uses_persisted_records_without_reentering_runner_status() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            let run_id = "runner-linked-terminal-recovery";
            let record = AgentTaskRunRecord {
                schema: crate::agent_task_lifecycle::schemas::RUN.to_string(),
                run_id: run_id.to_string(),
                plan_id: "plan".to_string(),
                state: AgentTaskRunState::Running,
                submitted_at: "2026-09-12T00:00:00Z".to_string(),
                updated_at: None,
                plan_path: store.controller_plan_path(run_id).display().to_string(),
                aggregate_path: None,
                totals: None,
                tasks: Vec::new(),
                artifact_refs: Vec::new(),
                provider_handles: Vec::new(),
                latest_executor_evidence: None,
                lifecycle: RunLifecycleRecord::with_execution_state(RunExecutionState::Running),
                lab_handoff: Some(AgentTaskLabHandoff {
                    state: AgentTaskLabHandoffState::Accepted,
                    authority: AgentTaskLabHandoffAuthority::RunnerDaemon,
                    runner_id: "homeboy-lab".to_string(),
                    submission_key: None,
                    payload_fingerprint: None,
                    runner_job_id: Some("runner-job".to_string()),
                    submitted_at: None,
                    acceptance_deadline_at: None,
                    accepted_at: Some("2026-09-12T00:00:01Z".to_string()),
                    expired_at: None,
                    workspace_identity: None,
                    workspace_lifecycle_revision: 0,
                    workspace_owner_lease: None,
                    workspace_claim: None,
                }),
                candidate_adoption: None,
                adoption_run_id: None,
                acceptance: None,
                workspace_identity: None,
                workspace_lifecycle_revision: 0,
                workspace_owner_lease: None,
                workspace_claim: None,
                metadata: json!({}),
            };
            store.write_record(&record).expect("write linked record");

            let calls = Arc::new(AtomicUsize::new(0));
            let _guard = RunnerContinuationTestGuard::install(Box::new(CountingRunnerProvider(
                Arc::clone(&calls),
            )));
            assert_eq!(
                AgentTaskTerminalRecoveryProviderImpl.linked_durable_run_state(run_id),
                Some(DaemonLinkedDurableRunState::Active)
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);

            let mut terminal_record = record;
            terminal_record.state = AgentTaskRunState::Succeeded;
            terminal_record.lifecycle =
                RunLifecycleRecord::with_execution_state(RunExecutionState::Succeeded);
            store
                .write_aggregate(
                    run_id,
                    &AgentTaskAggregate {
                        schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                        plan_id: "plan".to_string(),
                        status: AgentTaskAggregateStatus::Succeeded,
                        totals: AgentTaskAggregateTotals::default(),
                        outcomes: Vec::new(),
                        events: Vec::new(),
                        artifact_lineage: Vec::new(),
                        child_runs: Vec::new(),
                        artifact_bindings: Vec::new(),
                        queue: Default::default(),
                    },
                )
                .expect("write terminal aggregate");
            store
                .write_record(&terminal_record)
                .expect("write terminal record");
            assert!(AgentTaskTerminalRecoveryProviderImpl
                .recovered_terminal_agent_task_job(run_id)
                .is_some());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        });
    }
}

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
    fn durable_run_is_cancelled(&self, run_id: &str) -> bool {
        let Ok(store) =
            crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
        else {
            return false;
        };
        store.read_record_bounded(run_id).is_ok_and(|record| {
            record.run_id == run_id
                && record.state() == crate::agent_task_lifecycle::AgentTaskRunState::Cancelled
        })
    }

    fn recovered_terminal_agent_task_job(&self, run_id: &str) -> Option<RecoveredTerminalJob> {
        let resolved_run_id = crate::agent_task_lifecycle::resolve_run_id(run_id).ok()?;
        let result = agent_task_service::persisted_terminal_run_result(&resolved_run_id).ok()??;
        let status = match result.value.status() {
            AgentTaskAggregateStatus::Succeeded => JobStatus::Succeeded,
            AgentTaskAggregateStatus::Cancelled => JobStatus::Cancelled,
            AgentTaskAggregateStatus::PartialRecoverable
            | AgentTaskAggregateStatus::PartialFailure
            | AgentTaskAggregateStatus::Failed => JobStatus::Failed,
        };
        let run_id = resolved_run_id;
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
        let lifecycle_store =
            crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                .ok()?;
        let resolved_run_id =
            crate::agent_task_lifecycle::resolve_run_id_in_store(&lifecycle_store, run_id).ok()?;
        let record = lifecycle_store.read_record_bounded(&resolved_run_id).ok()?;
        if record.state().is_terminal() {
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

    use homeboy_core::api_jobs::RunnerJobLogSnapshot;
    use homeboy_core::run_lifecycle_record::{RunExecutionState, RunLifecycleRecord};
    use serde_json::json;

    use super::*;
    use crate::agent_task_lifecycle::{
        AgentTaskLabHandoff, AgentTaskLabHandoffAuthority, AgentTaskLabHandoffState,
        AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState, RunnerContinuationProvider,
        RunnerContinuationTestGuard,
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
    }

    #[test]
    fn terminal_recovery_uses_persisted_records_without_reentering_runner_status() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            let run_id = "runner-linked-terminal-recovery";
            let record = {
                let mut record = AgentTaskRunRecord::new_queued(
                    run_id.to_string(),
                    "plan".to_string(),
                    store.controller_plan_path(run_id).display().to_string(),
                    "2026-09-12T00:00:00Z".to_string(),
                );
                record.lifecycle =
                    RunLifecycleRecord::with_execution_state(RunExecutionState::Running);
                record.lab_handoff = Some(AgentTaskLabHandoff {
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
                });
                record.metadata = json!({});
                record.set_state_for_test(AgentTaskRunState::Running);
                record
            };
            store.write_record(&record).expect("write linked record");
            store
                .write_cook_index_attempt(
                    "cook-alias-for-terminal-recovery",
                    1,
                    run_id,
                    "2026-09-12T00:00:00Z".to_string(),
                    None,
                )
                .expect("write cook alias");

            let calls = Arc::new(AtomicUsize::new(0));
            let _guard = RunnerContinuationTestGuard::install(Box::new(CountingRunnerProvider(
                Arc::clone(&calls),
            )));
            assert_eq!(
                AgentTaskTerminalRecoveryProviderImpl.linked_durable_run_state(run_id),
                Some(DaemonLinkedDurableRunState::Active)
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);

            assert!(!AgentTaskTerminalRecoveryProviderImpl.durable_run_is_cancelled(run_id));
            assert!(!AgentTaskTerminalRecoveryProviderImpl.durable_run_is_cancelled("missing-run"));
            let mut cancelled = record.clone();
            cancelled.run_id = "cancelled-without-aggregate".to_string();
            cancelled.set_state_for_test(AgentTaskRunState::Cancelled);
            cancelled.lifecycle =
                RunLifecycleRecord::with_execution_state(RunExecutionState::Cancelled);
            store
                .write_record(&cancelled)
                .expect("write canonical cancellation");
            assert!(
                AgentTaskTerminalRecoveryProviderImpl.durable_run_is_cancelled(&cancelled.run_id)
            );
            assert!(!AgentTaskTerminalRecoveryProviderImpl
                .durable_run_is_cancelled("cook-alias-for-terminal-recovery"));

            let mut terminal_record = record;
            terminal_record.set_state_for_test(AgentTaskRunState::Succeeded);
            terminal_record.lifecycle =
                RunLifecycleRecord::with_execution_state(RunExecutionState::Succeeded);
            // The record says Succeeded, so the aggregate carries a succeeded outcome.
            let outcomes = vec![crate::agent_task::AgentTaskOutcome {
                task_id: "task".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
                ..Default::default()
            }];
            store
                .write_aggregate(
                    run_id,
                    &AgentTaskAggregate {
                        schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                        plan_id: "plan".to_string(),
                        totals: AgentTaskAggregateTotals::from_outcomes(1, &outcomes),
                        outcomes,
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
            assert!(AgentTaskTerminalRecoveryProviderImpl
                .recovered_terminal_agent_task_job("cook-alias-for-terminal-recovery")
                .is_some());
            assert!(!AgentTaskTerminalRecoveryProviderImpl.durable_run_is_cancelled(run_id));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        });
    }
}

//! Agent-task terminal-recovery hook.
//!
//! When a remote-runner job carried a durable agent-task run, the job store can
//! recover the job's terminal outcome from that run's aggregate result. Looking
//! up the durable run and its aggregate is agent-task behavior, so it is
//! inverted behind this provider: core owns the job store and extracts the
//! run id from the (opaque) workload, the agent-task layer resolves the run's
//! terminal result into a recovered job.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! recovers nothing.

use std::sync::Mutex;

use serde_json::Value;

use super::store::RecoveredTerminalJob;

/// Resolves a durable agent-task run's terminal outcome into a recovered job.
pub trait AgentTaskTerminalRecoveryProvider: Send + Sync {
    /// Recover the terminal job for the durable agent-task `run_id`, or `None`
    /// when the run has no terminal result.
    fn recovered_terminal_agent_task_job(&self, run_id: &str) -> Option<RecoveredTerminalJob>;
}

struct NoopProvider;

impl AgentTaskTerminalRecoveryProvider for NoopProvider {
    fn recovered_terminal_agent_task_job(&self, _run_id: &str) -> Option<RecoveredTerminalJob> {
        None
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn AgentTaskTerminalRecoveryProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn AgentTaskTerminalRecoveryProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the agent-task terminal-recovery provider. Called once at startup by
/// the agent-task layer.
pub fn register_agent_task_terminal_recovery_provider(
    provider: Box<dyn AgentTaskTerminalRecoveryProvider>,
) {
    let mut slot = provider_slot()
        .lock()
        .expect("agent-task terminal recovery provider lock");
    *slot = Some(provider);
}

/// Recover the terminal job for a durable agent-task run via the registered
/// provider (or none when the agent-task subsystem is absent).
pub(crate) fn recovered_terminal_agent_task_job(run_id: &str) -> Option<RecoveredTerminalJob> {
    let slot = provider_slot()
        .lock()
        .expect("agent-task terminal recovery provider lock");
    match slot.as_deref() {
        Some(provider) => provider.recovered_terminal_agent_task_job(run_id),
        None => NoopProvider.recovered_terminal_agent_task_job(run_id),
    }
}

/// Build a [`RecoveredTerminalJob`] from its parts. Used by the agent-task
/// provider implementation to construct the core recovery type.
pub fn recovered_terminal_job(
    status: super::JobStatus,
    terminal_result: Value,
    run_id: String,
    artifacts: Vec<super::JobArtifactMetadata>,
) -> RecoveredTerminalJob {
    RecoveredTerminalJob::new(status, terminal_result, run_id, artifacts)
}

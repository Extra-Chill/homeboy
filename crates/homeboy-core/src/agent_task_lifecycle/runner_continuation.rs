//! Runner-continuation hook.
//!
//! The agent-task lifecycle can reconcile and resume a run that was dispatched
//! to an optional remote runner (Lab offload). That work is the ONLY part of
//! lifecycle reconciliation that depends on the runner subsystem, so it is
//! inverted behind this provider trait: `homeboy-core` owns the lifecycle
//! logic and calls a registered provider for the handful of genuinely-remote
//! operations, while the optional runner crate supplies the implementation.
//!
//! On a single-machine install no provider is registered and the [`NoopProvider`]
//! degrades exactly as a disconnected runner would: snapshots and execution
//! fail (so the caller annotates "runner disconnected"), and existence /
//! connection checks report `false`.

use std::sync::Mutex;

use crate::api_jobs::RunnerJobLogSnapshot;
use crate::error::{Error, Result};

/// Runner-side operations the agent-task lifecycle needs when reconciling or
/// resuming a run that was handed off to a remote runner.
pub trait RunnerContinuationProvider: Send + Sync {
    /// Durable snapshot (job + event log) for a runner job.
    fn runner_job_log_snapshot(
        &self,
        runner_id: &str,
        job_id: &str,
    ) -> Result<RunnerJobLogSnapshot>;

    /// Whether the runner currently reports a live connection.
    fn is_runner_connected(&self, runner_id: &str) -> bool;

    /// Whether a runner with this id is configured/registered.
    fn runner_exists(&self, runner_id: &str) -> bool;

    /// Execute a continuation command on the runner, returning the exit code.
    fn run_continuation_exec(
        &self,
        runner_id: &str,
        cwd: &str,
        command: &[String],
        run_id: &str,
    ) -> Result<i32>;
}

/// Default provider used when the runner subsystem is not present. Behaves like
/// a disconnected / absent runner.
struct NoopProvider;

impl RunnerContinuationProvider for NoopProvider {
    fn runner_job_log_snapshot(
        &self,
        _runner_id: &str,
        _job_id: &str,
    ) -> Result<RunnerJobLogSnapshot> {
        Err(Error::internal_unexpected(
            "runner subsystem is unavailable: cannot read runner job log snapshot",
        ))
    }

    fn is_runner_connected(&self, _runner_id: &str) -> bool {
        false
    }

    fn runner_exists(&self, _runner_id: &str) -> bool {
        false
    }

    fn run_continuation_exec(
        &self,
        _runner_id: &str,
        _cwd: &str,
        _command: &[String],
        _run_id: &str,
    ) -> Result<i32> {
        Err(Error::internal_unexpected(
            "runner subsystem is unavailable: cannot execute runner continuation",
        ))
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn RunnerContinuationProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn RunnerContinuationProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the runner-continuation provider. Called once at startup by the
/// runner subsystem when it is present.
pub fn register_runner_continuation_provider(provider: Box<dyn RunnerContinuationProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("runner continuation provider lock");
    *slot = Some(provider);
}

/// Run `f` against the registered provider, falling back to the no-op provider
/// when the runner subsystem is absent.
pub(crate) fn with_runner_continuation<R>(
    f: impl FnOnce(&dyn RunnerContinuationProvider) -> R,
) -> R {
    let slot = provider_slot()
        .lock()
        .expect("runner continuation provider lock");
    match slot.as_deref() {
        Some(provider) => f(provider),
        None => f(&NoopProvider),
    }
}

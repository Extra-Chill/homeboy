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

use homeboy_core::api_jobs::{Job, RemoteRunnerJobRequest, RunnerJobLogSnapshot};
use homeboy_core::error::{Error, Result};

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

    /// Submit a replayable reverse-broker request during lifecycle reconciliation.
    fn submit_reverse_broker_job(
        &self,
        runner_id: &str,
        request: RemoteRunnerJobRequest,
    ) -> Result<Job>;
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

    fn submit_reverse_broker_job(
        &self,
        _runner_id: &str,
        _request: RemoteRunnerJobRequest,
    ) -> Result<Job> {
        Err(Error::internal_unexpected(
            "runner subsystem is unavailable: cannot submit reverse broker job",
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

/// Clear any registered runner-continuation provider so a fresh test starts from
/// the no-op default. The provider slot is a process-global; without this reset a
/// provider registered by one test would leak into every later test in the same
/// process, making lifecycle results order-dependent (#8964).
#[cfg(any(test, feature = "test-support"))]
pub fn clear_runner_continuation_provider_for_test() {
    let mut slot = provider_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = None;
}

/// RAII guard that installs a runner-continuation provider for the duration of a
/// test and restores the no-op default on drop (including on panic), so the
/// registration cannot leak into another test.
#[cfg(any(test, feature = "test-support"))]
pub struct RunnerContinuationTestGuard {
    _private: (),
}

#[cfg(any(test, feature = "test-support"))]
impl RunnerContinuationTestGuard {
    pub fn install(provider: Box<dyn RunnerContinuationProvider>) -> Self {
        register_runner_continuation_provider(provider);
        Self { _private: () }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for RunnerContinuationTestGuard {
    fn drop(&mut self) {
        clear_runner_continuation_provider_for_test();
    }
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

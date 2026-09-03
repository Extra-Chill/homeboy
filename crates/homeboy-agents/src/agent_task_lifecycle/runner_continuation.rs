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

use homeboy_core::api_jobs::{
    Job, RemoteRunnerJobRequest, RemoteRunnerSubmissionLookup, RunnerJobLogSnapshot,
};
use homeboy_core::error::{Error, Result};
use homeboy_runner_contract::{RunnerApiSubmitRequest, WorkspaceClaim, WorkspaceIdentity};

/// Result of reconciling a runner job across its known daemon generations.
///
/// An accepted handoff is terminal only when every authoritative generation
/// confirms the job is absent. Transport failures remain deliberately
/// unconfirmed so a transient daemon error cannot discard runner-owned work.
pub enum RunnerJobReconciliation {
    Snapshot(Box<RunnerJobLogSnapshot>),
    ConfirmedAbsent { checked_generations: usize },
    UnconfirmedAbsence,
}

/// What the current runner provider can authoritatively establish about a
/// durable runner id. Unknown must retain runner ownership: unavailable
/// provider/configuration evidence cannot prove a removed authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerAuthority {
    Configured,
    Removed,
    Unknown,
}

impl RunnerAuthority {
    pub fn is_configured(self) -> bool {
        matches!(self, Self::Configured)
    }
}

/// Authoritative live-job evidence for one runner after generation
/// reconciliation. Unknown must keep runner-generation ownership: a missing
/// probe cannot prove the daemon is idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerLiveJobAuthority {
    Idle,
    Busy,
    Unknown,
}

/// One reverse-broker submission operation. Current callers use the Runner API;
/// the legacy variant exists only to replay request-shaped durable records
/// without changing their established idempotency fingerprint.
pub enum RunnerContinuationSubmission {
    RunnerApi(RunnerApiSubmitRequest),
    LegacyReplay(RemoteRunnerJobRequest),
}

/// Runner-side operations the agent-task lifecycle needs when reconciling or
/// resuming a run that was handed off to a remote runner.
pub trait RunnerContinuationProvider: Send + Sync {
    /// `false` is a fail-closed declaration for older providers. Ordinary jobs
    /// do not call these methods and retain their existing compatibility.
    fn supports_workspace_claims(&self) -> bool {
        false
    }

    /// Explicit capability gate for terminal historical inspection. Older
    /// providers fail closed rather than turning an unavailable probe into
    /// absent-job evidence.
    fn supports_terminal_workspace_authority(&self) -> bool {
        false
    }

    /// Configured remote authorities which must participate in a workspace
    /// claim. The default keeps older providers compatible for local-only runs.
    fn workspace_claim_runner_ids(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Every configured direct or reverse runner authority which must attest a
    /// terminal workspace. Implementations may use a stricter set than claims.
    fn terminal_workspace_authority_runner_ids(&self) -> Result<Vec<String>> {
        self.workspace_claim_runner_ids()
    }

    fn acquire_workspace_claim(
        &self,
        _runner_id: &str,
        _workspace: WorkspaceIdentity,
        _lifecycle_revision: u64,
    ) -> Result<WorkspaceClaim> {
        Err(Error::validation_invalid_argument(
            "workspace_claim",
            "runner does not advertise workspace claim capability",
            None,
            None,
        ))
    }

    fn validate_workspace_claim(&self, _runner_id: &str, _claim: &WorkspaceClaim) -> Result<bool> {
        Ok(false)
    }

    fn release_workspace_claim(&self, _runner_id: &str, _claim: &WorkspaceClaim) -> Result<()> {
        Err(Error::validation_invalid_argument(
            "workspace_claim",
            "runner does not advertise workspace claim capability",
            None,
            None,
        ))
    }

    /// Token-free inventory for bounded write-ahead-intent recovery. `Ok(true)`
    /// means the authority confirms no live claim or owner for this identity.
    fn workspace_claim_authority_is_clear(
        &self,
        _runner_id: &str,
        _workspace: &WorkspaceIdentity,
    ) -> Result<bool> {
        Err(Error::validation_invalid_argument(
            "workspace_claim",
            "runner does not advertise workspace claim authority inventory",
            None,
            None,
        ))
    }
    /// Durable snapshot (job + event log) for a runner job.
    fn runner_job_log_snapshot(
        &self,
        runner_id: &str,
        job_id: &str,
    ) -> Result<RunnerJobLogSnapshot>;

    /// Reconcile a job against the daemon generation that owns it and, when
    /// necessary, other known generations. Providers without generation
    /// ownership support retain the conservative snapshot-only behavior.
    fn reconcile_runner_job(&self, runner_id: &str, job_id: &str) -> RunnerJobReconciliation {
        match self.runner_job_log_snapshot(runner_id, job_id) {
            Ok(snapshot) => RunnerJobReconciliation::Snapshot(Box::new(snapshot)),
            Err(_) => RunnerJobReconciliation::UnconfirmedAbsence,
        }
    }

    /// Recover an accepted runner job whose response did not reach the
    /// controller. The durable run id is the daemon's idempotency key, so a
    /// unique matching active job is sufficient to bind the handoff safely.
    fn runner_job_id_for_durable_run(
        &self,
        _runner_id: &str,
        _durable_run_id: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }

    /// Whether the runner currently reports a live connection.
    fn is_runner_connected(&self, runner_id: &str) -> bool;

    /// Legacy two-state runner inventory retained for external provider
    /// compatibility. New providers should implement [`Self::runner_authority`]
    /// so an unavailable inventory remains distinct from a removed runner.
    fn runner_exists(&self, _runner_id: &str) -> bool {
        false
    }

    /// Authoritative configuration state for a durable runner id. Providers
    /// that cannot inspect their configuration return `Unknown` fail-closed.
    fn runner_authority(&self, runner_id: &str) -> RunnerAuthority {
        if self.runner_exists(runner_id) {
            RunnerAuthority::Configured
        } else {
            RunnerAuthority::Unknown
        }
    }

    /// Live-job evidence after runner-generation reconciliation. Default is
    /// unknown so older providers cannot turn a missing probe into idle proof.
    fn runner_live_job_authority(&self, _runner_id: &str) -> RunnerLiveJobAuthority {
        RunnerLiveJobAuthority::Unknown
    }

    /// Execute a continuation command on the runner, returning the exit code.
    fn run_continuation_exec(
        &self,
        runner_id: &str,
        cwd: &str,
        command: &[String],
        run_id: &str,
    ) -> Result<i32>;

    /// Submit a replayable reverse-broker request during lifecycle reconciliation.
    fn submit_runner_api_request(
        &self,
        runner_id: &str,
        submission: RunnerContinuationSubmission,
    ) -> Result<Job>;

    fn lookup_reverse_broker_submission(
        &self,
        _runner_id: &str,
        _submission_key: &str,
    ) -> Result<RemoteRunnerSubmissionLookup> {
        Err(Error::internal_unexpected(
            "runner subsystem is unavailable: cannot look up reverse broker submission",
        ))
    }
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

    fn submit_runner_api_request(
        &self,
        _runner_id: &str,
        _submission: RunnerContinuationSubmission,
    ) -> Result<Job> {
        Err(Error::internal_unexpected(
            "runner subsystem is unavailable: cannot submit reverse broker job",
        ))
    }
}

#[cfg(not(test))]
homeboy_engine_primitives::provider_registry! {
    provider: dyn RunnerContinuationProvider,
    noop: NoopProvider,
    /// Register the runner-continuation provider. Called once at startup by the
    /// runner subsystem when it is present.
    register: pub fn register_runner_continuation_provider,
    /// Run `f` against the registered provider, falling back to the no-op provider
    /// when the runner subsystem is absent.
    with: pub(crate) fn with_runner_continuation,
}

#[cfg(test)]
thread_local! {
    static TEST_PROVIDER: std::cell::RefCell<Option<Box<dyn RunnerContinuationProvider>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub fn register_runner_continuation_provider(provider: Box<dyn RunnerContinuationProvider>) {
    TEST_PROVIDER.with(|slot| *slot.borrow_mut() = Some(provider));
}

#[cfg(test)]
pub(crate) fn with_runner_continuation<T>(
    operation: impl FnOnce(&dyn RunnerContinuationProvider) -> T,
) -> T {
    TEST_PROVIDER.with(|slot| {
        let provider = slot.borrow();
        match provider.as_deref() {
            Some(provider) => operation(provider),
            None => operation(&NoopProvider),
        }
    })
}

/// Resolve the current provider's authoritative configuration state for a
/// durable runner id. A missing provider remains unknown, not removed.
pub fn runner_authority(runner_id: &str) -> RunnerAuthority {
    with_runner_continuation(|provider| provider.runner_authority(runner_id))
}

/// Resolve whether a runner currently has zero live jobs and a resolved
/// generation projection. A missing provider remains unknown, not idle.
pub fn runner_live_job_authority(runner_id: &str) -> RunnerLiveJobAuthority {
    with_runner_continuation(|provider| provider.runner_live_job_authority(runner_id))
}

/// Clear any registered runner-continuation provider so a fresh test starts from
/// the no-op default. The provider slot is a process-global; without this reset a
/// provider registered by one test would leak into every later test in the same
/// process, making lifecycle results order-dependent (#8964).
#[cfg(any(test, feature = "test-support"))]
pub fn clear_runner_continuation_provider_for_test() {
    let _guard = runner_continuation_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    clear_runner_continuation_provider_for_test_unlocked();
}

#[cfg(any(test, feature = "test-support"))]
fn clear_runner_continuation_provider_for_test_unlocked() {
    #[cfg(test)]
    {
        TEST_PROVIDER.with(|slot| *slot.borrow_mut() = None);
    }
    #[cfg(not(test))]
    {
        let mut slot = provider_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = None;
    }
}

#[cfg(any(test, feature = "test-support"))]
fn runner_continuation_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

/// RAII guard that installs a runner-continuation provider for the duration of a
/// test and restores the no-op default on drop (including on panic), so the
/// registration cannot leak into another test.
#[cfg(any(test, feature = "test-support"))]
pub struct RunnerContinuationTestGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(any(test, feature = "test-support"))]
impl RunnerContinuationTestGuard {
    pub fn install(provider: Box<dyn RunnerContinuationProvider>) -> Self {
        let guard = runner_continuation_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        register_runner_continuation_provider(provider);
        Self { _guard: guard }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for RunnerContinuationTestGuard {
    fn drop(&mut self) {
        clear_runner_continuation_provider_for_test_unlocked();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LegacyProvider {
        runner_exists: bool,
    }

    impl RunnerContinuationProvider for LegacyProvider {
        fn runner_job_log_snapshot(
            &self,
            _runner_id: &str,
            _job_id: &str,
        ) -> Result<RunnerJobLogSnapshot> {
            Err(Error::internal_unexpected("unused in fixture"))
        }

        fn is_runner_connected(&self, _runner_id: &str) -> bool {
            false
        }

        fn runner_exists(&self, _runner_id: &str) -> bool {
            self.runner_exists
        }

        fn run_continuation_exec(
            &self,
            _runner_id: &str,
            _cwd: &str,
            _command: &[String],
            _run_id: &str,
        ) -> Result<i32> {
            Err(Error::internal_unexpected("unused in fixture"))
        }

        fn submit_runner_api_request(
            &self,
            _runner_id: &str,
            _submission: RunnerContinuationSubmission,
        ) -> Result<Job> {
            Err(Error::internal_unexpected("unused in fixture"))
        }
    }

    #[test]
    fn default_live_job_authority_is_unknown() {
        assert_eq!(
            LegacyProvider {
                runner_exists: true,
            }
            .runner_live_job_authority("legacy-runner"),
            RunnerLiveJobAuthority::Unknown
        );
    }

    #[test]
    fn legacy_runner_exists_cannot_prove_a_runner_was_removed() {
        assert_eq!(
            LegacyProvider {
                runner_exists: true,
            }
            .runner_authority("legacy-runner"),
            RunnerAuthority::Configured
        );
        assert_eq!(
            LegacyProvider {
                runner_exists: false,
            }
            .runner_authority("legacy-runner"),
            RunnerAuthority::Unknown
        );
    }
}

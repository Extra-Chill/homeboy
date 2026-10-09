//! Direct runner-daemon evidence for controller-upgrade admission.

/// Direct runner-daemon evidence used to distinguish stale ownership from
/// genuinely unverified active work during controller version upgrades.
pub trait RunnerDirectActiveJobsProvider: Send + Sync {
    fn direct_active_job_count(&self, runner_id: &str) -> Option<usize>;
}

struct NoopRunnerDirectActiveJobsProvider;

impl RunnerDirectActiveJobsProvider for NoopRunnerDirectActiveJobsProvider {
    fn direct_active_job_count(&self, _runner_id: &str) -> Option<usize> {
        None
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn RunnerDirectActiveJobsProvider,
    noop: NoopRunnerDirectActiveJobsProvider,
    register: pub fn register_runner_direct_active_jobs_provider,
    with: pub(crate) fn with_runner_direct_active_jobs_provider,
}

pub fn runner_direct_active_job_count(runner_id: &str) -> Option<usize> {
    with_runner_direct_active_jobs_provider(|provider| provider.direct_active_job_count(runner_id))
}

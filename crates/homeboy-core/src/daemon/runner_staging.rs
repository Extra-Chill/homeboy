//! Runner-owned sealed staging hook for authenticated daemon/broker routes.

use serde_json::Value;

use crate::api_jobs::JobStore;
use crate::error::Result;

pub trait RunnerStagingProvider: Send + Sync {
    fn capabilities(&self, runner_id: &str) -> Result<Vec<String>>;
    /// The daemon-owned queue is passed through this narrow boundary so staging
    /// admits the real execution job instead of inventing a second executor.
    fn stage(&self, request: Value, jobs: &JobStore) -> Result<Value>;
}

struct NoopProvider;

impl RunnerStagingProvider for NoopProvider {
    fn capabilities(&self, _runner_id: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn stage(&self, _request: Value, _jobs: &JobStore) -> Result<Value> {
        Err(crate::Error::validation_invalid_argument(
            "runner_capabilities",
            "runner does not support sealed staging",
            None,
            None,
        ))
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn RunnerStagingProvider,
    noop: NoopProvider,
    register: pub fn register_runner_staging_provider,
    with: pub(crate) fn with_provider,
}

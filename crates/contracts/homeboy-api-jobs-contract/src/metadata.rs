//! Compatibility exports for metadata shared by the job model and runner
//! execution contract.

/// The canonical artifact pointer, re-exported so existing `api_jobs`-side call
/// sites remain unchanged.
pub use homeboy_runner_contract::JobArtifactMetadata;

/// Compatibility export for API-job consumers. Runner lifecycle metadata is
/// owned by the transport-neutral runner contract.
pub use homeboy_runner_contract::RunnerJobLifecycleMetadata;

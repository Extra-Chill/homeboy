//! Compatibility exports for metadata shared by the job model and runner
//! execution contract.

/// The canonical artifact pointer. Defined in `homeboy-lab-contract` because
/// this crate already depends on it and the Lab workload types need the same
/// shape -- defining it here and importing it there would be a cycle.
/// Re-exported so existing `api_jobs`-side call sites are unchanged.
pub use homeboy_lab_contract::lab::workload::JobArtifactMetadata;

/// Compatibility export for API-job consumers. Runner lifecycle metadata is
/// owned by the transport-neutral runner contract.
pub use homeboy_runner_contract::RunnerJobLifecycleMetadata;

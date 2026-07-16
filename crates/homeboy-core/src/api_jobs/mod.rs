mod persistence;
mod remote_runner;
mod runner_job_preparation;
mod store;
mod summary;
mod types;

pub use remote_runner::{
    JobArtifactMetadata, RemoteRunnerJobClaim, RemoteRunnerJobRequest, RemoteRunnerJobResult,
    RunnerJobLifecycleMetadata,
};
pub(crate) use runner_job_preparation::with_runner_job_preparation;
pub use runner_job_preparation::{
    register_runner_job_preparation_provider, RunnerJobPreparationProvider,
};
pub(crate) use store::LocalChildStartDiscriminator;
pub(crate) use store::LocalRunnerJob;
pub use store::{JobHandle, JobRunner, JobStore};
pub use summary::{active_runner_job_run_summary, active_runner_job_run_summary_if_durable};
pub use types::{
    ActiveRunnerJobRunSummary, ActiveRunnerJobSummary, DaemonActiveJobRecoveryDisposition,
    DaemonActiveJobRecoveryEvidence, DaemonLeaseJobDiagnostics, DaemonLinkedDurableRunState, Job,
    JobClaimMetadata, JobEvent, JobEventKind, JobStatus, LeaselessOrphanAffectedJob,
    LeaselessOrphanJobDiagnostics, RunnerJobLifecycleOwner, RunnerJobProjection, RunnerJobSource,
};

#[cfg(test)]
mod tests;

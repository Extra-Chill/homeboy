pub mod agent_task_terminal_recovery;
mod persistence;
mod remote_runner;
mod runner_job_preparation;
mod store;
mod summary;
// The pure job-model data types (Job/JobStatus/JobEvent + companions) were
// extracted to the homeboy-api-jobs-contract leaf crate. Re-exported as
// `api_jobs::types` so existing `crate::api_jobs::types::*` paths keep
// resolving. The job store / persistence / remote-runner behavior stays here.
use homeboy_api_jobs_contract::types;

pub use remote_runner::{
    JobArtifactMetadata, RemoteRunnerJobClaim, RemoteRunnerJobRequest, RemoteRunnerJobResult,
    RunnerJobLifecycleMetadata, RunnerJobProjectionCancelRequest,
};
pub(crate) use runner_job_preparation::with_runner_job_preparation;
pub use runner_job_preparation::{
    register_runner_job_preparation_provider, RunnerJobPreparationProvider,
};
pub(crate) use store::LocalChildStartDiscriminator;
pub(crate) use store::LocalRunnerJob;
pub use store::{JobHandle, JobRunner, JobStore, RecoveredTerminalJob};
pub use summary::{active_runner_job_run_summary, active_runner_job_run_summary_if_durable};
pub use types::{
    ActiveRunnerJobRunSummary, ActiveRunnerJobSummary, DaemonActiveJobRecoveryDisposition,
    DaemonActiveJobRecoveryEvidence, DaemonLeaseJobDiagnostics, DaemonLinkedDurableRunState, Job,
    JobClaimMetadata, JobEvent, JobEventKind, JobStatus, LeaselessOrphanAffectedJob,
    LeaselessOrphanJobDiagnostics, RunnerJobLifecycleOwner, RunnerJobLogSnapshot,
    RunnerJobProjection, RunnerJobSource,
};

#[cfg(test)]
mod tests;

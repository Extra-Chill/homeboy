//! Pure serializable job-model contract types.
//!
//! `Job`, `JobStatus`, `JobEvent`, and their runner/daemon companions describe
//! the shape of a homeboy API job as it crosses process boundaries between the
//! controller, daemon, and runner. These are behavior-free serde data types, so
//! this crate can depend on the canonical runner contract without pulling in
//! core. Established API-job imports for shared metadata remain re-exported.
//!
//! The job *store* (`api_jobs::store`, persistence, remote-runner dispatch,
//! provider hooks) stays in `homeboy-core`.

pub mod metadata;
pub mod types;

pub use metadata::{JobArtifactMetadata, RunnerJobLifecycleMetadata};
pub use types::{
    ActiveRunnerJobRunSummary, ActiveRunnerJobSummary, DaemonActiveJobRecoveryDisposition,
    DaemonActiveJobRecoveryEvidence, DaemonLeaseJobDiagnostics, DaemonLinkedDurableRunState, Job,
    JobClaimMetadata, JobEvent, JobEventKind, JobStatus, LeaselessOrphanAffectedJob,
    LeaselessOrphanJobDiagnostics, RunnerJobLogSnapshot, RunnerJobProjection, RunnerJobSource,
};

//! Pure serializable job-model contract types.
//!
//! `Job`, `JobStatus`, `JobEvent`, and their runner/daemon companions describe
//! the shape of a homeboy API job as it crosses process boundaries between the
//! controller, daemon, and runner. These are behavior-free serde data types
//! (plus the pure `JobArtifactMetadata` / `RunnerJobLifecycleMetadata`), so this
//! is a leaf crate other crates can depend on without pulling in core.
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
    LeaselessOrphanJobDiagnostics, RunnerJobLifecycleOwner, RunnerJobLogSnapshot,
    RunnerJobProjection, RunnerJobSource,
};

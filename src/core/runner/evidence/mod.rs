//! Runner evidence: artifact retrieval and mirroring of daemon/broker job
//! evidence into the local observation store.
//!
//! Submodules group the concerns that previously lived in a single
//! `evidence.rs` god-file:
//! - [`artifact`]: runner artifact path/token resolution and downloads.
//! - [`mirror`]: mirroring daemon and reverse-broker job runs/artifacts.
//! - [`conversion`]: translating remote payloads into observation records.

mod artifact;
mod conversion;
mod mirror;

#[cfg(test)]
mod tests;

pub(crate) use artifact::artifact_store_locator_from_runner_artifact_id;
pub use artifact::{
    download_remote_artifact, is_remote_runner_artifact_path, is_reportable_artifact_evidence_path,
    is_retrievable_runner_artifact, reportable_artifact_evidence_path, runner_artifact_store_token,
    RemoteArtifactDownload,
};
pub use mirror::{
    mirror_connected_runner_run, mirror_daemon_evidence, mirror_daemon_job_progress,
    mirror_reverse_broker_evidence, mirrored_runner_job_identity, refresh_mirrored_daemon_evidence,
    runner_job_log_snapshot, RunnerJobLogSnapshot,
};

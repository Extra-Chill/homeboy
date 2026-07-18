//! Runner-side implementation of core's `RunnerEvidenceProvider` hook.
//!
//! Core's `observation::runs_service` calls this contract to enrich run/artifact
//! lookups with live runner + daemon evidence, without depending on runner
//! behavior directly. This adapter delegates to the runner evidence functions
//! and maps runner types onto the slim core-facing types.

use std::path::PathBuf;

use serde_json::Value;

use homeboy_core::error::Result;
use homeboy_core::observation::runs_service::{
    RemoteArtifactDownloadInfo, RunnerConnectionInfo, RunnerEvidenceProvider, StaleRunnerJobInfo,
};
use homeboy_core::observation::RunRecord;

/// The runner layer's `RunnerEvidenceProvider`. Registered with core at startup.
pub struct RunnerEvidence;

impl RunnerEvidenceProvider for RunnerEvidence {
    fn mirror_connected_runner_run(&self, run_id: &str) -> Result<Option<RunRecord>> {
        super::mirror::mirror_connected_runner_run(run_id)
    }

    fn statuses(&self) -> Vec<RunnerConnectionInfo> {
        super::super::connection::statuses()
            .unwrap_or_default()
            .into_iter()
            .map(|report| RunnerConnectionInfo {
                runner_id: report.runner_id,
                connected: report.connected,
                active_jobs: report.active_jobs,
                stale_runner_jobs: report
                    .stale_runner_jobs
                    .into_iter()
                    .map(|job| StaleRunnerJobInfo {
                        durable_run_id: job.durable_run_id,
                        runner_id: job.runner_id,
                        job_id: job.job_id,
                        status: job.status.daemon_status_label().to_string(),
                        lifecycle_state: job.lifecycle_state,
                        stale_reason: job.stale_reason,
                        retryable: job.retryable,
                    })
                    .collect(),
            })
            .collect()
    }

    fn daemon_api_get(&self, runner_id: &str, path: &str) -> Result<Value> {
        super::super::execution::daemon_api_get(runner_id, path)
    }

    fn runner_artifact_content(
        &self,
        runner_id: &str,
        job_id: &str,
        artifact_id: &str,
    ) -> Result<Value> {
        super::super::connection::runner_artifact_content(runner_id, job_id, artifact_id)
    }

    fn runner_job_cancel(
        &self,
        runner_id: &str,
        job_id: &str,
    ) -> Result<(
        homeboy_core::api_jobs::Job,
        Vec<homeboy_core::api_jobs::JobEvent>,
    )> {
        super::super::execution::runner_job_cancel(runner_id, job_id)
    }

    fn runner_job_cancel_projection(
        &self,
        runner_id: &str,
        job_id: &str,
        durable_run_id: &str,
    ) -> Result<(
        homeboy_core::api_jobs::Job,
        Vec<homeboy_core::api_jobs::JobEvent>,
    )> {
        super::super::execution::runner_job_cancel_projection(runner_id, job_id, durable_run_id)
    }

    fn refresh_mirrored_daemon_evidence(&self, run_id: &str) -> Result<Option<Vec<RunRecord>>> {
        super::mirror::refresh_mirrored_daemon_evidence(run_id)
    }

    fn mirrored_runner_job_identity(&self, run: &RunRecord) -> Option<(String, String)> {
        super::mirror::mirrored_runner_job_identity(run)
    }

    fn download_remote_artifact(
        &self,
        path: &str,
        output: Option<PathBuf>,
    ) -> Result<RemoteArtifactDownloadInfo> {
        let download = super::download::download_remote_artifact(path, output)?;
        Ok(RemoteArtifactDownloadInfo {
            output_path: download.output_path,
            content_type: download.content_type,
            size_bytes: download.size_bytes,
            sha256: download.sha256,
            artifact_ref: download.artifact_ref,
        })
    }
}

/// Register the runner evidence provider with core. Called once at startup.
pub fn register() {
    homeboy_core::observation::runs_service::register_runner_evidence_provider(Box::new(
        RunnerEvidence,
    ));
}

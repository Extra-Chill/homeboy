use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{
    job_not_found, timestamp_ms, Job, JobEvent, JobEventKind, JobStatus, JobStore, StoredJob,
};
use crate::core::engine::command::CommandCaptureMetadata;
use crate::core::error::{Error, Result};
use crate::core::runner::{RunnerMutationArtifacts, RunnerResourceMetrics};
use crate::core::source_snapshot::SourceSnapshot;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobArtifactMetadata {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteRunnerJobRequest {
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default = "default_runner_exec_operation")]
    pub operation: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env_names: Vec<String>,
    #[serde(default)]
    pub capture_patch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<SourceSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl RemoteRunnerJobRequest {
    pub(crate) fn public_metadata(&self) -> Self {
        let mut public = self.clone();
        let secret_env_names = self
            .secret_env_names
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        for (name, value) in public.env.iter_mut() {
            if secret_env_names.contains(name.as_str()) {
                *value = "<redacted>".to_string();
            }
        }
        public
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRunnerJobClaim {
    pub job: Job,
    pub request: RemoteRunnerJobRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteRunnerJobResult {
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_artifacts: Option<RunnerMutationArtifacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_run_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<JobArtifactMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<JobArtifactMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RunnerResourceMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CommandCaptureMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredRemoteRunnerJob {
    #[serde(default, skip)]
    pub(super) execution_request: Option<RemoteRunnerJobRequest>,
    pub(super) request: RemoteRunnerJobRequest,
}

impl JobStore {
    pub(crate) fn submit_remote_runner_job(&self, request: RemoteRunnerJobRequest) -> Result<Job> {
        if request.runner_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                "remote runner job requires a runner id",
                None,
                None,
            ));
        }
        if request.command.is_empty() {
            return Err(Error::validation_invalid_argument(
                "command",
                "remote runner job requires a command",
                None,
                None,
            ));
        }

        let now = timestamp_ms();
        let job = Job {
            id: Uuid::new_v4(),
            operation: request.operation.clone(),
            status: JobStatus::Queued,
            created_at_ms: now,
            updated_at_ms: now,
            started_at_ms: None,
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: request.source_snapshot.clone(),
            stale_reason: None,
            target_runner_id: Some(request.runner_id.clone()),
            target_project_id: request.project_id.clone(),
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
        };

        let public_request = request.public_metadata();
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        inner.jobs.insert(
            job.id,
            StoredJob {
                job: job.clone(),
                events: Vec::new(),
                remote_runner: Some(StoredRemoteRunnerJob {
                    execution_request: Some(request),
                    request: public_request,
                }),
            },
        );
        drop(inner);

        self.append_status_event(job.id, JobStatus::Queued, "remote runner job queued")?;
        self.get(job.id)
    }

    pub(crate) fn claim_remote_runner_job(
        &self,
        runner_id: &str,
        project_id: Option<&str>,
        lease_ms: u64,
        concurrency_limit: Option<usize>,
    ) -> Result<Option<RemoteRunnerJobClaim>> {
        if runner_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                "remote runner claim requires a runner id",
                None,
                None,
            ));
        }
        if concurrency_limit == Some(0) {
            return Err(Error::validation_invalid_argument(
                "concurrency_limit",
                "remote runner claim concurrency_limit must be greater than zero",
                Some(runner_id.to_string()),
                None,
            ));
        }

        let now = timestamp_ms();
        let lease_ms = lease_ms.max(1);
        let mut claimed: Option<(Uuid, RemoteRunnerJobRequest)> = None;
        {
            let mut inner = self.inner.lock().expect("job store mutex poisoned");
            if let Some(limit) = concurrency_limit {
                let running = inner
                    .jobs
                    .values()
                    .filter(|stored| {
                        stored.remote_runner.is_some()
                            && stored.job.status == JobStatus::Running
                            && stored.job.claimed_by_runner_id.as_deref() == Some(runner_id)
                    })
                    .count();
                if running >= limit {
                    return Ok(None);
                }
            }
            let mut candidates: Vec<_> = inner
                .jobs
                .values_mut()
                .filter(|stored| {
                    stored.remote_runner.is_some()
                        && stored.job.status == JobStatus::Queued
                        && stored.job.target_runner_id.as_deref() == Some(runner_id)
                        && project_matches(stored.job.target_project_id.as_deref(), project_id)
                })
                .collect();
            candidates.sort_by_key(|stored| {
                (
                    queued_event_sequence(stored),
                    stored.job.created_at_ms,
                    stored.job.id,
                )
            });
            if let Some(stored) = candidates.into_iter().next() {
                stored.job.status = JobStatus::Running;
                stored.job.updated_at_ms = now;
                stored.job.started_at_ms = Some(now);
                stored.job.claim_id = Some(Uuid::new_v4().to_string());
                stored.job.claimed_by_runner_id = Some(runner_id.to_string());
                stored.job.claimed_at_ms = Some(now);
                stored.job.claim_expires_at_ms = Some(now.saturating_add(lease_ms));
                let remote_runner = stored
                    .remote_runner
                    .as_ref()
                    .expect("filtered remote runner job has request");
                let request = remote_runner
                    .execution_request
                    .as_ref()
                    .unwrap_or(&remote_runner.request)
                    .clone();
                claimed = Some((stored.job.id, request));
            }
        }

        let Some((job_id, request)) = claimed else {
            return Ok(None);
        };

        self.persist()?;
        self.append_status_event(job_id, JobStatus::Running, "remote runner job claimed")?;
        Ok(Some(RemoteRunnerJobClaim {
            job: self.get(job_id)?,
            request,
        }))
    }

    pub(crate) fn append_remote_runner_event(
        &self,
        job_id: Uuid,
        runner_id: &str,
        claim_id: &str,
        kind: JobEventKind,
        message: Option<String>,
        data: Option<Value>,
    ) -> Result<JobEvent> {
        self.ensure_remote_runner_claim(job_id, runner_id, claim_id)?;
        self.append_event(job_id, kind, message, data)
    }

    pub(crate) fn finish_remote_runner_job(
        &self,
        job_id: Uuid,
        runner_id: &str,
        claim_id: &str,
        result: RemoteRunnerJobResult,
    ) -> Result<Job> {
        self.ensure_remote_runner_claim(job_id, runner_id, claim_id)?;
        let status = if result.exit_code == 0 {
            JobStatus::Succeeded
        } else {
            JobStatus::Failed
        };
        self.ensure_transition(job_id, status)?;

        let result_data = serde_json::to_value(&result).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize remote runner job result".to_string()),
            )
        })?;
        self.append_event(job_id, JobEventKind::Result, None, Some(result_data))?;
        if result.exit_code != 0 {
            self.append_event(
                job_id,
                JobEventKind::Error,
                Some(format!(
                    "remote runner job exited with code {}",
                    result.exit_code
                )),
                Some(json_exit_code(result.exit_code)),
            )?;
        }

        {
            let mut inner = self.inner.lock().expect("job store mutex poisoned");
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            stored.job.artifacts = result
                .artifacts
                .into_iter()
                .map(|mut artifact| {
                    artifact.content_base64 = None;
                    artifact
                })
                .collect();
        }
        self.persist()?;

        self.transition(
            job_id,
            status,
            if status == JobStatus::Succeeded {
                "remote runner job succeeded".to_string()
            } else {
                format!(
                    "remote runner job failed with exit code {}",
                    result.exit_code
                )
            },
        )
    }

    pub(crate) fn renew_remote_runner_claim(
        &self,
        job_id: Uuid,
        runner_id: &str,
        claim_id: &str,
        lease_ms: u64,
    ) -> Result<Job> {
        self.ensure_remote_runner_claim(job_id, runner_id, claim_id)?;
        let now = timestamp_ms();
        {
            let mut inner = self.inner.lock().expect("job store mutex poisoned");
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            stored.job.updated_at_ms = now;
            stored.job.claim_expires_at_ms = Some(now.saturating_add(lease_ms.max(1)));
        }
        self.persist()?;
        self.get(job_id)
    }

    pub(crate) fn cancel_remote_runner_job(
        &self,
        job_id: Uuid,
        reason: impl Into<String>,
    ) -> Result<Job> {
        {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            let stored = inner
                .jobs
                .get(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            if stored.remote_runner.is_none() {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "job is not a remote runner job",
                    Some(job_id.to_string()),
                    None,
                ));
            }
        }
        self.cancel(job_id, reason)
    }

    pub(crate) fn reconcile_expired_remote_runner_claims(&self, now_ms: u64) -> Result<Vec<Job>> {
        let expired_ids = {
            let inner = self.inner.lock().expect("job store mutex poisoned");
            inner
                .jobs
                .values()
                .filter(|stored| {
                    stored.remote_runner.is_some()
                        && stored.job.status == JobStatus::Running
                        && stored
                            .job
                            .claim_expires_at_ms
                            .is_some_and(|expires_at| expires_at <= now_ms)
                })
                .map(|stored| stored.job.id)
                .collect::<Vec<_>>()
        };

        let mut reconciled = Vec::new();
        for job_id in expired_ids {
            reconciled.push(self.fail(job_id, "remote runner claim expired")?);
        }
        Ok(reconciled)
    }

    fn ensure_remote_runner_claim(
        &self,
        job_id: Uuid,
        runner_id: &str,
        claim_id: &str,
    ) -> Result<()> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        if stored.remote_runner.is_none() {
            return Err(Error::validation_invalid_argument(
                "job_id",
                "job is not a remote runner job",
                Some(job_id.to_string()),
                None,
            ));
        }
        if stored.job.claimed_by_runner_id.as_deref() != Some(runner_id) {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                "remote runner job is not claimed by this runner",
                Some(runner_id.to_string()),
                None,
            ));
        }
        if claim_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "claim_id",
                "remote runner event or result requires a claim id",
                None,
                None,
            ));
        }
        if stored.job.claim_id.as_deref() != Some(claim_id) {
            return Err(Error::validation_invalid_argument(
                "claim_id",
                "remote runner job is not claimed by this claim id",
                Some(claim_id.to_string()),
                None,
            ));
        }
        if stored.job.status != JobStatus::Running {
            return Err(Error::validation_invalid_argument(
                "status",
                "remote runner job must be running before events or results are returned",
                Some(job_id.to_string()),
                None,
            ));
        }
        let now = timestamp_ms();
        if match stored.job.claim_expires_at_ms {
            Some(expires_at) => expires_at <= now,
            None => true,
        } {
            return Err(Error::validation_invalid_argument(
                "claim_expires_at_ms",
                "remote runner claim has expired",
                Some(job_id.to_string()),
                None,
            ));
        }

        Ok(())
    }
}

fn default_runner_exec_operation() -> String {
    "runner.exec".to_string()
}

fn project_matches(job_project_id: Option<&str>, claim_project_id: Option<&str>) -> bool {
    claim_project_id.is_none() || job_project_id == claim_project_id
}

fn queued_event_sequence(stored: &StoredJob) -> u64 {
    stored
        .events
        .iter()
        .find(|event| {
            event.kind == JobEventKind::Status
                && event
                    .data
                    .as_ref()
                    .is_some_and(|data| data["status"] == serde_json::json!("queued"))
        })
        .map(|event| event.sequence)
        .unwrap_or(u64::MAX)
}

fn json_exit_code(exit_code: i32) -> Value {
    serde_json::json!({ "exit_code": exit_code })
}

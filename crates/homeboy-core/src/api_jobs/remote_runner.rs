use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::persistence::{job_not_found, timestamp_ms};
use super::store::{JobStore, StoredJob};
use super::types::{Job, JobEvent, JobEventKind, JobStatus};
use crate::engine::command::CommandCaptureMetadata;
use crate::env_materialization_plan::EnvMaterializationPlan;
use crate::error::{Error, Result};
use crate::lab_contract::RunnerWorkload;
use crate::runner_execution_envelope::{
    PathMaterializationPlan, RunnerExecutionDispatch, RunnerExecutionEnvelope,
    RunnerExecutionLifecycle, RunnerExecutionMutationPolicy, RunnerExecutionResultRefs,
};
use crate::secret_env_plan::SecretEnvPlan;
use crate::source_snapshot::SourceSnapshot;
use homeboy_lab_runner_contract::{RunnerMutationArtifacts, RunnerResourceMetrics};

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
pub struct RunnerJobLifecycleMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_child_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_cell_count: Option<u64>,
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
    pub secret_env_plan: SecretEnvPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_materialization: Option<EnvMaterializationPlan>,
    #[serde(default)]
    pub capture_patch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<SourceSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_materialization_plan: Option<PathMaterializationPlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_workload: Option<RunnerWorkload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<RunnerJobLifecycleMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl RemoteRunnerJobRequest {
    pub(crate) fn normalize(&mut self) -> SecretEnvPlan {
        let mut base_plan = self
            .runner_workload
            .as_ref()
            .map(|workload| workload.required_secrets.secret_env_plan.clone())
            .filter(|plan| *plan != SecretEnvPlan::default());
        if self.secret_env_plan != SecretEnvPlan::default() {
            if let Some(plan) = base_plan.as_mut() {
                plan.merge_from(self.secret_env_plan.clone());
            } else {
                base_plan = Some(self.secret_env_plan.clone());
            }
        }

        let secret_env_plan = super::with_runner_job_preparation(|p| {
            p.runner_exec_secret_env_plan(
                &self.command,
                None,
                &self.secret_env_names,
                &self.env,
                base_plan,
            )
        });
        self.secret_env_names = secret_env_plan.secret_env_names();
        self.secret_env_plan = secret_env_plan.clone();
        secret_env_plan
    }

    pub fn execution_envelope(&self) -> RunnerExecutionEnvelope {
        let mut request = self.clone();
        let secret_env_plan = request.normalize();
        let envelope_id = request
            .runner_workload
            .as_ref()
            .map(|workload| workload.workload_id.clone())
            .or_else(|| {
                request
                    .lifecycle
                    .as_ref()
                    .and_then(|lifecycle| non_empty_string(lifecycle.durable_run_id.as_deref()))
            })
            .or_else(|| {
                request
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("run_id"))
                    .and_then(|run_id| non_empty_string(run_id.as_str()))
            })
            .unwrap_or_else(|| {
                format!("remote-runner:{}:{}", request.runner_id, request.operation)
            });
        let mut envelope = request
            .runner_workload
            .clone()
            .map(RunnerExecutionEnvelope::from_runner_workload)
            .unwrap_or_else(|| {
                RunnerExecutionEnvelope::planned(&envelope_id, "remote_runner_job_request")
            });

        envelope.envelope_id = envelope_id.clone();
        envelope.source.kind = "remote_runner_job_request".to_string();
        envelope.source.ref_id = Some(envelope_id);
        envelope.secret_env = Some(secret_env_plan);
        envelope.dispatch = Some(RunnerExecutionDispatch {
            runner_id: request.runner_id,
            project_id: request.project_id,
            operation: request.operation,
            command: request.command,
            cwd: request.cwd,
            env: request
                .env
                .into_iter()
                .filter(|(name, _)| !homeboy_lab_runner_contract::is_internal_control_env(name))
                .collect(),
            source_snapshot: request.source_snapshot,
            require_paths: request.require_paths,
        });
        if let Some(path_materialization_plan) = request.path_materialization_plan {
            envelope.metadata = merge_metadata_value(
                request.metadata.take().unwrap_or(Value::Null),
                "path_materialization_plan",
                serde_json::to_value(path_materialization_plan).unwrap_or(Value::Null),
            );
        } else {
            envelope.metadata = request.metadata.unwrap_or(Value::Null);
        }
        envelope.lifecycle = request.lifecycle.map(RunnerExecutionLifecycle::from);
        envelope.mutation_policy = RunnerExecutionMutationPolicy {
            capture_patch: request.capture_patch,
            ..envelope.mutation_policy.clone()
        };
        if envelope.result_refs.run_id.is_none() {
            envelope.result_refs.run_id = envelope
                .lifecycle
                .as_ref()
                .and_then(|lifecycle| non_empty_string(lifecycle.durable_run_id.as_deref()))
                .or_else(|| metadata_run_id(&envelope.metadata));
        }
        if envelope.result_refs.artifacts.is_empty() {
            if let Some(workload) = envelope.runner_workload.as_ref() {
                envelope.result_refs = RunnerExecutionResultRefs {
                    artifacts: workload.result_refs.artifacts.clone(),
                    ..envelope.result_refs.clone()
                };
            }
        }
        envelope
    }

    pub(crate) fn public_metadata(&self) -> Self {
        let mut public = self.clone();
        public
            .env
            .retain(|name, _| !homeboy_lab_runner_contract::is_internal_control_env(name));
        let mut secret_env_name_values = self.secret_env_plan.secret_env_names();
        secret_env_name_values.extend(self.secret_env_names.clone());
        let secret_env_names = secret_env_name_values
            .iter()
            .map(|name| name.as_str())
            .collect::<HashSet<_>>();
        for (name, value) in public.env.iter_mut() {
            if secret_env_names.contains(name.as_str()) {
                *value = "<redacted>".to_string();
            }
        }
        public
    }

    pub(crate) fn run_ref_metadata(&self) -> Option<Value> {
        let durable_run_id = self
            .lifecycle
            .as_ref()
            .and_then(|lifecycle| non_empty_string(lifecycle.durable_run_id.as_deref()))
            .or_else(|| self.metadata.as_ref().and_then(metadata_run_id));
        let agent_task_run_id = self
            .runner_workload
            .as_ref()
            .and_then(|workload| workload.agent_task.as_ref())
            .and_then(|agent_task| non_empty_string(Some(agent_task.run_id.as_str())))
            .or_else(|| {
                self.metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("agent_task_run_id"))
                    .and_then(|run_id| non_empty_string(run_id.as_str()))
            })
            .or_else(|| durable_run_id.clone());

        if durable_run_id.is_none() && agent_task_run_id.is_none() {
            return None;
        }

        Some(serde_json::json!({
            "durable_run_id": durable_run_id,
            "agent_task_run_id": agent_task_run_id,
        }))
    }
}

impl From<RunnerJobLifecycleMetadata> for RunnerExecutionLifecycle {
    fn from(lifecycle: RunnerJobLifecycleMetadata) -> Self {
        Self {
            source: lifecycle.source,
            kind: lifecycle.kind,
            durable_run_id: lifecycle.durable_run_id,
            active_child_count: lifecycle.active_child_count,
            active_cell_count: lifecycle.active_cell_count,
        }
    }
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn metadata_run_id(metadata: &Value) -> Option<String> {
    ["durable_run_id", "run_id", "record_run_id"]
        .iter()
        .find_map(|key| metadata.get(*key))
        .and_then(|run_id| non_empty_string(run_id.as_str()))
}

fn merge_metadata_value(mut metadata: Value, key: &str, value: Value) -> Value {
    if !metadata.is_object() {
        metadata = Value::Object(Default::default());
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(key.to_string(), value);
    }
    metadata
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
    pub fn submit_remote_runner_job(&self, mut request: RemoteRunnerJobRequest) -> Result<Job> {
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
        let secret_env_plan = request.normalize();
        super::with_runner_job_preparation(|p| {
            p.validate_runner_workload_dispatch(
                request.runner_workload.as_ref(),
                &request.runner_id,
                request.cwd.as_deref(),
                &request.command,
                &secret_env_plan,
                request.capture_patch,
            )
        })?;

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
            path_materialization_plan: request.path_materialization_plan.clone(),
            stale_reason: None,
            daemon_lease_id: self.daemon_lease_id.clone(),
            target_runner_id: Some(request.runner_id.clone()),
            target_project_id: request.project_id.clone(),
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };

        let public_request = request.public_metadata();
        let run_ref_metadata = public_request.run_ref_metadata();
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
                local_runner: None,
                local_child: None,
            },
        );
        drop(inner);

        if let Some(metadata) = run_ref_metadata {
            self.append_status_event_with_data(
                job.id,
                JobStatus::Queued,
                "remote runner job queued",
                metadata,
            )?;
        } else {
            self.append_status_event(job.id, JobStatus::Queued, "remote runner job queued")?;
        }
        self.get(job.id)
    }

    pub fn claim_remote_runner_job(
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

    pub fn append_remote_runner_event(
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

    pub fn finish_remote_runner_job(
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
            // A runner attempting to act on a job claimed by a *different*
            // runner is an authorization violation, not a plain validation
            // error: it must surface as a `broker.auth_denied` (401) so a
            // second runner with its own valid token cannot finish/heartbeat
            // another runner's job.
            return Err(Error::broker_auth_denied(
                "remote runner job is not claimed by this runner",
                Some(runner_id.to_string()),
                Vec::new(),
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

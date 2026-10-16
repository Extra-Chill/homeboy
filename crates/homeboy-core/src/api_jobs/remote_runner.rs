use std::sync::atomic::Ordering;

use homeboy_engine_primitives::content_hash;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::persistence::{
    apply_event_retention, job_not_found, lookup_tombstone, timestamp_ms, validate_transition,
    ReplayTombstoneKind,
};
use super::store::{JobStore, LocalRunnerJob, RemoteRunnerSubmission, StoredJob};
use super::types::{Job, JobEvent, JobEventKind, JobStatus};
use crate::engine::command::CommandCaptureMetadata;
use crate::env_materialization_plan::EnvMaterializationPlan;
use crate::error::{Error, ErrorCode, Result};
use crate::lab_contract::LabRunnerWorkload;
use crate::runner_execution_envelope::{
    PathMaterializationPlan, RunnerExecutionDispatch, RunnerExecutionEnvelope,
    RunnerExecutionMutationPolicy, RunnerExecutionResultRefs,
};
use crate::secret_env_plan::SecretEnvPlan;
use crate::source_snapshot::SourceSnapshot;
use homeboy_lab_contract::lab::execution_envelope::{
    lab_runner_workload_from_execution_envelope, runner_execution_envelope_from_workload,
};
use homeboy_runner_contract::{
    is_internal_control_env, RunnerApiSubmitRequest, RunnerMutationArtifacts,
    RunnerResourceMetrics, RUNNER_API_SUBMIT_REQUEST_SCHEMA, RUNNER_API_V1,
};

/// Broker metadata is durable queue input. Keep command-file payloads bounded
/// before they can be persisted or decoded by a worker.
const MAX_COMMAND_ASSET_COUNT: usize = 16;
const MAX_COMMAND_ASSET_BASE64_BYTES: usize = 1_400_000;
const MAX_COMMAND_ASSETS_BASE64_BYTES: usize = 4_200_000;

pub use homeboy_runner_contract::{JobArtifactMetadata, RunnerJobLifecycleMetadata};

/// Caller-owned identity required to cancel one daemon-local runner projection.
/// This is deliberately distinct from the operator-facing job cancellation API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobProjectionCancelRequest {
    pub expected_runner_id: String,
    pub expected_durable_run_id: String,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_env_providers: Vec<String>,
    #[serde(
        rename = "runner_workload",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub lab_runner_workload: Option<LabRunnerWorkload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<RunnerJobLifecycleMetadata>,
    /// Exact reconciliation authority required to mutate this workspace. It is
    /// absent for ordinary runner jobs, preserving the original wire contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_claim_binding: Option<crate::workspace_claim::WorkspaceClaimBinding>,
    /// Exact active-work authority for portable workspace jobs. Unlike a
    /// reconciliation fence this remains live while the worker executes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_owner_lease: Option<crate::workspace_claim::WorkspaceOwnerLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl RemoteRunnerJobRequest {
    /// A caller-owned identity makes a retried POST safe across a lost response.
    /// It deliberately lives in metadata so older clients can continue submitting
    /// anonymous jobs while durable handoffs opt into replayable ownership.
    pub fn submission_key(&self) -> Option<&str> {
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.get("submission_key"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
    }

    pub(crate) fn normalize(&mut self) -> SecretEnvPlan {
        let mut base_plan = self
            .lab_runner_workload
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

    /// The Runner API submission a fixture request describes. A fixture
    /// without a caller-owned key receives a unique one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn into_runner_api_fixture(self) -> RunnerApiSubmitRequest {
        let submission_key = self
            .submission_key()
            .map(str::to_string)
            .unwrap_or_else(|| format!("test:runner-api:{}", Uuid::new_v4()));
        RunnerApiSubmitRequest {
            schema: RUNNER_API_SUBMIT_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            submission_key,
            envelope: self.execution_envelope(),
            workspace_claim_binding: self.workspace_claim_binding,
            workspace_owner_lease: self.workspace_owner_lease,
        }
    }

    pub fn execution_envelope(&self) -> RunnerExecutionEnvelope {
        let mut request = self.clone();
        let secret_env_plan = request.normalize();
        let envelope_id = request
            .lab_runner_workload
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
            .lab_runner_workload
            .clone()
            .map(runner_execution_envelope_from_workload)
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
                .filter(|(name, _)| !is_internal_control_env(name))
                .collect(),
            source_snapshot: request.source_snapshot,
            require_paths: request.require_paths,
            extension_env_providers: request.extension_env_providers,
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
        envelope.lifecycle = request.lifecycle.clone();
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
            if let Some(workload) = request.lab_runner_workload.as_ref() {
                envelope.result_refs = RunnerExecutionResultRefs {
                    artifacts: workload.result_refs.artifacts.clone(),
                    ..envelope.result_refs.clone()
                };
            }
        }
        envelope
    }
}

fn validate_command_assets(metadata: Option<&Value>) -> Result<()> {
    let Some(assets) = metadata
        .and_then(|metadata| metadata.get("command_assets"))
        .and_then(|assets| assets.get("assets"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    if assets.len() > MAX_COMMAND_ASSET_COUNT {
        return Err(Error::validation_invalid_argument(
            "metadata.command_assets",
            "remote runner job has too many command assets",
            None,
            None,
        ));
    }
    let mut total = 0usize;
    for asset in assets {
        let encoded = asset
            .get("content_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "metadata.command_assets",
                    "remote runner command asset is missing base64 content",
                    None,
                    None,
                )
            })?;
        if encoded.len() > MAX_COMMAND_ASSET_BASE64_BYTES {
            return Err(Error::validation_invalid_argument(
                "metadata.command_assets",
                "remote runner command asset exceeds the size limit",
                None,
                None,
            ));
        }
        total = total.saturating_add(encoded.len());
    }
    if total > MAX_COMMAND_ASSETS_BASE64_BYTES {
        return Err(Error::validation_invalid_argument(
            "metadata.command_assets",
            "remote runner command assets exceed the aggregate size limit",
            None,
            None,
        ));
    }
    Ok(())
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

pub(crate) fn canonical_run_ref_metadata(
    lifecycle: Option<&RunnerJobLifecycleMetadata>,
    workload: Option<&LabRunnerWorkload>,
    metadata: Option<&Value>,
) -> Option<Value> {
    let durable_run_id = lifecycle
        .and_then(|lifecycle| non_empty_string(lifecycle.durable_run_id.as_deref()))
        .or_else(|| metadata.and_then(metadata_run_id));
    let agent_task_run_id = workload
        .and_then(|workload| workload.agent_task.as_ref())
        .and_then(|agent_task| non_empty_string(Some(agent_task.run_id.as_str())))
        .or_else(|| {
            metadata
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

fn envelope_run_ref_metadata(
    envelope: &RunnerExecutionEnvelope,
    workload: Option<&LabRunnerWorkload>,
) -> Option<Value> {
    canonical_run_ref_metadata(
        envelope.lifecycle.as_ref(),
        workload,
        Some(&envelope.metadata),
    )
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
    /// Durable terminal observations retained by the runner. Each record
    /// carries its complete artifact metadata so disconnected reconciliation
    /// never needs a live runner endpoint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_run_details: Vec<RemoteRunnerObservationRunDetail>,
    /// Set by the broker when a legacy worker declared run IDs without the
    /// typed terminal records needed for disconnected reconciliation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub observation_run_details_compatibility_degraded: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<JobArtifactMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<JobArtifactMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RunnerResourceMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CommandCaptureMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteRunnerObservationRunDetail {
    pub schema: String,
    pub run: crate::observation::RunRecord,
    #[serde(default)]
    pub artifacts: Vec<crate::observation::ArtifactRecord>,
}

impl RemoteRunnerObservationRunDetail {
    pub const SCHEMA_V1: &'static str = "homeboy/remote-runner-observation-run-detail/v1";

    pub fn v1(
        run: crate::observation::RunRecord,
        artifacts: Vec<crate::observation::ArtifactRecord>,
    ) -> Self {
        Self {
            schema: Self::SCHEMA_V1.to_string(),
            run,
            artifacts,
        }
    }
}

/// The only persisted shape of a remote runner job. Unknown fields are
/// refused, so a record written in an older shape (for example the
/// request-only representation) fails the store load with a typed JSON error
/// naming the field instead of being reinterpreted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredRemoteRunnerJob {
    pub(super) envelope: RunnerExecutionEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) workspace_claim_binding: Option<crate::workspace_claim::WorkspaceClaimBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) workspace_owner_lease: Option<crate::workspace_claim::WorkspaceOwnerLease>,
    /// Execution context recorded when a worker claimed the job. Records that
    /// carry one are never selected or adopted for direct execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) execution_context_id: Option<String>,
    /// Execution receipt recorded when a worker consumed a claimed job. Records
    /// that carry one are never selected or adopted for direct execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) execution_receipt: Option<RemoteRunnerExecutionReceipt>,
    /// Durable direction fence set before runner-side staged-source work starts.
    /// A selected job remains queued and retryable until direct adoption.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) direct_execution_selected: bool,
}

struct RemoteRunnerAdmission {
    operation: String,
    runner_id: String,
    project_id: Option<String>,
    source_snapshot: Option<SourceSnapshot>,
    path_materialization_plan: Option<PathMaterializationPlan>,
    run_ref_metadata: Option<Value>,
    submission_key: Option<String>,
    submission_fingerprint: Option<String>,
    stored_remote_runner: StoredRemoteRunnerJob,
}

impl StoredRemoteRunnerJob {
    pub(super) fn workspace_owner_lease(
        &self,
    ) -> Option<crate::workspace_claim::WorkspaceOwnerLease> {
        self.workspace_owner_lease.clone()
    }

    pub(super) fn workspace_claim_binding(
        &self,
    ) -> Option<crate::workspace_claim::WorkspaceClaimBinding> {
        self.workspace_claim_binding.clone()
    }
}

fn redacted_envelope_for_durable_replay(
    mut envelope: RunnerExecutionEnvelope,
) -> RunnerExecutionEnvelope {
    let secret_env_names = envelope
        .secret_env
        .as_ref()
        .map(SecretEnvPlan::secret_env_names)
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    if let Some(dispatch) = envelope.dispatch.as_mut() {
        dispatch
            .env
            .retain(|name, _| !is_internal_control_env(name));
        for (name, value) in &mut dispatch.env {
            if secret_env_names.contains(name) {
                *value = "<redacted>".to_string();
            }
        }
    }
    envelope
}

pub fn runner_api_submission_payload_fingerprint(
    submission: &RunnerApiSubmitRequest,
) -> Result<String> {
    let envelope = redacted_envelope_for_durable_replay(submission.envelope.clone());
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": "homeboy/runner-api-submission-payload/v1",
        "envelope": envelope,
        "workspace_claim_binding": submission.workspace_claim_binding,
        "workspace_owner_lease": submission.workspace_owner_lease,
    }))
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("fingerprint runner envelope submission".to_string()),
        )
    })?;
    Ok(format!("sha256:{}", content_hash::sha256_hex(&bytes)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RemoteRunnerExecutionReceipt {
    pub(super) context_id: String,
    pub(super) consumed_at_ms: u64,
}

/// Only the sealed staging pipeline may convert a remote runner submission
/// into a direct daemon child.
fn is_runner_staged_execution(stored: &StoredJob) -> bool {
    stored.job.operation == "runner_staged_execution"
        && stored.remote_runner.as_ref().is_some_and(|remote| {
            remote
                .envelope
                .metadata
                .get("staged_source_artifact")
                .is_some()
                && remote
                    .envelope
                    .metadata
                    .get("staged_workspace_materialization")
                    .is_some()
        })
}

impl JobStore {
    /// Return the immutable, redacted envelope of one still-queued staged job.
    /// This is intentionally unavailable after direct adoption or terminalization
    /// so callers cannot treat historical staged input as new admission work.
    pub fn queued_staged_execution(
        &self,
        job_id: Uuid,
        runner_id: &str,
    ) -> Result<Option<RunnerExecutionEnvelope>> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        if stored.job.target_runner_id.as_deref() != Some(runner_id) {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                "staged execution belongs to a different runner",
                Some(job_id.to_string()),
                None,
            ));
        }
        Ok(
            (stored.job.status == JobStatus::Queued && is_runner_staged_execution(stored))
                .then(|| {
                    stored
                        .remote_runner
                        .as_ref()
                        .map(|remote| remote.envelope.clone())
                })
                .flatten(),
        )
    }

    /// Read the sealed staged envelope for either its queued staged record or
    /// a previously adopted direct record. Adoption retains this provenance so
    /// a replacement daemon can replay the exact UUID.
    pub fn staged_direct_execution_envelope(
        &self,
        job_id: Uuid,
        runner_id: &str,
    ) -> Result<Option<RunnerExecutionEnvelope>> {
        let inner = self.inner.lock().expect("job store mutex poisoned");
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        if stored.job.target_runner_id.as_deref() != Some(runner_id) {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                "staged execution belongs to a different runner",
                Some(job_id.to_string()),
                None,
            ));
        }
        if is_runner_staged_execution(stored) {
            return Ok(stored
                .remote_runner
                .as_ref()
                .map(|remote| remote.envelope.clone()));
        }
        Ok(stored.events.iter().rev().find_map(|event| {
            let data = event.data.as_ref()?;
            (data.get("phase").and_then(Value::as_str) == Some("staged_execution_adopted"))
                .then(|| data.get("staged_execution_envelope"))?
                .cloned()
                .and_then(|envelope| serde_json::from_value(envelope).ok())
        }))
    }

    /// Select direct execution before any runner-side source preparation. This
    /// durable direction fence runs in one transaction, so concurrent direct
    /// replays of a staged job converge on one execution.
    pub fn select_staged_direct_execution(
        &self,
        job_id: Uuid,
        runner_id: &str,
    ) -> Result<Option<RunnerExecutionEnvelope>> {
        self.durable_transaction(|inner| {
            let stored = inner.jobs.get_mut(&job_id).ok_or_else(|| job_not_found(job_id))?;
            if stored.job.target_runner_id.as_deref() != Some(runner_id) {
                return Err(Error::validation_invalid_argument(
                    "runner_id",
                    "staged execution belongs to a different runner",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if stored.local_runner.is_some() || stored.job.status.is_terminal() {
                return Ok(None);
            }
            if stored.job.status != JobStatus::Queued {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "staged execution is not queued",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if !is_runner_staged_execution(stored) {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "job is not a runner-staged execution",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            let (envelope, selected_now) = {
                let remote = stored
                    .remote_runner
                    .as_mut()
                    .expect("staged job has remote record");
                if remote.execution_context_id.is_some()
                    || remote.execution_receipt.is_some()
                    || remote.workspace_claim_binding().is_some()
                    || remote.workspace_owner_lease().is_some()
                {
                    return Err(Error::validation_invalid_argument(
                        "job_id",
                        "staged execution carries workspace authority and cannot be selected directly",
                        Some(job_id.to_string()),
                        None,
                    ));
                }
                let selected_now = !remote.direct_execution_selected;
                remote.direct_execution_selected = true;
                (remote.envelope.clone(), selected_now)
            };
            if selected_now {
                stored.job.updated_at_ms = timestamp_ms();
                Self::append_event_already_locked(
                    self,
                    inner,
                    job_id,
                    JobEventKind::Progress,
                    Some("staged direct execution selected".to_string()),
                    Some(serde_json::json!({ "phase": "staged_direct_execution_selected" })),
                )?;
            }
            Ok(Some(envelope))
        })
    }

    /// Atomically turn one exact queued staged submission into the existing
    /// daemon-local execution lifecycle. The submission index and event history
    /// remain attached to the same UUID; clearing `remote_runner` makes the
    /// adoption final.
    pub(crate) fn adopt_queued_staged_execution_for_local_runner(
        &self,
        job_id: Uuid,
        expected_envelope: &RunnerExecutionEnvelope,
        local_runner: LocalRunnerJob,
        source_snapshot: Option<SourceSnapshot>,
        metadata: Value,
        path_materialization_plan: Option<PathMaterializationPlan>,
    ) -> Result<(Job, bool)> {
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            if stored.local_runner.is_some() {
                return Ok((stored.job.clone(), false));
            }
            if stored.job.status.is_terminal() {
                return Ok((stored.job.clone(), false));
            }
            if stored.job.status != JobStatus::Queued {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "staged execution is not queued",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if stored.job.target_runner_id.as_deref() != Some(local_runner.runner_id.as_str()) {
                return Err(Error::validation_invalid_argument(
                    "runner_id",
                    "staged execution belongs to a different runner",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            let remote = stored.remote_runner.as_ref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "job_id",
                    "job is not a queued staged execution",
                    Some(job_id.to_string()),
                    None,
                )
            })?;
            if !is_runner_staged_execution(stored) {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "job is not a runner-staged execution",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if !remote.direct_execution_selected {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "staged execution was not selected for direct execution",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if &remote.envelope != expected_envelope {
                return Err(Error::validation_invalid_argument(
                    "expected_envelope",
                    "staged execution envelope does not match the durable submission",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if remote.execution_context_id.is_some()
                || remote.execution_receipt.is_some()
                || remote.workspace_claim_binding().is_some()
                || remote.workspace_owner_lease().is_some()
            {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "staged execution carries workspace authority and cannot be directly adopted",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            stored.local_runner = Some(local_runner);
            stored.remote_runner = None;
            stored.job.operation = "runner.exec".to_string();
            // This daemon generation owns the pre-worker handoff. A later
            // generation may recover only if no child reservation was recorded.
            stored.job.daemon_lease_id = self.daemon_lease_id.clone();
            stored.job.source_snapshot = source_snapshot;
            stored.job.path_materialization_plan = path_materialization_plan;
            stored.job.updated_at_ms = timestamp_ms();
            Self::append_event_already_locked(
                self,
                inner,
                job_id,
                JobEventKind::Progress,
                Some("queued staged execution adopted for direct daemon execution".to_string()),
                Some(serde_json::json!({
                    "phase": "staged_execution_adopted",
                    "staged_execution_envelope": expected_envelope,
                    "local_execution": metadata,
                })),
            )?;
            Ok((
                inner
                    .jobs
                    .get(&job_id)
                    .expect("adopted job exists")
                    .job
                    .clone(),
                true,
            ))
        })
    }

    /// Take over a queued adopted job only after the daemon lifecycle has
    /// proved the exact previous lease dead. A merely different lease is not
    /// ownership evidence: its worker may still be waiting for capacity.
    pub(crate) fn recover_proven_dead_adopted_staged_direct_execution(
        &self,
        job_id: Uuid,
        runner_id: &str,
        proven_dead_lease_id: &str,
    ) -> Result<Option<Job>> {
        let Some(current_lease_id) = self.daemon_lease_id.as_deref() else {
            return Ok(None);
        };
        self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            let Some(local) = stored.local_runner.as_ref() else {
                return Ok(None);
            };
            if local.runner_id != runner_id {
                return Err(Error::validation_invalid_argument(
                    "runner_id",
                    "adopted staged execution belongs to a different runner",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if stored.job.status != JobStatus::Queued || stored.local_child.is_some() {
                return Ok(None);
            }
            let Some(previous_lease_id) = stored.job.daemon_lease_id.clone() else {
                return Ok(None);
            };
            if previous_lease_id == current_lease_id {
                return Ok(None);
            }
            if previous_lease_id != proven_dead_lease_id {
                return Err(Error::validation_invalid_argument(
                    "proven_dead_lease_id",
                    "adopted staged execution is not owned by the proven-dead daemon lease",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            stored.job.daemon_lease_id = Some(current_lease_id.to_string());
            stored.job.updated_at_ms = timestamp_ms();
            let job = stored.job.clone();
            Self::append_event_already_locked(
                self,
                inner,
                job_id,
                JobEventKind::Progress,
                Some("abandoned staged direct pre-worker handoff recovered".to_string()),
                Some(serde_json::json!({
                    "phase": "staged_direct_execution_recovered",
                    "proven_dead_lease_id": previous_lease_id,
                    "daemon_lease_id": current_lease_id,
                })),
            )?;
            Ok(Some(job))
        })
    }

    /// Submits existing request fixtures through the canonical Runner API.
    #[cfg(any(test, feature = "test-support"))]
    pub fn submit_runner_api_fixture(&self, request: RemoteRunnerJobRequest) -> Result<Job> {
        self.submit_runner_api_request(request.into_runner_api_fixture())
    }

    pub fn submit_runner_api_request(&self, submission: RunnerApiSubmitRequest) -> Result<Job> {
        if submission.schema != RUNNER_API_SUBMIT_REQUEST_SCHEMA
            || submission.api_version != RUNNER_API_V1
        {
            return Err(Error::validation_invalid_argument(
                "schema",
                "unsupported Runner API submit request",
                Some(submission.schema),
                None,
            ));
        }
        if submission.submission_key.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "submission_key",
                "runner submission requires a caller-owned idempotency key",
                None,
                None,
            ));
        }
        let fingerprint = runner_api_submission_payload_fingerprint(&submission)?;
        let envelope = redacted_envelope_for_durable_replay(submission.envelope);
        let dispatch = envelope.dispatch.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "envelope.dispatch",
                "runner submission envelope requires dispatch",
                None,
                None,
            )
        })?;
        if dispatch.runner_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "runner_id",
                "remote runner job requires a runner id",
                None,
                None,
            ));
        }
        if dispatch.command.is_empty() {
            return Err(Error::validation_invalid_argument(
                "command",
                "remote runner job requires a command",
                None,
                None,
            ));
        }
        validate_command_assets(Some(&envelope.metadata))?;
        let workspace_claim_binding = submission.workspace_claim_binding;
        let workspace_owner_lease = submission.workspace_owner_lease;
        if let Some(binding) = workspace_claim_binding.as_ref() {
            binding.verify()?;
        }
        if let Some(lease) = workspace_owner_lease.as_ref() {
            lease.verify_shape(timestamp_ms())?;
        }
        let secret_env_plan = envelope.secret_env.clone().unwrap_or_default();
        reject_inline_durable_secret_env(&dispatch.env, &secret_env_plan)?;
        let lab_runner_workload = lab_runner_workload_from_execution_envelope(&envelope)?;
        super::with_runner_job_preparation(|preparation| {
            preparation.validate_lab_runner_workload_dispatch(
                lab_runner_workload.as_ref(),
                &dispatch.runner_id,
                dispatch.cwd.as_deref(),
                &dispatch.command,
                &secret_env_plan,
                envelope.mutation_policy.capture_patch,
            )
        })?;
        let path_materialization_plan = envelope
            .metadata
            .get("path_materialization_plan")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("decode envelope path materialization plan".to_string()),
                )
            })?;
        self.admit_remote_runner_job(
            RemoteRunnerAdmission {
                operation: dispatch.operation.clone(),
                runner_id: dispatch.runner_id.clone(),
                project_id: dispatch.project_id.clone(),
                source_snapshot: dispatch.source_snapshot.clone(),
                path_materialization_plan,
                run_ref_metadata: envelope_run_ref_metadata(
                    &envelope,
                    lab_runner_workload.as_ref(),
                ),
                submission_key: Some(submission.submission_key),
                submission_fingerprint: Some(fingerprint),
                stored_remote_runner: StoredRemoteRunnerJob {
                    envelope,
                    workspace_claim_binding,
                    workspace_owner_lease,
                    execution_context_id: None,
                    execution_receipt: None,
                    direct_execution_selected: false,
                },
            },
            timestamp_ms(),
        )
    }

    fn admit_remote_runner_job(&self, admission: RemoteRunnerAdmission, now: u64) -> Result<Job> {
        let RemoteRunnerAdmission {
            operation,
            runner_id,
            project_id,
            source_snapshot,
            path_materialization_plan,
            run_ref_metadata,
            submission_key,
            submission_fingerprint,
            stored_remote_runner,
        } = admission;
        self.durable_transaction(|inner| {
        if let Some(submission_key) = submission_key.as_deref() {
            let fingerprint = submission_fingerprint
                .as_deref()
                .expect("key has fingerprint");
            if let Some(existing) = inner.submission_keys.get(submission_key) {
                if existing.fingerprint != fingerprint {
                    return Err(Error::new(
                        ErrorCode::ValidationInvalidArgument,
                        "remote runner submission key is already bound to a different normalized payload",
                        serde_json::json!({
                            "schema": "homeboy/remote-runner-submission-conflict/v1",
                            "submission_key": submission_key,
                            "existing_fingerprint": existing.fingerprint,
                            "submitted_fingerprint": fingerprint,
                            "accepted_job_id": existing.job_id,
                        }),
                    ));
                }
                let job = inner
                    .jobs
                    .get(&existing.job_id)
                    .map(|stored| stored.job.clone())
                    .ok_or_else(|| {
                        Error::internal_unexpected(
                            "durable runner submission index references a missing job",
                        )
                    })?;
                if job.status.is_terminal() {
                    // The original terminal snapshot is authoritative. A replay
                    // is an observation, not new work or a legal state mutation.
                    return Ok(job);
                }
                Self::append_event_already_locked(
                    self,
                    &mut *inner,
                    job.id,
                    JobEventKind::Progress,
                    Some("remote runner submission replayed".to_string()),
                    Some(serde_json::json!({
                        "schema": "homeboy/remote-runner-submission/v1",
                        "submission_key": submission_key,
                        "payload_fingerprint": fingerprint,
                        "decision": "replay",
                        "accepted_job_id": job.id,
                    })),
                )?;
                return Ok(job);
            }
            if let Some(expired) = inner.expired_submission_keys.get(submission_key) {
                return Err(Error::new(
                    ErrorCode::ValidationInvalidArgument,
                    "remote runner submission key has expired with its retained job evidence",
                    serde_json::json!({
                        "schema": "homeboy/remote-runner-submission-expired/v1",
                        "submission_key": submission_key,
                        "payload_fingerprint": expired.fingerprint,
                        "expired_job_id": expired.job_id,
                    }),
                ));
            }
            if let Some(expired) = self
                .persistence
                .as_ref()
                .map(|persistence| {
                    lookup_tombstone(
                        &persistence.path,
                        ReplayTombstoneKind::RemoteRunner,
                        submission_key,
                    )
                })
                .transpose()?
                .flatten()
            {
                return Err(Error::new(
                    ErrorCode::ValidationInvalidArgument,
                    "remote runner submission key has expired with its retained job evidence",
                    serde_json::json!({
                        "schema": "homeboy/remote-runner-submission-expired/v1",
                        "submission_key": submission_key,
                        "payload_fingerprint": expired.fingerprint,
                        "expired_job_id": expired.job_id,
                    }),
                ));
            }
        }
        let mut job = Job {
            id: Uuid::new_v4(),
            operation,
            status: JobStatus::Queued,
            created_at_ms: now,
            updated_at_ms: now,
            started_at_ms: None,
            finished_at_ms: None,
            event_count: 0,
            source_snapshot,
            path_materialization_plan,
            stale_reason: None,
            daemon_lease_id: self.daemon_lease_id.clone(),
            target_runner_id: Some(runner_id),
            target_project_id: project_id,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        };

        inner.jobs.insert(
            job.id,
            StoredJob {
                job: job.clone(),
                events: Vec::new(),
                admission_idempotency_key: None,
                controller_job: None,
                admission_lease: None,
                remote_runner: Some(stored_remote_runner),
                local_runner: None,
                local_child: None,
            },
        );
        if let (Some(submission_key), Some(fingerprint)) =
            (submission_key.clone(), submission_fingerprint.clone())
        {
            inner.submission_keys.insert(
                submission_key,
                RemoteRunnerSubmission {
                    fingerprint,
                    job_id: job.id,
                },
            );
        }
        let mut evidence = run_ref_metadata.unwrap_or_else(|| serde_json::json!({}));
        evidence["status"] = serde_json::json!("queued");
        if let (Some(submission_key), Some(fingerprint)) =
            (submission_key.as_deref(), submission_fingerprint.as_deref())
        {
            evidence["submission"] = serde_json::json!({
                "schema": "homeboy/remote-runner-submission/v1",
                "submission_key": submission_key,
                "payload_fingerprint": fingerprint,
                "decision": "new",
                "accepted_job_id": job.id,
            });
        }
        let sequence = self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let stored = inner.jobs.get_mut(&job.id).expect("newly inserted job");
        stored.events.push(JobEvent {
            sequence,
            job_id: job.id,
            kind: JobEventKind::Status,
            timestamp_ms: now,
            message: Some("remote runner job queued".to_string()),
            data: Some(evidence),
        });
        stored.job.event_count = stored.events.len();
        // Return the admitted projection, including the durable queued event.
        job = stored.job.clone();
        // Persist job creation, queue evidence, and key index as one locked
        // snapshot. This is the admission commit point used by lost-response
        // recovery and daemon restart replay.
        Ok(job)
        })
    }

    /// Cancel exactly one daemon-local runner job after proving its durable
    /// controller projection still belongs to the requesting run.
    pub fn cancel_local_runner_projection(
        &self,
        job_id: Uuid,
        request: &RunnerJobProjectionCancelRequest,
    ) -> Result<Job> {
        let expected_runner_id = request.expected_runner_id.trim();
        let expected_durable_run_id = request.expected_durable_run_id.trim();
        if expected_runner_id.is_empty() || expected_durable_run_id.is_empty() {
            return Err(Error::validation_invalid_argument(
                "projection",
                "strict runner projection cancellation requires non-empty runner and durable run IDs",
                Some(job_id.to_string()),
                None,
            ));
        }

        let (job, child) = self.durable_transaction(|inner| {
            let stored = inner
                .jobs
                .get_mut(&job_id)
                .ok_or_else(|| job_not_found(job_id))?;
            let Some(local_runner) = stored.local_runner.as_ref() else {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "job has no daemon-local runner projection",
                    Some(job_id.to_string()),
                    None,
                ));
            };
            let Some(lifecycle) = local_runner.lifecycle.as_ref() else {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "daemon-local runner projection has no lifecycle metadata",
                    Some(job_id.to_string()),
                    None,
                ));
            };
            let Some(durable_run_id) = lifecycle
                .durable_run_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                return Err(Error::validation_invalid_argument(
                    "job_id",
                    "daemon-local runner projection has no durable run ID",
                    Some(job_id.to_string()),
                    None,
                ));
            };
            if local_runner.runner_id != expected_runner_id {
                return Err(Error::validation_invalid_argument(
                    "expected_runner_id",
                    "daemon-local runner projection belongs to a different runner",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if durable_run_id != expected_durable_run_id {
                return Err(Error::validation_invalid_argument(
                    "expected_durable_run_id",
                    "daemon-local runner projection belongs to a different durable run",
                    Some(job_id.to_string()),
                    None,
                ));
            }
            if stored.job.status.is_terminal() {
                let child = (stored.job.status == JobStatus::Cancelled)
                    .then(|| {
                        stored
                            .local_child
                            .as_ref()
                            .and_then(|child| child.process.clone())
                    })
                    .flatten();
                return Ok((stored.job.clone(), child));
            }
            validate_transition(stored.job.status, JobStatus::Cancelled)?;
            let now = timestamp_ms();
            stored.job.status = JobStatus::Cancelled;
            stored.job.updated_at_ms = now;
            stored.job.finished_at_ms = Some(now);
            let event = JobEvent {
                sequence: self.next_event_sequence.fetch_add(1, Ordering::SeqCst) + 1,
                job_id,
                kind: JobEventKind::Status,
                timestamp_ms: now,
                message: Some("cancelled via strict runner projection".to_string()),
                data: Some(serde_json::json!({ "status": JobStatus::Cancelled })),
            };
            stored.events.push(event);
            apply_event_retention(&mut stored.events, self.event_retention_limit());
            stored.job.event_count = stored.events.len();
            Ok((
                stored.job.clone(),
                stored
                    .local_child
                    .as_ref()
                    .and_then(|child| child.process.clone()),
            ))
        })?;
        super::store::reap_cancelled_local_child(child.as_ref())?;
        Ok(job)
    }
}

fn reject_inline_durable_secret_env(
    env: &HashMap<String, String>,
    secret_env_plan: &SecretEnvPlan,
) -> Result<()> {
    let inline_secret_names = secret_env_plan
        .secret_env_names()
        .into_iter()
        .filter(|name| env.get(name).is_some_and(|value| !value.is_empty()))
        .collect::<Vec<_>>();
    if inline_secret_names.is_empty() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "env",
        "durable runner jobs cannot accept inline secret env values",
        Some("durable_reverse_runner_inline_secret_env".to_string()),
        Some(vec![
            format!("Inline secret variables: {}", inline_secret_names.join(", ")),
            "Configure named runner secret_env or SecretEnvPlan references so the worker can rehydrate secrets after replay.".to_string(),
        ]),
    ))
}

fn default_runner_exec_operation() -> String {
    "runner.exec".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lifecycle(run_id: &str) -> RunnerJobLifecycleMetadata {
        RunnerJobLifecycleMetadata {
            source: Some("runner-daemon".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: Some(run_id.to_string()),
            active_child_count: None,
            active_cell_count: None,
        }
    }

    fn workload(run_id: &str) -> LabRunnerWorkload {
        serde_json::from_value(json!({
            "schema": "homeboy/runner-workload/v1",
            "workload_id": "plan.runner_workload",
            "kind": { "command_label": "agent-task run-plan", "command_family": "agent_task" },
            "agent_task": {
                "run_id": run_id,
                "dispatch_kind": "run_plan",
                "lifecycle_mirror_policy": "run_plan_aggregate"
            },
            "workspace_mappings": {
                "source_path_mode": "existing_remote",
                "workspace_mode_policy": "existing",
                "mapping_ref": null
            },
            "required_capabilities": [],
            "required_secrets": { "categories": [], "secret_env_plan": {} },
            "required_extensions": [],
            "mutation_policy": {
                "capture_patch": false,
                "mutation_flag": null,
                "allow_dirty_lab_workspace": false
            },
            "assignment": {
                "runner_id": "homeboy-lab",
                "runner_mode": "direct_ssh",
                "source": "explicit"
            },
            "state": {
                "status": "offloaded",
                "remote_workspace": "/tmp",
                "fallback_reason": null
            },
            "result_refs": {
                "plan_id": "plan",
                "proof_id": null,
                "workspace_mapping_ref": null
            }
        }))
        .expect("workload")
    }

    fn assert_run_ref_wire(metadata: &Value, durable_run_id: Value, agent_task_run_id: Value) {
        assert_eq!(metadata["durable_run_id"], durable_run_id);
        assert_eq!(metadata["agent_task_run_id"], agent_task_run_id);
        assert_eq!(metadata.as_object().map(|object| object.len()), Some(2));
    }

    #[test]
    fn canonical_run_ref_metadata_copies_lifecycle_id_onto_the_existing_alias() {
        let metadata = canonical_run_ref_metadata(Some(&lifecycle("run-1")), None, None)
            .expect("lifecycle identity");
        assert_run_ref_wire(&metadata, json!("run-1"), json!("run-1"));
    }

    #[test]
    fn canonical_run_ref_metadata_keeps_workload_identity_on_the_existing_alias() {
        let metadata = canonical_run_ref_metadata(None, Some(&workload("run-2")), None)
            .expect("workload identity");
        assert_run_ref_wire(&metadata, Value::Null, json!("run-2"));
    }

    #[test]
    fn canonical_run_ref_metadata_still_emits_legacy_alias_input() {
        let metadata = canonical_run_ref_metadata(
            None,
            None,
            Some(&json!({ "agent_task_run_id": "legacy-run" })),
        )
        .expect("legacy identity");
        assert_run_ref_wire(&metadata, Value::Null, json!("legacy-run"));
    }

    #[test]
    fn canonical_run_ref_metadata_preserves_divergent_durable_and_alias_sources() {
        let metadata = canonical_run_ref_metadata(
            Some(&lifecycle("canonical-run")),
            Some(&workload("workload-run")),
            Some(&json!({
                "run_id": "metadata-run",
                "agent_task_run_id": "legacy-run"
            })),
        )
        .expect("canonical identity");
        assert_run_ref_wire(&metadata, json!("canonical-run"), json!("workload-run"));
    }
}

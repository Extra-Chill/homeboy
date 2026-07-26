//! Controller-owned promotion execution.
//!
//! This is deliberately below the CLI boundary: durable controller jobs and
//! interactive commands use the same checkpointed mutation state machine.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::daemon::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobHandle, ControllerJobPublicError,
};
use homeboy_core::Result;

use crate::agent_task_gate::VerifyGateOptions;
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::{
    promote_with_checkpoint, resume_promoted_patch, AgentTaskPromotionOptions,
    AgentTaskPromotionReport,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskPromotionRequest {
    pub source: String,
    pub source_run_id: Option<String>,
    pub source_path: Option<std::path::PathBuf>,
    pub source_worktree_path: Option<std::path::PathBuf>,
    pub to_worktree: String,
    pub base_ref: Option<String>,
    pub task_base_sha: Option<String>,
    pub candidate_ref: Option<String>,
    pub task_id: Option<String>,
    pub artifact_id: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub gates: VerifyGateOptions,
    pub provider_command: Option<String>,
    pub provider_invocation: Option<CommandInvocation>,
}

pub const AGENT_TASK_PROMOTION_JOB_TYPE: &str = "agent-task-promotion";
pub const AGENT_TASK_PROMOTION_JOB_VERSION: u32 = 1;

/// Durable controller-job request. `request` is a fully resolved promotion
/// input, including the source artifact and destination identity; a recovered
/// job never reparses operator argv or reselects an aggregate candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskPromotionJob {
    pub schema: String,
    pub idempotency_key: String,
    pub request: AgentTaskPromotionRequest,
    #[serde(default = "promotion_job_phase_queued")]
    pub phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskPromotionJobSubmission {
    pub job_id: String,
    pub status_command: String,
    pub watch_command: String,
    pub cancel_command: String,
}

fn promotion_job_phase_queued() -> String {
    "queued".to_string()
}

struct AgentTaskPromotionJobDriver;

impl ControllerJobDriver for AgentTaskPromotionJobDriver {
    fn job_type(&self) -> &'static str {
        AGENT_TASK_PROMOTION_JOB_TYPE
    }

    fn version(&self) -> u32 {
        AGENT_TASK_PROMOTION_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        let job: AgentTaskPromotionJob =
            serde_json::from_value(request.clone()).map_err(|error| {
                homeboy_core::Error::validation_invalid_argument(
                    "promotion_job",
                    format!("invalid durable promotion job: {error}"),
                    None,
                    None,
                )
            })?;
        Ok(serde_json::json!({
            "schema": job.schema,
            "idempotency_key": job.idempotency_key,
            "phase": job.phase,
            "source_run_id": job.request.source_run_id,
            "task_id": job.request.task_id,
            "artifact_id": job.request.artifact_id,
            "to_worktree": job.request.to_worktree,
            "base_ref": job.request.base_ref,
        }))
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        Ok(progress.clone())
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        Ok(result.clone())
    }

    fn public_error(&self, error: &homeboy_core::Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: error.message.clone(),
            data: error.details.clone(),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        let job: AgentTaskPromotionJob =
            serde_json::from_value(request.clone()).map_err(|error| {
                homeboy_core::Error::validation_invalid_argument(
                    "promotion_job",
                    error.to_string(),
                    None,
                    None,
                )
            })?;
        if job.schema != "homeboy/agent-task-promotion-job/v1"
            || job.idempotency_key.trim().is_empty()
            || job
                .request
                .source_run_id
                .as_deref()
                .is_none_or(str::is_empty)
            || job.request.artifact_id.as_deref().is_none_or(str::is_empty)
        {
            return Err(homeboy_core::Error::validation_invalid_argument(
                "promotion_job",
                "promotion jobs require a schema, idempotency key, durable source run, and exact patch artifact",
                None,
                None,
            ));
        }
        Ok(())
    }

    fn execute(&self, prepared: Value, handle: ControllerJobHandle) -> Result<Value> {
        let mut job: AgentTaskPromotionJob = serde_json::from_value(prepared).map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("parse promotion job checkpoint".to_string()),
            )
        })?;
        if handle.is_cancelled() {
            return Ok(serde_json::json!({ "phase": "cancelled" }));
        }
        job.phase = "mutating_and_verifying".to_string();
        handle.checkpoint(serde_json::to_value(&job).unwrap_or(Value::Null))?;
        handle.progress(serde_json::json!({ "phase": job.phase }))?;
        let report = execute_promotion(job.request.clone())?;
        job.phase = "completed".to_string();
        handle.checkpoint(serde_json::to_value(&job).unwrap_or(Value::Null))?;
        Ok(serde_json::json!({ "phase": job.phase, "promotion": report }))
    }

    fn cancel(&self, _prepared: &Value) -> Result<()> {
        Ok(())
    }
}

/// Register the domain driver with core's generic controller-job lifecycle.
/// Registration is idempotent because CLI startup can run in test processes
/// that initialize the command runtime more than once.
pub fn register_promotion_job_driver() {
    let _ = controller_job_driver::register_controller_job_driver(std::sync::Arc::new(
        AgentTaskPromotionJobDriver,
    ));
}

/// Submit a fully resolved promotion to the generic controller-job lifecycle.
/// The operation key binds the durable source artifact to its declared target,
/// preventing duplicate apply after a lost client response or daemon restart.
pub fn submit_promotion_job(
    request: AgentTaskPromotionRequest,
) -> Result<AgentTaskPromotionJobSubmission> {
    let run_id = request.source_run_id.clone().ok_or_else(|| {
        homeboy_core::Error::validation_invalid_argument(
            "source",
            "durable promotion queueing requires a controller-owned source run",
            None,
            None,
        )
    })?;
    let artifact_id = request.artifact_id.clone().ok_or_else(|| {
        homeboy_core::Error::validation_invalid_argument(
            "artifact_id",
            "durable promotion queueing requires an exact patch artifact",
            None,
            None,
        )
    })?;
    let idempotency_key = format!("promotion:{run_id}:{artifact_id}:{}", request.to_worktree);
    let job = AgentTaskPromotionJob {
        schema: "homeboy/agent-task-promotion-job/v1".to_string(),
        idempotency_key: idempotency_key.clone(),
        request,
        phase: promotion_job_phase_queued(),
    };
    let daemon = homeboy_core::daemon::ensure_running(homeboy_core::daemon::DEFAULT_ADDR)?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| homeboy_core::Error::internal_unexpected(error.to_string()))?;
    let response: Value = client
        .post(format!("http://{}/controller/jobs", daemon.address))
        .json(&serde_json::json!({
            "type": AGENT_TASK_PROMOTION_JOB_TYPE,
            "version": AGENT_TASK_PROMOTION_JOB_VERSION,
            "request": job,
            "idempotency_key": idempotency_key,
        }))
        .send()
        .map_err(|error| homeboy_core::Error::internal_unexpected(error.to_string()))?
        .json()
        .map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("parse promotion job submission".to_string()),
            )
        })?;
    let job_id = response
        .pointer("/data/body/job/id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            homeboy_core::Error::internal_unexpected(
                "controller daemon did not return a promotion job id",
            )
        })?
        .to_string();
    let start: Value = client
        .post(format!(
            "http://{}/controller/jobs/{job_id}/start",
            daemon.address
        ))
        .send()
        .map_err(|error| homeboy_core::Error::internal_unexpected(error.to_string()))?
        .json()
        .map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("start promotion job".to_string()),
            )
        })?;
    if start.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(homeboy_core::Error::internal_unexpected(
            "controller daemon rejected promotion job start",
        ));
    }
    Ok(AgentTaskPromotionJobSubmission {
        status_command: format!("homeboy activity show {job_id}"),
        watch_command: format!("homeboy activity watch {job_id}"),
        cancel_command: format!("homeboy activity cancel {job_id}"),
        job_id,
    })
}

impl AgentTaskPromotionRequest {
    fn options(&self) -> AgentTaskPromotionOptions {
        AgentTaskPromotionOptions {
            source: self.source.clone(),
            source_run_id: self.source_run_id.clone(),
            source_path: self.source_path.clone(),
            source_worktree_path: self.source_worktree_path.clone(),
            base_ref: self.base_ref.clone(),
            task_base_sha: self.task_base_sha.clone(),
            candidate_ref: self.candidate_ref.clone(),
            to_worktree: self.to_worktree.clone(),
            task_id: self.task_id.clone(),
            artifact_id: self.artifact_id.clone(),
            dry_run: self.dry_run,
            gates: self.gates.clone(),
            provider_command: self.provider_command.clone(),
            provider_invocation: self.provider_invocation.clone(),
        }
    }
}

/// Execute or resume an immutable controller-owned promotion. The durable run
/// is the operation key: each post-apply checkpoint is persisted before gates,
/// so a restart can only verify the same materialized candidate.
pub fn execute_promotion(request: AgentTaskPromotionRequest) -> Result<AgentTaskPromotionReport> {
    let previous = request.source_run_id.as_ref().and_then(|run_id| {
        agent_task_lifecycle::status(run_id)
            .ok()
            .and_then(|record| record.metadata.get("latest_promotion").cloned())
    });
    let options = request.options();
    let report = if let Some(previous) = previous
        .filter(|previous| promotion_is_resumable(previous, options.gates.rerun_completed_gates))
    {
        let target_path = previous
            .pointer("/target/path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                homeboy_core::Error::validation_invalid_argument(
                    "promotion",
                    "gate-failed promotion has no materialized target path to resume",
                    None,
                    None,
                )
            })?;
        resume_promoted_patch(options, Path::new(target_path), &previous)?
    } else {
        let checkpoint_run_id = (!request.dry_run)
            .then(|| request.source_run_id.clone())
            .flatten();
        promote_with_checkpoint(options, |checkpoint| {
            if let Some(run_id) = checkpoint_run_id.as_deref() {
                agent_task_lifecycle::record_promotion(
                    run_id,
                    serde_json::to_value(checkpoint).map_err(|error| {
                        homeboy_core::Error::internal_json(
                            error.to_string(),
                            Some("serialize pending agent-task promotion report".to_string()),
                        )
                    })?,
                )?;
            }
            Ok(())
        })?
    };
    if let Some(run_id) = request.source_run_id.filter(|_| !request.dry_run) {
        agent_task_lifecycle::record_promotion(
            &run_id,
            serde_json::to_value(&report).map_err(|error| {
                homeboy_core::Error::internal_json(
                    error.to_string(),
                    Some("serialize agent-task promotion report".to_string()),
                )
            })?,
        )?;
    }
    Ok(report)
}

pub fn promotion_is_resumable(previous: &Value, rerun_completed_gates: bool) -> bool {
    let status = previous.get("status").and_then(Value::as_str);
    matches!(status, Some("gate_failed") | Some("verification_pending"))
        || (rerun_completed_gates && status == Some("applied"))
}

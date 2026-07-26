//! Controller-owned promotion execution.
//!
//! This is deliberately below the CLI boundary: durable controller jobs and
//! interactive commands use the same checkpointed mutation state machine.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
const AGENT_TASK_PROMOTION_JOB_SCHEMA: &str = "homeboy/agent-task-promotion-job/v1";

/// Durable controller-job request. `request` is a fully resolved promotion
/// input, including the source artifact and destination identity; a recovered
/// job never reparses operator argv or reselects an aggregate candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskPromotionJob {
    pub schema: String,
    pub idempotency_key: String,
    pub request: AgentTaskPromotionRequest,
    #[serde(default)]
    pub phase: AgentTaskPromotionJobPhase,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskPromotionJobPhase {
    #[default]
    Queued,
    MutatingAndVerifying,
    Completed,
}

impl AgentTaskPromotionJob {
    pub fn new(request: AgentTaskPromotionRequest) -> Result<Self> {
        let source_run_id = required_job_field(&request.source_run_id, "source_run_id")?;
        let artifact_id = required_job_field(&request.artifact_id, "artifact_id")?;
        if request.to_worktree.trim().is_empty() {
            return Err(invalid_promotion_job(
                "promotion jobs require a destination worktree",
            ));
        }
        if request.provider_command.is_some() || request.provider_invocation.is_some() {
            return Err(invalid_promotion_job(
                "promotion jobs require controller-owned provider references, not inline commands",
            ));
        }

        Ok(Self {
            schema: AGENT_TASK_PROMOTION_JOB_SCHEMA.to_string(),
            idempotency_key: format!(
                "promotion:{source_run_id}:{artifact_id}:{}",
                request.to_worktree
            ),
            request,
            phase: AgentTaskPromotionJobPhase::Queued,
        })
    }

    fn parse(value: Value) -> Result<Self> {
        let job: Self = serde_json::from_value(value).map_err(|error| {
            invalid_promotion_job(&format!("invalid durable promotion job: {error}"))
        })?;
        job.validate()?;
        Ok(job)
    }

    fn validate(&self) -> Result<()> {
        if self.schema != AGENT_TASK_PROMOTION_JOB_SCHEMA || self.idempotency_key.trim().is_empty()
        {
            return Err(invalid_promotion_job(
                "promotion jobs require a recognized schema and idempotency key",
            ));
        }
        let expected = Self::new(self.request.clone())?;
        if self.idempotency_key != expected.idempotency_key {
            return Err(invalid_promotion_job(
                "promotion job idempotency key does not match its immutable request",
            ));
        }
        Ok(())
    }

    fn public_projection(&self) -> Value {
        serde_json::json!({
            "schema": self.schema,
            "idempotency_key": self.idempotency_key,
            "phase": self.phase,
            "source_run_id": self.request.source_run_id,
            "task_id": self.request.task_id,
            "artifact_id": self.request.artifact_id,
            "to_worktree": self.request.to_worktree,
            "base_ref": self.request.base_ref,
        })
    }
}

fn required_job_field<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str> {
    value
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid_promotion_job(&format!("promotion jobs require {name}")))
}

fn invalid_promotion_job(message: &str) -> homeboy_core::Error {
    homeboy_core::Error::validation_invalid_argument("promotion_job", message, None, None)
}

pub struct AgentTaskPromotionJobDriver;

impl ControllerJobDriver for AgentTaskPromotionJobDriver {
    fn job_type(&self) -> &'static str {
        AGENT_TASK_PROMOTION_JOB_TYPE
    }

    fn version(&self) -> u32 {
        AGENT_TASK_PROMOTION_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(AgentTaskPromotionJob::parse(request.clone())?.public_projection())
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        let phase: AgentTaskPromotionJobPhase = serde_json::from_value(
            progress.get("phase").cloned().unwrap_or(Value::Null),
        )
        .map_err(|error| invalid_promotion_job(&format!("invalid promotion progress: {error}")))?;
        Ok(serde_json::json!({ "phase": phase }))
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        Ok(serde_json::json!({
            "phase": result.get("phase").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_error(&self, error: &homeboy_core::Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: "controller-owned promotion failed".to_string(),
            data: serde_json::json!({ "code": format!("{:?}", error.code) }),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        AgentTaskPromotionJob::parse(request.clone()).map(|_| ())
    }

    fn execute(&self, prepared: Value, handle: ControllerJobHandle) -> Result<Value> {
        let mut job = AgentTaskPromotionJob::parse(prepared)?;
        job.phase = AgentTaskPromotionJobPhase::MutatingAndVerifying;
        handle.checkpoint(serde_json::to_value(&job).map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("serialize promotion checkpoint".to_string()),
            )
        })?)?;
        handle.progress(serde_json::json!({ "phase": job.phase }))?;
        let report = execute_promotion(job.request.clone())?;
        job.phase = AgentTaskPromotionJobPhase::Completed;
        handle.checkpoint(serde_json::to_value(&job).map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("serialize promotion checkpoint".to_string()),
            )
        })?)?;
        Ok(serde_json::json!({ "phase": job.phase, "promotion": report }))
    }

    fn resume(&self, checkpoint: Value, handle: ControllerJobHandle) -> Result<Value> {
        self.execute(checkpoint, handle)
    }

    fn cancel(&self, _prepared: &Value) -> Result<()> {
        Err(invalid_promotion_job(
            "promotion jobs cannot be cancelled after execution starts",
        ))
    }
}

/// Register the domain driver with core's generic controller-job lifecycle.
/// Registration is idempotent because CLI startup can run in test processes
/// that initialize the command runtime more than once.
pub fn register_promotion_job_driver() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        controller_job_driver::register_controller_job_driver(std::sync::Arc::new(
            AgentTaskPromotionJobDriver,
        ))
        .expect("register promotion controller job driver");
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> AgentTaskPromotionRequest {
        AgentTaskPromotionRequest {
            source: "private aggregate content".to_string(),
            source_run_id: Some("run-123".to_string()),
            source_path: Some("/private/source.json".into()),
            source_worktree_path: Some("/private/worktree".into()),
            to_worktree: "homeboy@promotion".to_string(),
            base_ref: Some("main".to_string()),
            task_base_sha: Some("base-sha".to_string()),
            candidate_ref: None,
            task_id: Some("task-1".to_string()),
            artifact_id: Some("patch-1".to_string()),
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        }
    }

    #[test]
    fn durable_job_uses_a_deterministic_key_and_preserves_its_checkpoint_phase() {
        let job = AgentTaskPromotionJob::new(request()).expect("valid durable job");
        let same_job = AgentTaskPromotionJob::new(request()).expect("same durable job");

        assert_eq!(job.idempotency_key, same_job.idempotency_key);
        assert_eq!(job.phase, AgentTaskPromotionJobPhase::Queued);

        let mut checkpoint = job.clone();
        checkpoint.phase = AgentTaskPromotionJobPhase::MutatingAndVerifying;
        let recovered = AgentTaskPromotionJob::parse(
            serde_json::to_value(checkpoint).expect("serialize checkpoint"),
        )
        .expect("recover checkpoint");
        assert_eq!(
            recovered.phase,
            AgentTaskPromotionJobPhase::MutatingAndVerifying
        );
        assert_eq!(recovered.idempotency_key, job.idempotency_key);
    }

    #[test]
    fn durable_job_rejects_missing_or_inline_provider_inputs() {
        let mut missing_artifact = request();
        missing_artifact.artifact_id = None;
        assert!(AgentTaskPromotionJob::new(missing_artifact).is_err());

        let mut inline_provider = request();
        inline_provider.provider_command = Some("provider --token private".to_string());
        assert!(AgentTaskPromotionJob::new(inline_provider).is_err());
    }

    #[test]
    fn driver_redacts_private_request_fields_and_accepts_its_typed_payload() {
        let job = AgentTaskPromotionJob::new(request()).expect("valid durable job");
        let value = serde_json::to_value(job).expect("serialize job");
        let driver = AgentTaskPromotionJobDriver;

        driver
            .validate_secret_references(&value)
            .expect("validate typed job");
        let public = driver.public_request(&value).expect("public projection");
        let public_text = public.to_string();
        assert!(!public_text.contains("private aggregate content"));
        assert!(!public_text.contains("/private/source.json"));
        assert!(!public_text.contains("base-sha"));
        assert_eq!(public["source_run_id"], "run-123");
        assert_eq!(public["phase"], "queued");
    }

    #[test]
    fn driver_registration_is_idempotent() {
        register_promotion_job_driver();
        register_promotion_job_driver();
    }
}

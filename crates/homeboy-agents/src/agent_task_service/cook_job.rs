//! Daemon-owned lifecycle for a locally-placed, detached Cook.
//!
//! # Why this driver supervises rather than executes
//!
//! A Cook spawns an AI provider subprocess with credentials, in a worktree. Two
//! facts about this workspace make executing that subprocess *inside the daemon*
//! unsafe, so this driver deliberately does not:
//!
//! 1. `agent_task_secrets::resolve_secret_env_with_config_and_fallbacks` lists
//!    the ambient process environment as the **first** secret provider
//!    (`SecretEnvValueProvider::new("env", |name| env::var(name).ok())`), ahead
//!    of configured agent-task secrets. Whichever process runs the cook decides
//!    which credentials the provider receives.
//! 2. The main provider invocation in `agent_task_provider::command_runner`
//!    never calls `env_clear` — only the bounded readiness probe does — so the
//!    provider inherits the full environment of the executing process.
//! 3. The daemon is spawned by `daemon::control::spawn_and_wait_for_lease_attempt`
//!    with neither `.env_clear()` nor `.current_dir()`, and it is long-lived and
//!    shared. Its environment and working directory are a snapshot of whichever
//!    caller happened to start it first, which `LocalControllerJobClient::connect`
//!    does implicitly via `ensure_running`.
//!
//! Together those mean a daemon-hosted cook would silently resolve credentials
//! from a different environment than the operator's shell — a different account,
//! or none at all — with the outcome depending on who started the daemon. So the
//! launcher still spawns the cook child exactly as it always did, preserving the
//! execution environment byte for byte, and the daemon owns everything *around*
//! that child: the durable job record, checkpointing, cancellation, and HTTP
//! inspection.
//!
//! # The correctness property this buys
//!
//! Because this driver never spawns provider work, `resume` cannot double-run an
//! attempt. It re-adopts supervision of a process it did not create, identified
//! by PID *and* kernel start identity so PID reuse cannot alias a stranger.
//! Idempotency here is structural rather than claimed.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

use homeboy_core::daemon::controller_job_driver::ControllerJobPublicError;
use homeboy_core::process::{
    process_identity_state_with_start_identity, ProcessIdentityState, ProcessStartIdentity,
};
use homeboy_core::{Error, ErrorCode, Result};

use super::work_job::{
    register_work_job_handler, work_job_submission, WorkJobHandle, WorkJobHandler,
    WorkJobInvocation,
};
use crate::agent_task_lifecycle;

pub const AGENT_TASK_COOK_JOB_TYPE: &str = "agent-task-cook";
pub const AGENT_TASK_COOK_JOB_VERSION: u32 = 1;
const AGENT_TASK_COOK_JOB_SCHEMA: &str = "homeboy/agent-task-cook-job/v1";

/// How often supervision re-reads durable cook state and child liveness.
const SUPERVISION_POLL: Duration = Duration::from_millis(250);
const CHILD_RESULT_LOG_LIMIT: usize = 16 * 1024;

/// The durable controller-job request for one detached Cook.
///
/// `deny_unknown_fields` is the enforcement point for
/// [`ControllerJobDriver::validate_secret_references`]: the only admissible
/// request is a *reference* to durable cook state plus the child identity needed
/// to supervise it. A caller cannot smuggle a prompt, a provider invocation, an
/// environment block, or a notification route through this struct, because any
/// field not named here is a hard parse error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskCookJobRequest {
    pub schema: String,
    /// The durable Cook alias. Every execution input lives behind this id in the
    /// Cook recipe, never inline in the job.
    pub cook_id: String,
    pub child_pid: u32,
    pub child_start_identity: ProcessStartIdentity,
    /// A retry has an existing Cook alias but needs a fresh durable supervisor.
    /// Initial Cook jobs retain the Cook id as their owner identity.
    #[serde(default)]
    pub supervisor_id: Option<String>,
    /// The retry run this supervisor is allowed to observe. Unlike the Cook
    /// alias, it never advances when a later retry becomes the index latest.
    #[serde(default)]
    pub pinned_retry_run_id: Option<String>,
    /// Sanitized directory name beneath Homeboy's detached-session data root.
    #[serde(default)]
    pub child_session_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskCookJobPhase {
    /// Admitted, not yet supervising.
    #[default]
    Queued,
    /// The daemon is watching a live detached child.
    Supervising,
    /// The child ended and its durable outcome was observed.
    Completed,
}

/// The durable controller job, including its recovery checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskCookJob {
    pub schema: String,
    pub idempotency_key: String,
    pub request: AgentTaskCookJobRequest,
    #[serde(default)]
    pub phase: AgentTaskCookJobPhase,
    /// The attempt id the cook published, once it has one. Recorded in the
    /// checkpoint so a resumed job reports the same handle it already announced.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Terminal run state observed for the cook, set only in `Completed`.
    #[serde(default)]
    pub terminal_state: Option<agent_task_lifecycle::AgentTaskRunState>,
}

impl AgentTaskCookJob {
    pub fn new(request: AgentTaskCookJobRequest) -> Result<Self> {
        if request.schema != AGENT_TASK_COOK_JOB_SCHEMA {
            return Err(invalid_cook_job(
                "cook jobs require a recognized request schema",
            ));
        }
        if request.cook_id.trim().is_empty() {
            return Err(invalid_cook_job("cook jobs require a durable cook id"));
        }
        if request.child_pid == 0 {
            return Err(invalid_cook_job(
                "cook jobs require the detached child's process id",
            ));
        }
        if request
            .child_session_ref
            .as_deref()
            .is_some_and(|reference| {
                reference.is_empty()
                    || homeboy_core::paths::sanitize_path_segment(reference) != reference
            })
        {
            return Err(invalid_cook_job(
                "cook jobs require a sanitized detached child session reference",
            ));
        }
        let run_id = request.pinned_retry_run_id.clone();
        Ok(Self {
            schema: AGENT_TASK_COOK_JOB_SCHEMA.to_string(),
            // The cook id is already unique and is the durable identity of this
            // work, so replaying a submit converges on one job rather than
            // creating a second supervisor for the same child.
            idempotency_key: format!(
                "agent-task-cook:{}",
                request
                    .pinned_retry_run_id
                    .as_deref()
                    .unwrap_or_else(|| request
                        .supervisor_id
                        .as_deref()
                        .unwrap_or(&request.cook_id))
            ),
            request,
            phase: AgentTaskCookJobPhase::Queued,
            run_id,
            terminal_state: None,
        })
    }

    fn parse(value: Value) -> Result<Self> {
        let job: Self = serde_json::from_value(value)
            .map_err(|error| invalid_cook_job(&format!("invalid durable cook job: {error}")))?;
        job.validate()?;
        Ok(job)
    }

    fn validate(&self) -> Result<()> {
        if self.schema != AGENT_TASK_COOK_JOB_SCHEMA || self.idempotency_key.trim().is_empty() {
            return Err(invalid_cook_job(
                "cook jobs require a recognized schema and idempotency key",
            ));
        }
        let expected = Self::new(self.request.clone())?;
        if self.idempotency_key != expected.idempotency_key {
            return Err(invalid_cook_job(
                "cook job idempotency key does not match its immutable request",
            ));
        }
        if self.phase == AgentTaskCookJobPhase::Completed && self.terminal_state.is_none() {
            return Err(invalid_cook_job(
                "completed cook jobs require an observed terminal state",
            ));
        }
        if self.phase != AgentTaskCookJobPhase::Completed && self.terminal_state.is_some() {
            return Err(invalid_cook_job(
                "only completed cook jobs may record a terminal state",
            ));
        }
        Ok(())
    }

    fn to_checkpoint(&self) -> Result<Value> {
        serde_json::to_value(self).map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("serialize cook job checkpoint".to_string()),
            )
        })
    }

    fn completed_result(&self) -> Result<Value> {
        let terminal_state = self.terminal_state.ok_or_else(|| {
            invalid_cook_job("completed cook jobs require an observed terminal state")
        })?;
        Ok(json!({
            "phase": self.phase,
            "cook_id": self.request.cook_id,
            "run_id": self.run_id,
            "terminal_state": terminal_state,
        }))
    }

    /// The public projection.
    ///
    /// Deliberately withheld, matching `AgentTaskPromotionJob::public_projection`'s
    /// conservatism about paths and execution inputs: the launcher log path and
    /// the child's start identity. The log path names an on-disk location under
    /// the operator's home and the cook's own log may quote provider output; the
    /// start identity is a kernel-level liveness token that only supervision
    /// needs. The prompt, provider invocation, notification route, gate policy
    /// and worktree never enter this job at all — they live behind `cook_id` in
    /// the durable Cook recipe.
    fn public_projection(&self) -> Value {
        json!({
            "schema": self.schema,
            "idempotency_key": self.idempotency_key,
            "phase": self.phase,
            "cook_id": self.request.cook_id,
            "durable_run_id": self.request.cook_id,
            "run_id": self.run_id,
            "terminal_state": self.terminal_state,
        })
    }
}

fn invalid_cook_job(message: &str) -> homeboy_core::Error {
    homeboy_core::Error::validation_invalid_argument("cook_job", message, None, None)
}

struct CookWorkHandler;

impl WorkJobHandler for CookWorkHandler {
    fn work_type(&self) -> &'static str {
        AGENT_TASK_COOK_JOB_TYPE
    }

    fn version(&self) -> u32 {
        AGENT_TASK_COOK_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(AgentTaskCookJob::parse(request.clone())?.public_projection())
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        Ok(json!({
            "phase": progress.get("phase").cloned().unwrap_or(Value::Null),
            "cook_id": progress.get("cook_id").cloned().unwrap_or(Value::Null),
            "run_id": progress.get("run_id").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        Ok(json!({
            "phase": result.get("phase").cloned().unwrap_or(Value::Null),
            "cook_id": result.get("cook_id").cloned().unwrap_or(Value::Null),
            "run_id": result.get("run_id").cloned().unwrap_or(Value::Null),
            "terminal_state": result.get("terminal_state").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_error(&self, error: &homeboy_core::Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: "controller-owned cook supervision failed".to_string(),
            data: json!({ "code": format!("{:?}", error.code) }),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        AgentTaskCookJob::parse(request.clone()).map(|_| ())
    }

    fn prepare(&self, request: Value) -> Result<Value> {
        let mut job = AgentTaskCookJob::parse(request)?;
        if job.phase != AgentTaskCookJobPhase::Queued || job.terminal_state.is_some() {
            return Err(invalid_cook_job(
                "new cook jobs must start queued without a terminal state",
            ));
        }
        job.phase = AgentTaskCookJobPhase::Supervising;
        job.to_checkpoint()
    }

    fn advance(
        &self,
        checkpoint: Value,
        handle: WorkJobHandle,
        invocation: WorkJobInvocation,
    ) -> Result<Value> {
        let mut job = AgentTaskCookJob::parse(checkpoint)?;
        if invocation == WorkJobInvocation::Resume
            && job.resume_disposition() == CookJobResumeDisposition::AlreadyComplete
        {
            return job.completed_result();
        }
        self.supervise(&mut job, handle)
    }

    fn cancel(&self, checkpoint: &Value) -> Result<()> {
        // `agent_task_lifecycle::cancel_run` owns both cancellation paths:
        // before materialization it terminates the detached child's process
        // tree under an exact `ProcessStartIdentity` match; after
        // materialization it marks the live attempt cancelled for the cook's
        // own supervisor to stop cleanly.
        let job = AgentTaskCookJob::parse(checkpoint.clone())?;
        if job.phase == AgentTaskCookJobPhase::Completed {
            return Ok(());
        }
        let run_id = job
            .request
            .pinned_retry_run_id
            .as_deref()
            .unwrap_or(&job.request.cook_id);
        agent_task_lifecycle::cancel_run(run_id, Some("controller job cancelled")).map(|_| ())
    }
}

impl CookWorkHandler {
    /// Watch a detached child to its durable terminal state.
    ///
    /// Liveness is judged by PID *and* kernel start identity. An
    /// `IdentityMismatch` or `Unverifiable` reading is treated as "no longer our
    /// child" rather than as death, so supervision can never attribute a
    /// stranger's process to this cook.
    fn supervise(&self, job: &mut AgentTaskCookJob, handle: WorkJobHandle) -> Result<Value> {
        job.phase = AgentTaskCookJobPhase::Supervising;
        handle.checkpoint(job.to_checkpoint()?)?;
        handle.progress(job.progress_projection())?;

        loop {
            // Cancellation is the daemon's to terminalize; this thread only
            // needs to stop supervising promptly so the supervisor can join it.
            if handle.is_cancelled() {
                return job.observe_terminal(None);
            }

            // Publish the attempt id exactly once, as soon as the cook has one.
            if job.run_id.is_none() {
                if let Some(run_id) = latest_run_id(&job.request.cook_id) {
                    job.run_id = Some(run_id);
                    handle.checkpoint(job.to_checkpoint()?)?;
                    handle.progress(job.progress_projection())?;
                }
            }

            // Runner ownership starts when the durable attempt records its job,
            // not when the launcher child exits. A detached Cook can retain a
            // provably-live launcher while its reverse worker has already
            // published a terminal broker result. Keep process identity as the
            // child-exit safety guard below, but let the daemon project the
            // runner authority throughout supervision.
            if let Some(run_id) = job.run_id.as_deref() {
                let lifecycle_store =
                    agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
                let mut record = lifecycle_store.read_record(run_id)?;
                if record.state.is_terminal() {
                    return job.observe_terminal(Some(record.run_id));
                }
                if record.runner_id().is_some() && record.runner_job_id().is_some() {
                    agent_task_lifecycle::reconcile_runner_job_state_in_store(
                        &lifecycle_store,
                        &mut record,
                    )?;
                    handle.checkpoint(job.to_checkpoint()?)?;
                    handle.progress(job.progress_projection())?;
                    if record.state.is_terminal() {
                        return job.observe_terminal(Some(record.run_id));
                    }
                }
            }

            if !child_is_live(&job.request) {
                return job.observe_terminal(job.run_id.clone());
            }

            std::thread::sleep(SUPERVISION_POLL);
        }
    }
}

/// What a resumed cook job must do, decided before any job handle is needed.
///
/// Extracted so the idempotency property can be asserted directly: no variant
/// here spawns provider work, and the enum is exhaustive over the states a
/// recovered checkpoint can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookJobResumeDisposition {
    /// The job already reached a durable terminal state. Replay it.
    AlreadyComplete,
    /// The supervised child is still provably the process we were handed.
    ReadoptLiveChild,
    /// The child is gone. Read the cook's durable outcome; never re-run it.
    ObserveTerminalOutcome,
}

impl AgentTaskCookJob {
    pub fn resume_disposition(&self) -> CookJobResumeDisposition {
        if self.phase == AgentTaskCookJobPhase::Completed {
            return CookJobResumeDisposition::AlreadyComplete;
        }
        if child_is_live(&self.request) {
            return CookJobResumeDisposition::ReadoptLiveChild;
        }
        CookJobResumeDisposition::ObserveTerminalOutcome
    }

    fn progress_projection(&self) -> Value {
        json!({
            "phase": self.phase,
            "cook_id": self.request.cook_id,
            "durable_run_id": self.request.cook_id,
            "run_id": self.run_id,
        })
    }

    /// Read the cook's durable terminal state and complete the job.
    ///
    /// A child that ended without ever publishing durable identity is reported
    /// as `Failed` rather than dressed up as a success, mirroring the launcher's
    /// existing `exited_before_handoff` honesty.
    fn observe_terminal(&mut self, run_id: Option<String>) -> Result<Value> {
        let run_id = run_id
            .or_else(|| self.request.pinned_retry_run_id.clone())
            .or_else(|| latest_run_id(&self.request.cook_id));
        if run_id.is_none() {
            agent_task_lifecycle::fail_detached_cook_handoff_parent(
                &self.request.cook_id,
                "detached Cook exited before materializing its first attempt",
            )?;
        } else if let Some(run_id) = run_id.as_deref() {
            let lifecycle_store =
                agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
            let record = lifecycle_store.read_record(run_id)?;
            if !record.state.is_terminal() {
                let provider_started = record
                    .metadata
                    .get("provider_executions")
                    .and_then(Value::as_array)
                    .is_some_and(|executions| !executions.is_empty());
                if self.request.supervisor_id.is_some() && !provider_started {
                    let plan = lifecycle_store.read_controller_plan(run_id)?;
                    let error = retry_child_failure(&self.request);
                    agent_task_lifecycle::record_pre_execution_failure_in_store(
                        &lifecycle_store,
                        run_id,
                        &plan,
                        "local_retry_supervisor",
                        &error,
                    )?;
                } else {
                    agent_task_lifecycle::record_interrupted_local_owner_in_store(
                        &lifecycle_store,
                        run_id,
                    )?;
                }
            }
        }
        let observed = run_id
            .as_deref()
            .or(Some(self.request.cook_id.as_str()))
            .and_then(|id| agent_task_lifecycle::exact_record(id).ok())
            .map(|record| record.state);
        self.run_id = run_id;
        self.terminal_state = Some(match observed {
            Some(state) if state.is_terminal() => state,
            // The child is gone but its record is not terminal: the run was
            // interrupted, not finished. Say so.
            _ => agent_task_lifecycle::AgentTaskRunState::Failed,
        });
        self.phase = AgentTaskCookJobPhase::Completed;
        self.completed_result()
    }
}

/// The attempt id the cook published, if it has reached durable submission.
fn latest_run_id(cook_id: &str) -> Option<String> {
    if !agent_task_lifecycle::cook_index_exists(cook_id).unwrap_or(false) {
        return None;
    }
    agent_task_lifecycle::cook_index(cook_id)
        .ok()
        .map(|index| index.latest_run_id)
}

/// Whether the supervised child is still provably the process we were handed.
fn child_is_live(request: &AgentTaskCookJobRequest) -> bool {
    matches!(
        process_identity_state_with_start_identity(
            request.child_pid,
            None,
            Some(&request.child_start_identity),
        ),
        ProcessIdentityState::Live
    )
}

fn retry_child_failure(request: &AgentTaskCookJobRequest) -> Error {
    let Some(reference) = request.child_session_ref.as_deref() else {
        return retry_child_diagnostics_unavailable("log_reference_missing", None);
    };
    let Ok(root) = homeboy_core::paths::homeboy_data() else {
        return retry_child_diagnostics_unavailable("data_root_unavailable", Some(reference));
    };
    let path = root
        .join("agent-task-detached")
        .join(reference)
        .join("cook-retry.log");
    let path = path.display().to_string();
    let mut bytes = Vec::with_capacity(CHILD_RESULT_LOG_LIMIT + 1);
    let read = std::fs::File::open(&path)
        .and_then(|file| {
            file.take((CHILD_RESULT_LOG_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| error.kind().to_string());
    if let Err(reason) = read {
        return retry_child_diagnostics_unavailable(&reason, Some(reference));
    }
    if bytes.len() > CHILD_RESULT_LOG_LIMIT {
        return retry_child_diagnostics_unavailable("log_exceeds_bound", Some(reference));
    }
    let value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return retry_child_diagnostics_unavailable("malformed_command_result", Some(reference))
        }
    };
    if value["schema"] != "homeboy/command-result/v3" || value["success"] != Value::Bool(false) {
        return retry_child_diagnostics_unavailable("no_failed_command_result", Some(reference));
    }
    let diagnostic = value
        .get("error")
        .or_else(|| value.get("diagnostics"))
        .or_else(|| value.pointer("/data/failure_context/diagnostic"));
    let Some(diagnostic) = diagnostic.filter(|diagnostic| diagnostic.is_object()) else {
        return retry_child_diagnostics_unavailable("no_typed_diagnostic", Some(reference));
    };
    let Some(code) = diagnostic.get("code").and_then(Value::as_str) else {
        return retry_child_diagnostics_unavailable(
            "typed_diagnostic_missing_code",
            Some(reference),
        );
    };
    let Some(message) = diagnostic.get("message").and_then(Value::as_str) else {
        return retry_child_diagnostics_unavailable(
            "typed_diagnostic_missing_message",
            Some(reference),
        );
    };

    let mut evidence = json!({
        "schema": value.get("schema"),
        "command": value.get("command"),
        "operation": value.get("operation"),
        "status": value.get("status"),
        "diagnostic": diagnostic,
        "failure_context": value.pointer("/data/failure_context"),
        "next_actions": value.get("next_actions"),
        "artifacts": value.get("artifacts"),
        "evidence": value.get("evidence"),
        "child_result_evidence": {
            "kind": "detached-child-command-result",
            "source_session_ref": reference,
            "evidence_uri": request.pinned_retry_run_id.as_deref().map(|run_id| format!("homeboy://agent-task/run/{run_id}/status#detached-child-command-result")),
        },
    });
    evidence = homeboy_core::redaction::redact_json(&evidence);
    bound_child_diagnostic(&mut evidence, 0);
    let classification = diagnostic
        .pointer("/details/classification")
        .or_else(|| value.pointer("/data/failure_context/classification"))
        .or_else(|| value.pointer("/data/terminal_failure_classification"))
        .and_then(Value::as_str);
    let mut error = Error::new(
        child_error_code(code),
        homeboy_core::redaction::redact_string(message),
        json!({
            "field": code,
            "child_reported_error_code": code,
            "child_error_code": code,
            "child_failure_classification": classification,
            "child_command_result": evidence,
        }),
    )
    .with_retryable(true);
    if let Some(hints) = diagnostic.get("hints").and_then(Value::as_array) {
        for hint in hints
            .iter()
            .filter_map(|hint| hint.get("message").and_then(Value::as_str))
            .take(4)
        {
            error = error.with_hint(homeboy_core::redaction::redact_string(hint));
        }
    }
    error
}

fn retry_child_diagnostics_unavailable(reason: &str, session_ref: Option<&str>) -> Error {
    Error::new(
        ErrorCode::InternalUnexpected,
        "local Cook retry launcher exited before provider execution",
        json!({
            "field": "local_retry_supervisor",
            "child_diagnostics": {
                "status": "unavailable",
                "reason": reason,
                "child_result_evidence": session_ref.map(|reference| json!({ "source_session_ref": reference })),
            },
        }),
    )
    .with_retryable(true)
}

fn child_error_code(code: &str) -> ErrorCode {
    match code {
        "storage.exhausted" => ErrorCode::StorageExhausted,
        _ => ErrorCode::InternalUnexpected,
    }
}

fn bound_child_diagnostic(value: &mut Value, depth: usize) {
    const TEXT_LIMIT: usize = 2048;
    if depth >= 4 {
        *value = Value::String("[omitted: diagnostic depth limit]".to_string());
        return;
    }
    match value {
        Value::String(text) if text.len() > TEXT_LIMIT => {
            text.truncate(TEXT_LIMIT);
            text.push_str("...[truncated]");
        }
        Value::Array(values) => {
            values.truncate(8);
            for value in values {
                bound_child_diagnostic(value, depth + 1);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                bound_child_diagnostic(value, depth + 1);
            }
        }
        _ => {}
    }
}

/// Register the cook handler with the generic work lifecycle.
/// Registration is idempotent because CLI startup can run in test processes
/// that initialize the command runtime more than once.
pub fn register_cook_work_handler() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        register_work_job_handler(std::sync::Arc::new(CookWorkHandler))
            .expect("register cook work job handler");
    });
}

/// Build the durable submit payload for one detached cook.
///
/// Lives here rather than in the launcher so the wire shape and the driver that
/// parses it cannot drift apart.
pub fn cook_job_submission(
    cook_id: &str,
    child_pid: u32,
    child_start_identity: &ProcessStartIdentity,
) -> Result<Value> {
    let job = AgentTaskCookJob::new(AgentTaskCookJobRequest {
        schema: AGENT_TASK_COOK_JOB_SCHEMA.to_string(),
        cook_id: cook_id.to_string(),
        child_pid,
        child_start_identity: child_start_identity.clone(),
        supervisor_id: None,
        pinned_retry_run_id: None,
        child_session_ref: None,
    })?;
    work_job_submission(
        &CookWorkHandler,
        job.idempotency_key.clone(),
        job.to_checkpoint()?,
    )
}

/// Build a retry-specific supervisor submission. The Cook alias remains the
/// cancellation target while the retry run is the one-shot supervisor owner.
pub fn cook_retry_job_submission(
    cook_id: &str,
    run_id: &str,
    child_pid: u32,
    child_start_identity: &ProcessStartIdentity,
    child_session_ref: &str,
) -> Result<Value> {
    let job = AgentTaskCookJob::new(AgentTaskCookJobRequest {
        schema: AGENT_TASK_COOK_JOB_SCHEMA.to_string(),
        cook_id: cook_id.to_string(),
        child_pid,
        child_start_identity: child_start_identity.clone(),
        supervisor_id: Some(run_id.to_string()),
        pinned_retry_run_id: Some(run_id.to_string()),
        child_session_ref: Some(child_session_ref.to_string()),
    })?;
    work_job_submission(
        &CookWorkHandler,
        job.idempotency_key.clone(),
        job.to_checkpoint()?,
    )
}

#[cfg(test)]
mod tests {

    /// Tests are the entry point for their own unit of work, so the store
    /// resolves once here (#7505).
    fn test_lifecycle_store() -> crate::agent_task_lifecycle::AgentTaskLifecycleStore {
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("lifecycle store")
    }
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::agent_task_service::work_job::{
        WorkJobDriver, WORK_JOB_CHECKPOINT_SCHEMA, WORK_JOB_PROGRESS_SCHEMA,
        WORK_JOB_RESULT_SCHEMA, WORK_JOB_TYPE, WORK_JOB_VERSION,
    };
    use homeboy_core::api_jobs::JobEventKind;
    use homeboy_core::daemon::controller_job_driver::ControllerJobDriver;
    use homeboy_core::test_support::{with_isolated_home, ControllerJobHarness};
    use std::sync::Arc;

    const IDENTITY: ProcessStartIdentity = ProcessStartIdentity::Linux {
        starttime_ticks: 4242,
    };

    fn submission(cook_id: &str, pid: u32) -> Value {
        register_cook_work_handler();
        cook_job_submission(cook_id, pid, &IDENTITY).expect("build cook job submission")
    }

    #[test]
    fn retry_supervisors_are_owned_by_the_exact_retry_run() {
        let first = cook_retry_job_submission(
            "cook-retry",
            "cook-retry-attempt-2",
            1,
            &IDENTITY,
            "retry-session",
        )
        .expect("first retry submission");
        let replay = cook_retry_job_submission(
            "cook-retry",
            "cook-retry-attempt-2",
            2,
            &IDENTITY,
            "retry-session",
        )
        .expect("replayed retry submission");
        let successor = cook_retry_job_submission(
            "cook-retry",
            "cook-retry-attempt-3",
            3,
            &IDENTITY,
            "retry-session",
        )
        .expect("successor retry submission");

        assert_eq!(
            first["idempotency_key"],
            "agent-task-cook:cook-retry-attempt-2"
        );
        assert_eq!(first["idempotency_key"], replay["idempotency_key"]);
        assert_ne!(first["idempotency_key"], successor["idempotency_key"]);
        let first_job =
            AgentTaskCookJob::parse(first["request"]["request"].clone()).expect("first job");
        let successor_job = AgentTaskCookJob::parse(successor["request"]["request"].clone())
            .expect("successor job");
        assert_eq!(first_job.run_id.as_deref(), Some("cook-retry-attempt-2"));
        assert_eq!(
            successor_job.run_id.as_deref(),
            Some("cook-retry-attempt-3")
        );
        assert_ne!(first_job.run_id, successor_job.run_id);
        assert_eq!(
            first_job.request.pinned_retry_run_id.as_deref(),
            first_job.run_id.as_deref()
        );
    }

    fn request_of(cook_id: &str, pid: u32) -> Value {
        submission(cook_id, pid)
            .get("request")
            .and_then(|request| request.get("request"))
            .cloned()
            .expect("submission carries a request")
    }

    fn work_request_of(cook_id: &str, pid: u32) -> Value {
        submission(cook_id, pid)
            .get("request")
            .cloned()
            .expect("submission carries a work request")
    }

    /// The wire payload the launcher sends must be exactly what the driver
    /// admits, or detachment silently stops being daemon-owned.
    #[test]
    fn the_submission_round_trips_through_the_driver() {
        let submission = submission("cook-round-trip", 4242);

        assert_eq!(submission["type"], WORK_JOB_TYPE);
        assert_eq!(submission["version"], WORK_JOB_VERSION);
        assert_eq!(
            submission["idempotency_key"],
            "agent-task-cook:cook-round-trip"
        );
        assert_eq!(submission["request"]["work_type"], AGENT_TASK_COOK_JOB_TYPE);
        assert_eq!(
            submission["request"]["work_version"],
            AGENT_TASK_COOK_JOB_VERSION
        );

        let job = AgentTaskCookJob::parse(submission["request"]["request"].clone())
            .expect("parse request");
        assert_eq!(job.request.cook_id, "cook-round-trip");
        assert_eq!(job.request.child_pid, 4242);
        assert_eq!(job.request.child_start_identity, IDENTITY);
        assert_eq!(job.phase, AgentTaskCookJobPhase::Queued);
        assert_eq!(job.run_id, None);
        assert_eq!(job.terminal_state, None);

        let driver = WorkJobDriver;
        driver
            .validate_secret_references(&submission["request"])
            .expect("a reference-only request validates");
        driver
            .public_request(&submission["request"])
            .expect("public projection");
    }

    /// The cook id is the durable identity of this work, so a replayed submit
    /// must converge on one supervisor rather than spawn a second.
    #[test]
    fn the_idempotency_key_is_the_cook_id() {
        assert_eq!(
            submission("cook-same", 1)["idempotency_key"],
            submission("cook-same", 2)["idempotency_key"],
        );
        assert_ne!(
            submission("cook-a", 1)["idempotency_key"],
            submission("cook-b", 1)["idempotency_key"],
        );
    }

    /// The prompt, provider invocation, notification route and worktree are all
    /// sensitive and none of them belong in a job whose request is a reference.
    /// `deny_unknown_fields` is what enforces that, so prove it rejects.
    #[test]
    fn inline_secrets_are_refused_rather_than_carried() {
        let driver = WorkJobDriver;
        for smuggled in [
            json!({ "prompt": "the private task text" }),
            json!({ "env": { "ANTHROPIC_API_KEY": "sk-live-secret" } }),
            json!({ "provider_invocation": { "command": "claude --token sk-live" } }),
            json!({ "notification_route": "opaque-destination" }),
            json!({ "launcher_log": "/home/operator/.homeboy/cook.log" }),
            json!({ "child_session_ref": "/home/operator/.homeboy/private" }),
        ] {
            let mut request = work_request_of("cook-secrets", 4242);
            let object = request.as_object_mut().expect("request is an object");
            let object = object
                .get_mut("request")
                .and_then(Value::as_object_mut)
                .expect("domain request is an object");
            for (key, value) in smuggled.as_object().expect("smuggled object") {
                object.insert(key.clone(), value.clone());
            }

            assert!(
                driver.validate_secret_references(&request).is_err(),
                "an inline secret must be refused: {smuggled}"
            );
        }
    }

    /// Even a well-formed job must not project anything a reader could use to
    /// locate the operator's filesystem or the child's kernel identity.
    #[test]
    fn public_projections_withhold_paths_and_process_identity() {
        let driver = WorkJobDriver;
        let mut job =
            AgentTaskCookJob::parse(request_of("cook-public", 4242)).expect("parse request");
        job.phase = AgentTaskCookJobPhase::Completed;
        job.run_id = Some("cook-public-attempt-1".to_string());
        job.terminal_state = Some(agent_task_lifecycle::AgentTaskRunState::Succeeded);
        let mut value = work_request_of("cook-public", 4242);
        value["request"] = job.to_checkpoint().expect("serialize job");

        let public = driver.public_request(&value).expect("public request");
        let public_text = public.to_string();
        assert!(!public_text.contains("4242"), "{public_text}");
        assert!(!public_text.contains("starttime_ticks"), "{public_text}");
        assert!(!public_text.contains("child_pid"), "{public_text}");
        assert_eq!(public["cook_id"], "cook-public");
        assert_eq!(public["run_id"], "cook-public-attempt-1");
        assert_eq!(public["terminal_state"], "succeeded");

        // Progress and result are projected field by field, so a private field
        // added to either payload later cannot escape by default.
        let private = json!({
            "phase": "supervising",
            "cook_id": "cook-public",
            "run_id": "cook-public-attempt-1",
            "prompt": "the private task text",
        });
        let progress = driver
            .public_progress(&json!({
                "schema": WORK_JOB_PROGRESS_SCHEMA,
                "work_type": AGENT_TASK_COOK_JOB_TYPE,
                "work_version": AGENT_TASK_COOK_JOB_VERSION,
                "progress": private.clone(),
            }))
            .expect("public progress");
        assert!(!progress.to_string().contains("private task text"));
        let result = driver
            .public_result(&json!({
                "schema": WORK_JOB_RESULT_SCHEMA,
                "work_type": AGENT_TASK_COOK_JOB_TYPE,
                "work_version": AGENT_TASK_COOK_JOB_VERSION,
                "result": private,
            }))
            .expect("public result");
        assert!(!result.to_string().contains("private task text"));
    }

    /// A cook's error text can quote provider output, which can quote the
    /// prompt. Only the typed code may cross into public job state.
    #[test]
    fn the_public_error_carries_only_a_code() {
        let public =
            CookWorkHandler.public_error(&invalid_cook_job("prompt: the private task text"));

        assert_eq!(public.message, "controller-owned cook supervision failed");
        assert!(!public.data.to_string().contains("private task text"));
    }

    /// The single most important correctness property: a daemon restart must
    /// not be able to run an attempt twice. No disposition spawns work, and a
    /// finished job replays rather than re-executes.
    #[test]
    fn resume_never_re_runs_a_completed_job() {
        let mut job =
            AgentTaskCookJob::parse(request_of("cook-complete", 4242)).expect("parse request");
        job.phase = AgentTaskCookJobPhase::Completed;
        job.run_id = Some("cook-complete-attempt-1".to_string());
        job.terminal_state = Some(agent_task_lifecycle::AgentTaskRunState::Succeeded);

        assert_eq!(
            job.resume_disposition(),
            CookJobResumeDisposition::AlreadyComplete
        );
        // Replay is byte-identical however many times recovery happens.
        let first = job.completed_result().expect("first replay");
        let second = job.completed_result().expect("second replay");
        assert_eq!(first, second);
        assert_eq!(first["terminal_state"], "succeeded");
        assert_eq!(first["run_id"], "cook-complete-attempt-1");
    }

    /// A checkpoint whose child is gone resolves to observation, never to a
    /// re-execution. PID 0 is never live, so this is deterministic.
    #[test]
    fn resume_observes_rather_than_restarts_a_dead_child() {
        let mut job =
            AgentTaskCookJob::parse(request_of("cook-dead-child", 4242)).expect("parse request");
        job.phase = AgentTaskCookJobPhase::Supervising;
        // u32::MAX is not a live pid, and the recorded start identity cannot
        // match, so liveness is provably false.
        job.request.child_pid = u32::MAX;

        assert_eq!(
            job.resume_disposition(),
            CookJobResumeDisposition::ObserveTerminalOutcome
        );
    }

    #[test]
    fn execute_supervises_through_the_controller_job_handle() {
        with_isolated_home(|_| {
            let cook_id = "cook-controller-harness";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            let request = work_request_of(cook_id, u32::MAX);
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct controller job harness");
            let prepared = driver.prepare(request).expect("prepare cook job");

            let result = driver
                .execute(prepared, harness.handle())
                .expect("supervise dead child to durable outcome");

            assert_eq!(result["result"]["terminal_state"], "failed");
            assert_eq!(result["result"]["phase"], "completed");
            let checkpoint = harness
                .checkpoint()
                .expect("read checkpoint")
                .expect("supervision checkpoint");
            assert_eq!(checkpoint["schema"], WORK_JOB_CHECKPOINT_SCHEMA);
            assert_eq!(checkpoint["work_type"], AGENT_TASK_COOK_JOB_TYPE);
            assert_eq!(checkpoint["checkpoint"]["phase"], "supervising");
            let progress = harness
                .events()
                .expect("read controller events")
                .into_iter()
                .find(|event| event.kind == JobEventKind::Progress)
                .and_then(|event| event.data)
                .expect("projected supervision progress");
            assert_eq!(progress["phase"], "supervising");
            assert_eq!(progress["cook_id"], cook_id);
            assert!(progress.get("durable_run_id").is_none());
        });
    }

    #[test]
    fn resume_supervises_then_replays_a_completed_checkpoint_idempotently() {
        with_isolated_home(|_| {
            let cook_id = "cook-controller-resume-harness";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            let request = work_request_of(cook_id, u32::MAX);
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct controller job harness");
            let supervising = driver.prepare(request).expect("prepare cook job");

            let observed = driver
                .resume(supervising.clone(), harness.handle())
                .expect("resume supervision of dead child");

            assert_eq!(observed["result"]["phase"], "completed");
            assert_eq!(observed["result"]["terminal_state"], "failed");
            let mut completed = supervising;
            completed["checkpoint"]["phase"] = json!("completed");
            completed["checkpoint"]["terminal_state"] = json!("failed");
            let first = driver
                .resume(completed.clone(), harness.handle())
                .expect("first completed replay");
            let second = driver
                .resume(completed, harness.handle())
                .expect("second completed replay");
            assert_eq!(first, second);
            assert_eq!(first, observed);
        });
    }

    #[test]
    fn cancellation_requested_through_the_harness_stops_supervision() {
        with_isolated_home(|_| {
            let cook_id = "cook-controller-cancel-harness";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            let request = work_request_of(cook_id, u32::MAX);
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct controller job harness");
            harness
                .request_cancellation("test cancellation")
                .expect("request cancellation");
            let handle = harness.handle();
            assert!(handle.is_cancelled());

            let result = driver
                .execute(driver.prepare(request).expect("prepare cook job"), handle)
                .expect("stop supervision after cancellation");

            assert_eq!(result["result"]["phase"], "completed");
            assert_eq!(result["result"]["terminal_state"], "failed");
        });
    }

    /// A child that ended without ever publishing durable identity is not a
    /// success. Reporting it as one would reproduce the dishonesty detachment
    /// exists to remove.
    #[test]
    fn an_unfinished_cook_terminalizes_as_failed() {
        with_isolated_home(|_| {
            let cook_id = "cook-never-submitted";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            let mut job =
                AgentTaskCookJob::parse(request_of(cook_id, 4242)).expect("parse request");
            job.phase = AgentTaskCookJobPhase::Supervising;

            let result = job.observe_terminal(None).expect("observe terminal");

            assert_eq!(job.phase, AgentTaskCookJobPhase::Completed);
            assert_eq!(result["terminal_state"], "failed");
            let parent = agent_task_lifecycle::exact_record(cook_id)
                .expect("read terminalized handoff parent");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["state"],
                "exited_before_handoff"
            );
        });
    }

    /// Cancelling a finished job is a no-op rather than an error, so a
    /// cancellation racing completion cannot fail the durable job.
    #[test]
    fn cancelling_a_completed_job_is_a_no_op() {
        let mut job =
            AgentTaskCookJob::parse(request_of("cook-cancel-complete", 4242)).expect("parse");
        job.phase = AgentTaskCookJobPhase::Completed;
        job.terminal_state = Some(agent_task_lifecycle::AgentTaskRunState::Succeeded);

        CookWorkHandler
            .cancel(&job.to_checkpoint().expect("serialize"))
            .expect("cancelling a completed cook job is a no-op");
    }

    /// Cancellation must actually stop the provider. Before an attempt exists
    /// the detached child is the containment owner, so terminating its tree is
    /// the stop — and this proves the driver reaches that established path
    /// rather than only marking durable state.
    #[test]
    fn cancelling_a_supervised_job_terminates_the_detached_child() {
        with_isolated_home(|_| {
            let cook_id = "cook-driver-cancel";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            let child = std::process::Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn detached cook fixture");
            let start_identity = homeboy_core::process::process_start_identity(child.id())
                .expect("inspect fixture")
                .expect("fixture has a start identity");
            agent_task_lifecycle::record_detached_cook_handoff_child_in_store(
                &test_lifecycle_store(),
                cook_id,
                child.id(),
                start_identity.clone(),
            )
            .expect("persist detached child identity");

            let submission = cook_job_submission(cook_id, child.id(), &start_identity)
                .expect("build submission");

            CookWorkHandler
                .cancel(&submission["request"]["request"])
                .expect("driver cancellation reaches the lifecycle stop path");

            assert!(
                matches!(
                    homeboy_core::process::process_identity_state(child.id(), None),
                    homeboy_core::process::ProcessIdentityState::Dead
                ),
                "a cancelled cook job must leave no live child"
            );
        });
    }

    #[test]
    fn cancelling_a_retry_supervisor_targets_its_pinned_attempt_not_the_latest() {
        with_isolated_home(|_| {
            let cook_id = "cook-retry-cancel";
            let plan = crate::agent_task_scheduler::AgentTaskPlan::new("retry-cancel", Vec::new());
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            for (attempt, run_id) in [
                (1, "cook-retry-cancel-attempt-1"),
                (2, "cook-retry-cancel-attempt-2"),
                (3, "cook-retry-cancel-attempt-3"),
            ] {
                agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist attempt");
                agent_task_lifecycle::record_cook_attempt_in_store(
                    &test_lifecycle_store(),
                    cook_id,
                    attempt,
                    run_id,
                )
                .expect("index attempt");
            }

            let submission = cook_retry_job_submission(
                cook_id,
                "cook-retry-cancel-attempt-2",
                4242,
                &IDENTITY,
                "retry-session",
            )
            .expect("build retry supervisor submission");
            CookWorkHandler
                .cancel(&submission["request"]["request"])
                .expect("cancel pinned retry supervisor");

            assert_eq!(
                agent_task_lifecycle::exact_record("cook-retry-cancel-attempt-2")
                    .expect("read cancelled pinned attempt")
                    .state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(
                agent_task_lifecycle::exact_record("cook-retry-cancel-attempt-3")
                    .expect("read latest attempt")
                    .state,
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
        });
    }

    #[test]
    fn work_handler_registration_is_idempotent() {
        register_cook_work_handler();
        register_cook_work_handler();
    }

    #[test]
    fn retry_supervisor_projects_a_bounded_redacted_child_failure() {
        with_isolated_home(|_| {
            let cook_id = "cook-retry-child-diagnostic";
            let run_id = "cook-retry-child-diagnostic-attempt-1";
            let plan = crate::agent_task_scheduler::AgentTaskPlan::new(
                "retry-child",
                vec![AgentTaskRequest {
                    schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "retry-child-task".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: AgentTaskExecutor {
                        backend: "test".to_string(),
                        selector: None,
                        runtime_selection: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: None,
                        config: Value::Null,
                    },
                    instructions: "preserve child diagnostics".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: AgentTaskWorkspace::default(),
                    component_contracts: Vec::new(),
                    policy: AgentTaskPolicy::default(),
                    limits: AgentTaskLimits::default(),
                    expected_artifacts: Vec::new(),
                    artifact_declarations: Vec::new(),
                    output_declarations: Vec::new(),
                    runtime_tools: Vec::new(),
                    metadata: Value::Null,
                }],
            );
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist retry run");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                run_id,
            )
            .expect("index retry run");
            let session_ref = "retry-child-diagnostic-session";
            let session = homeboy_core::paths::homeboy_data()
                .expect("resolve data root")
                .join("agent-task-detached")
                .join(session_ref);
            std::fs::create_dir_all(&session).expect("create child session");
            let log = session.join("cook-retry.log");
            std::fs::write(
                &log,
                r#"{"schema":"homeboy/command-result/v3","command":"agent-task","operation":"retry","success":false,"status":"failed","data":{"failure_context":{"diagnostic":{"code":"validation.invalid_argument","message":"Invalid retry fixture","details":{"token":"secret-value"}},"next_actions":[{"action":"repair","command":"homeboy repair"}]},"terminal_failure_classification":"invalid_input"}}"#,
            )
            .expect("write child result");
            let mut job = AgentTaskCookJob::new(AgentTaskCookJobRequest {
                schema: AGENT_TASK_COOK_JOB_SCHEMA.to_string(),
                cook_id: cook_id.to_string(),
                child_pid: u32::MAX,
                child_start_identity: IDENTITY,
                supervisor_id: Some(run_id.to_string()),
                pinned_retry_run_id: Some(run_id.to_string()),
                child_session_ref: Some(session_ref.to_string()),
            })
            .expect("create retry supervisor");

            job.observe_terminal(Some(run_id.to_string()))
                .expect("observe child failure");

            let record = agent_task_lifecycle::exact_record(run_id).expect("read failed retry");
            let failure = &record.metadata["pre_execution_failure"];
            assert_eq!(failure["error_code"], "validation.invalid_argument");
            assert_eq!(failure["message"], "Invalid retry fixture");
            assert_eq!(failure["failure_code"], "validation.invalid_argument");
            assert_eq!(
                failure["details"]["child_failure_classification"],
                "invalid_input"
            );
            assert_eq!(
                failure["details"]["child_command_result"]["child_result_evidence"]["evidence_uri"],
                format!("homeboy://agent-task/run/{run_id}/status#detached-child-command-result")
            );
            assert!(!failure.to_string().contains("secret-value"));
            assert_eq!(failure["provider_executions_consumed"], 0);
            assert_eq!(failure["retryable"], true);
            let aggregate = test_lifecycle_store()
                .read_aggregate(run_id)
                .expect("read failure aggregate");
            assert!(aggregate.outcomes[0].evidence_refs.iter().any(|reference| {
                reference.kind == "detached-child-command-result"
                    && reference.uri
                        == format!("homeboy://agent-task/run/{run_id}/status#detached-child-command-result")
            }));
        });
    }

    #[cfg(unix)]
    #[test]
    fn observe_terminal_persists_interrupted_owner_aggregate_during_provider_execution() {
        with_isolated_home(|_| {
            let cook_id = "cook-interrupted-observer";
            let run_id = "cook-interrupted-observer-attempt-1";
            let plan = crate::agent_task_scheduler::AgentTaskPlan::new(
                "interrupted-observer",
                vec![AgentTaskRequest {
                    schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "task-a".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: AgentTaskExecutor {
                        backend: "test".to_string(),
                        selector: Some("fixture".to_string()),
                        runtime_selection: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: None,
                        config: Value::Null,
                    },
                    instructions: "run".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: AgentTaskWorkspace::default(),
                    component_contracts: Vec::new(),
                    policy: AgentTaskPolicy::default(),
                    limits: AgentTaskLimits::default(),
                    expected_artifacts: Vec::new(),
                    artifact_declarations: Vec::new(),
                    output_declarations: Vec::new(),
                    runtime_tools: Vec::new(),
                    metadata: Value::Null,
                }],
            );
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist attempt");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                run_id,
            )
            .expect("index attempt");
            agent_task_lifecycle::mark_running(run_id).expect("running");
            agent_task_lifecycle::reserve_provider_execution_in_store(
                &test_lifecycle_store(),
                run_id,
                &plan.tasks[0],
                1,
            )
            .expect("reserved");
            let mut owner = std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .expect("start cook observer");
            agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata["provider_executions"][0]["owner_pid"] = json!(owner.id());
                record.metadata["provider_executions"][0]["owner_linux_starttime_ticks"] =
                    Value::Null;
            })
            .expect("observer fixture");
            owner.kill().expect("kill cook observer");
            owner.wait().expect("reap cook observer");

            let mut job =
                AgentTaskCookJob::parse(request_of(cook_id, u32::MAX)).expect("parse request");
            job.phase = AgentTaskCookJobPhase::Supervising;
            job.observe_terminal(Some(run_id.to_string()))
                .expect("observe interrupted observer");

            let record = agent_task_lifecycle::exact_record(run_id).expect("read interrupted run");
            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(
                record.metadata["stop_reason"],
                json!("local Cook observer was interrupted during provider execution")
            );
            assert_eq!(
                record.metadata["terminal_failure_classification"],
                json!("interrupted_owner")
            );
            assert_eq!(
                record.metadata["interrupted_owner"]["provider_budget_consumed"],
                true
            );
            let aggregate = test_lifecycle_store()
                .read_aggregate(run_id)
                .expect("read interrupted-owner aggregate");
            assert_eq!(
                aggregate.outcomes[0].diagnostics[0].class,
                "interrupted_owner"
            );
        });
    }

    #[test]
    fn retry_supervisor_records_unavailable_child_diagnostics() {
        with_isolated_home(|_| {
            let error = retry_child_diagnostics_unavailable(
                "child_log_unreadable",
                Some("missing-session"),
            );

            assert_eq!(error.code, ErrorCode::InternalUnexpected);
            assert_eq!(error.details["child_diagnostics"]["status"], "unavailable");
            assert_eq!(
                error.details["child_diagnostics"]["child_result_evidence"]["source_session_ref"],
                "missing-session"
            );
            assert_eq!(error.retryable, Some(true));
        });
    }
}

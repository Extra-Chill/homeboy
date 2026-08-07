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
use std::time::Duration;

use homeboy_core::daemon::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobHandle, ControllerJobPublicError,
};
use homeboy_core::process::{
    process_identity_state_with_start_identity, ProcessIdentityState, ProcessStartIdentity,
};
use homeboy_core::Result;

use crate::agent_task_lifecycle;

pub const AGENT_TASK_COOK_JOB_TYPE: &str = "agent-task-cook";
pub const AGENT_TASK_COOK_JOB_VERSION: u32 = 1;
const AGENT_TASK_COOK_JOB_SCHEMA: &str = "homeboy/agent-task-cook-job/v1";

/// How often supervision re-reads durable cook state and child liveness.
const SUPERVISION_POLL: Duration = Duration::from_millis(250);

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
        Ok(Self {
            schema: AGENT_TASK_COOK_JOB_SCHEMA.to_string(),
            // The cook id is already unique and is the durable identity of this
            // work, so replaying a submit converges on one job rather than
            // creating a second supervisor for the same child.
            idempotency_key: format!("agent-task-cook:{}", request.cook_id),
            request,
            phase: AgentTaskCookJobPhase::Queued,
            run_id: None,
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
            "run_id": self.run_id,
            "terminal_state": self.terminal_state,
        })
    }
}

fn invalid_cook_job(message: &str) -> homeboy_core::Error {
    homeboy_core::Error::validation_invalid_argument("cook_job", message, None, None)
}

pub struct CookJobDriver;

impl ControllerJobDriver for CookJobDriver {
    fn job_type(&self) -> &'static str {
        AGENT_TASK_COOK_JOB_TYPE
    }

    fn version(&self) -> u32 {
        AGENT_TASK_COOK_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(AgentTaskCookJob::parse(request.clone())?.public_projection())
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        // Progress is projected field by field rather than passed through, so a
        // future private progress field cannot reach the public log by default.
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
        // A cook's error text can quote provider output, which can quote the
        // prompt. Only the typed code crosses into public job state.
        ControllerJobPublicError {
            message: "controller-owned cook supervision failed".to_string(),
            data: json!({ "code": format!("{:?}", error.code) }),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        // `deny_unknown_fields` on both the job and its request makes any inline
        // secret — a prompt, an env block, a provider invocation, a token — a
        // parse failure rather than an accepted field.
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

    fn execute(&self, prepared: Value, handle: ControllerJobHandle) -> Result<Value> {
        let mut job = AgentTaskCookJob::parse(prepared)?;
        self.supervise(&mut job, handle)
    }

    /// Re-adopt supervision after a daemon restart.
    ///
    /// This is idempotent by construction: no branch below spawns provider work.
    /// A completed job short-circuits on its durable terminal state; an
    /// unfinished one either re-attaches to a child still provably alive, or
    /// observes the durable outcome of one that is not.
    fn resume(&self, checkpoint: Value, handle: ControllerJobHandle) -> Result<Value> {
        let mut job = AgentTaskCookJob::parse(checkpoint)?;
        if job.phase == AgentTaskCookJobPhase::Completed {
            return job.completed_result();
        }
        self.supervise(&mut job, handle)
    }

    /// Stop the cook through the one established cancellation path.
    ///
    /// `agent_task_lifecycle::cancel_run` already owns both halves of this:
    /// before the cook materializes an attempt it terminates the detached
    /// child's process tree, guarded by an exact `ProcessStartIdentity` match so
    /// a reused PID is never signalled; after materialization it follows the
    /// Cook alias to the live attempt and marks it cancelled, which the running
    /// cook's own supervisor observes and turns into
    /// `terminate_process_tree(std::process::id())` — the in-flight stop path.
    /// No second mechanism is introduced here.
    fn cancel(&self, prepared: &Value) -> Result<()> {
        let job = AgentTaskCookJob::parse(prepared.clone())?;
        if job.phase == AgentTaskCookJobPhase::Completed {
            return Ok(());
        }
        agent_task_lifecycle::cancel_run(&job.request.cook_id, Some("controller job cancelled"))
            .map(|_| ())
    }
}

impl CookJobDriver {
    /// Watch a detached child to its durable terminal state.
    ///
    /// Liveness is judged by PID *and* kernel start identity. An
    /// `IdentityMismatch` or `Unverifiable` reading is treated as "no longer our
    /// child" rather than as death, so supervision can never attribute a
    /// stranger's process to this cook.
    fn supervise(&self, job: &mut AgentTaskCookJob, handle: ControllerJobHandle) -> Result<Value> {
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

            if !child_is_live(&job.request) {
                return job.observe_terminal(job.run_id.clone());
            }

            std::thread::sleep(SUPERVISION_POLL);
        }
    }
}

impl AgentTaskCookJob {
    fn progress_projection(&self) -> Value {
        json!({
            "phase": self.phase,
            "cook_id": self.request.cook_id,
            "run_id": self.run_id,
        })
    }

    /// Read the cook's durable terminal state and complete the job.
    ///
    /// A child that ended without ever publishing durable identity is reported
    /// as `Failed` rather than dressed up as a success, mirroring the launcher's
    /// existing `exited_before_handoff` honesty.
    fn observe_terminal(&mut self, run_id: Option<String>) -> Result<Value> {
        let run_id = run_id.or_else(|| latest_run_id(&self.request.cook_id));
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

/// Register the cook driver with core's generic controller-job lifecycle.
/// Registration is idempotent because CLI startup can run in test processes
/// that initialize the command runtime more than once.
pub fn register_cook_job_driver() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        controller_job_driver::register_controller_job_driver(std::sync::Arc::new(CookJobDriver))
            .expect("register cook controller job driver");
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
    })?;
    Ok(json!({
        "type": AGENT_TASK_COOK_JOB_TYPE,
        "version": AGENT_TASK_COOK_JOB_VERSION,
        "idempotency_key": job.idempotency_key,
        "request": job.to_checkpoint()?,
    }))
}

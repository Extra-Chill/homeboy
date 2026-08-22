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
        match job.resume_disposition() {
            // Replaying a finished job reports its durable result and touches
            // nothing. This is what makes repeated recovery safe.
            CookJobResumeDisposition::AlreadyComplete => job.completed_result(),
            // Neither branch spawns anything: supervision either re-attaches to
            // a child that is still provably ours, or observes on its first
            // iteration that the child is gone and terminalizes from durable
            // state.
            CookJobResumeDisposition::ReadoptLiveChild
            | CookJobResumeDisposition::ObserveTerminalOutcome => self.supervise(&mut job, handle),
        }
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
        if run_id.is_none() {
            agent_task_lifecycle::fail_detached_cook_handoff_parent(
                &self.request.cook_id,
                "detached Cook exited before materializing its first attempt",
            )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::test_support::with_isolated_home;

    const IDENTITY: ProcessStartIdentity = ProcessStartIdentity::Linux {
        starttime_ticks: 4242,
    };

    fn submission(cook_id: &str, pid: u32) -> Value {
        cook_job_submission(cook_id, pid, &IDENTITY).expect("build cook job submission")
    }

    fn request_of(cook_id: &str, pid: u32) -> Value {
        submission(cook_id, pid)
            .get("request")
            .cloned()
            .expect("submission carries a request")
    }

    /// The wire payload the launcher sends must be exactly what the driver
    /// admits, or detachment silently stops being daemon-owned.
    #[test]
    fn the_submission_round_trips_through_the_driver() {
        let submission = submission("cook-round-trip", 4242);

        assert_eq!(submission["type"], AGENT_TASK_COOK_JOB_TYPE);
        assert_eq!(submission["version"], AGENT_TASK_COOK_JOB_VERSION);
        assert_eq!(
            submission["idempotency_key"],
            "agent-task-cook:cook-round-trip"
        );

        let job = AgentTaskCookJob::parse(submission["request"].clone()).expect("parse request");
        assert_eq!(job.request.cook_id, "cook-round-trip");
        assert_eq!(job.request.child_pid, 4242);
        assert_eq!(job.request.child_start_identity, IDENTITY);
        assert_eq!(job.phase, AgentTaskCookJobPhase::Queued);
        assert_eq!(job.run_id, None);
        assert_eq!(job.terminal_state, None);

        let driver = CookJobDriver;
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
        let driver = CookJobDriver;
        for smuggled in [
            json!({ "prompt": "the private task text" }),
            json!({ "env": { "ANTHROPIC_API_KEY": "sk-live-secret" } }),
            json!({ "provider_invocation": { "command": "claude --token sk-live" } }),
            json!({ "notification_route": "opaque-destination" }),
            json!({ "launcher_log": "/home/operator/.homeboy/cook.log" }),
        ] {
            let mut request = request_of("cook-secrets", 4242);
            let object = request.as_object_mut().expect("request is an object");
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
        let driver = CookJobDriver;
        let mut job =
            AgentTaskCookJob::parse(request_of("cook-public", 4242)).expect("parse request");
        job.phase = AgentTaskCookJobPhase::Completed;
        job.run_id = Some("cook-public-attempt-1".to_string());
        job.terminal_state = Some(agent_task_lifecycle::AgentTaskRunState::Succeeded);
        let value = job.to_checkpoint().expect("serialize job");

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
        let progress = driver.public_progress(&private).expect("public progress");
        assert!(!progress.to_string().contains("private task text"));
        let result = driver.public_result(&private).expect("public result");
        assert!(!result.to_string().contains("private task text"));
    }

    /// A cook's error text can quote provider output, which can quote the
    /// prompt. Only the typed code may cross into public job state.
    #[test]
    fn the_public_error_carries_only_a_code() {
        let public = CookJobDriver.public_error(&invalid_cook_job("prompt: the private task text"));

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

    /// A child that ended without ever publishing durable identity is not a
    /// success. Reporting it as one would reproduce the dishonesty detachment
    /// exists to remove.
    #[test]
    fn an_unfinished_cook_terminalizes_as_failed() {
        with_isolated_home(|_| {
            let cook_id = "cook-never-submitted";
            agent_task_lifecycle::record_detached_cook_handoff_parent(cook_id)
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

        CookJobDriver
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
            agent_task_lifecycle::record_detached_cook_handoff_parent(cook_id)
                .expect("persist handoff parent");
            let child = std::process::Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn detached cook fixture");
            let start_identity = homeboy_core::process::process_start_identity(child.id())
                .expect("inspect fixture")
                .expect("fixture has a start identity");
            agent_task_lifecycle::record_detached_cook_handoff_child(
                cook_id,
                child.id(),
                start_identity.clone(),
            )
            .expect("persist detached child identity");

            let submission = cook_job_submission(cook_id, child.id(), &start_identity)
                .expect("build submission");

            CookJobDriver
                .cancel(&submission["request"])
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
    fn driver_registration_is_idempotent() {
        register_cook_job_driver();
        register_cook_job_driver();
    }
}

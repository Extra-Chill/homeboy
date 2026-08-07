//! Daemon-owned lifecycle for a locally-placed, detached cook batch.
//!
//! This is the sibling of [`super::cook_job`], and deliberately reads like it.
//! Where that driver owns one detached Cook, this one owns a whole fanout wave:
//! `fanout cook-batch --run-plan` and `fanout run-plan`.
//!
//! # It supervises rather than executes, for the same reasons
//!
//! Every argument in `cook_job`'s module documentation applies here unchanged,
//! and applies *more*, because a batch multiplies it by the number of children:
//!
//! 1. `agent_task_secrets::resolve_secret_env_with_config_and_fallbacks` lists
//!    the ambient process environment as the **first** secret provider, so
//!    whichever process runs a cook decides which credentials its provider gets.
//! 2. `agent_task_provider::command_runner` never calls `env_clear` for the
//!    real provider invocation, so the provider inherits that environment whole.
//! 3. The daemon is spawned with neither `env_clear` nor `current_dir`, is
//!    long-lived and shared, and auto-starts on the first `connect()` — so its
//!    environment is a snapshot of whoever happened to start it first.
//!
//! A daemon-hosted batch would therefore run every child against credentials
//! from an environment no operator chose. So the launcher still spawns the
//! coordinator as a child process in the operator's own environment, and the
//! daemon owns everything *around* it: the durable job, checkpointing,
//! cancellation, and HTTP inspection.
//!
//! # What the durable request is, and why a batch id is enough
//!
//! `cook_job` carries a `cook_id` because `AgentTaskCookServiceOptions` cannot
//! be serialized — it holds an `Arc<dyn AgentTaskCookAttemptDispatcher>` — while
//! the Cook recipe behind that id can. The batch has the same seam one level up.
//! Before a batch dispatches its first child, the fanout coordinator has already
//! written `persist_fanout_run_batch_record` (the durable batch record naming
//! every child run id) and `persist_batch_cook_recipes` (one durable recipe per
//! child). `resume_cook_batch` is the standing proof that this is sufficient:
//! it rebuilds an entire wave, children and all, from nothing but a batch id.
//!
//! So the batch id is the whole durable request, exactly as the cook id is for
//! a single Cook. Nothing about the plan, the prompts, the gates, the worktrees
//! or the provider selection enters this job.
//!
//! # The correctness property this buys
//!
//! Because this driver never spawns a coordinator, `resume` cannot re-run a
//! child. It re-adopts supervision of a process it did not create, identified by
//! PID *and* kernel start identity so PID reuse cannot alias a stranger, or —
//! if that process is gone — reads the durable outcome the wave actually
//! reached. Idempotency is structural rather than claimed.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::Duration;

use homeboy_core::daemon::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobHandle, ControllerJobPublicError,
};
use homeboy_core::process::{
    process_identity_state_with_start_identity, ProcessIdentityState, ProcessStartIdentity,
};
use homeboy_core::Result;

use super::cook::AgentTaskCookBatchControl;
use crate::agent_task_batch::{self, AgentTaskBatchState};
use crate::agent_task_lifecycle;

pub const AGENT_TASK_COOK_BATCH_JOB_TYPE: &str = "agent-task-cook-batch";
pub const AGENT_TASK_COOK_BATCH_JOB_VERSION: u32 = 1;
const AGENT_TASK_COOK_BATCH_JOB_SCHEMA: &str = "homeboy/agent-task-cook-batch-job/v1";

/// How often supervision re-reads durable batch state and coordinator liveness.
///
/// Slower than `cook_job`'s poll on purpose: each tick reads the batch record
/// plus one lifecycle record per child, so the cost scales with wave width,
/// while the events it is watching for — a child terminalizing, a multi-hour
/// coordinator exiting — are minutes apart.
const SUPERVISION_POLL: Duration = Duration::from_millis(1_000);

/// The durable controller-job request for one detached cook batch.
///
/// `deny_unknown_fields` is the enforcement point for
/// [`ControllerJobDriver::validate_secret_references`]: the only admissible
/// request is a *reference* to durable batch state plus the coordinator
/// identity needed to supervise it. A caller cannot smuggle a plan, a prompt, a
/// provider invocation, an environment block, or a notification route through
/// this struct, because any field not named here is a hard parse error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskCookBatchJobRequest {
    pub schema: String,
    /// The durable batch alias. Every execution input for every child lives
    /// behind this id, in the batch record and the per-child Cook recipes,
    /// never inline in the job.
    pub batch_id: String,
    pub child_pid: u32,
    pub child_start_identity: ProcessStartIdentity,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskCookBatchJobPhase {
    /// Admitted, not yet supervising.
    #[default]
    Queued,
    /// The daemon is watching a live detached coordinator.
    Supervising,
    /// The coordinator ended and the wave's durable outcome was observed.
    Completed,
}

/// The durable controller job, including its recovery checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskCookBatchJob {
    pub schema: String,
    pub idempotency_key: String,
    pub request: AgentTaskCookBatchJobRequest,
    #[serde(default)]
    pub phase: AgentTaskCookBatchJobPhase,
    /// Children observed to have reached a durable terminal state.
    ///
    /// This is the checkpoint that makes a recovered job resume rather than
    /// restart. It records what supervision has *seen*, never what it intends
    /// to do, so replaying it can only ever narrow the remaining work.
    #[serde(default)]
    pub observed_terminal_children: BTreeSet<String>,
    /// Children the batch record declared, once it exists.
    #[serde(default)]
    pub declared_children: usize,
    /// Aggregate batch state observed for the wave, set only in `Completed`.
    #[serde(default)]
    pub terminal_state: Option<AgentTaskBatchState>,
}

impl AgentTaskCookBatchJob {
    pub fn new(request: AgentTaskCookBatchJobRequest) -> Result<Self> {
        if request.schema != AGENT_TASK_COOK_BATCH_JOB_SCHEMA {
            return Err(invalid_cook_batch_job(
                "cook batch jobs require a recognized request schema",
            ));
        }
        if request.batch_id.trim().is_empty() {
            return Err(invalid_cook_batch_job(
                "cook batch jobs require a durable batch id",
            ));
        }
        if request.child_pid == 0 {
            return Err(invalid_cook_batch_job(
                "cook batch jobs require the detached coordinator's process id",
            ));
        }
        Ok(Self {
            schema: AGENT_TASK_COOK_BATCH_JOB_SCHEMA.to_string(),
            // The batch id is already unique and is the durable identity of this
            // wave, so replaying a submit converges on one job rather than
            // creating a second supervisor for the same coordinator.
            idempotency_key: format!("agent-task-cook-batch:{}", request.batch_id),
            request,
            phase: AgentTaskCookBatchJobPhase::Queued,
            observed_terminal_children: BTreeSet::new(),
            declared_children: 0,
            terminal_state: None,
        })
    }

    fn parse(value: Value) -> Result<Self> {
        let job: Self = serde_json::from_value(value).map_err(|error| {
            invalid_cook_batch_job(&format!("invalid durable cook batch job: {error}"))
        })?;
        job.validate()?;
        Ok(job)
    }

    fn validate(&self) -> Result<()> {
        if self.schema != AGENT_TASK_COOK_BATCH_JOB_SCHEMA || self.idempotency_key.trim().is_empty()
        {
            return Err(invalid_cook_batch_job(
                "cook batch jobs require a recognized schema and idempotency key",
            ));
        }
        let expected = Self::new(self.request.clone())?;
        if self.idempotency_key != expected.idempotency_key {
            return Err(invalid_cook_batch_job(
                "cook batch job idempotency key does not match its immutable request",
            ));
        }
        if self.phase == AgentTaskCookBatchJobPhase::Completed && self.terminal_state.is_none() {
            return Err(invalid_cook_batch_job(
                "completed cook batch jobs require an observed terminal state",
            ));
        }
        if self.phase != AgentTaskCookBatchJobPhase::Completed && self.terminal_state.is_some() {
            return Err(invalid_cook_batch_job(
                "only completed cook batch jobs may record a terminal state",
            ));
        }
        Ok(())
    }

    fn to_checkpoint(&self) -> Result<Value> {
        serde_json::to_value(self).map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("serialize cook batch job checkpoint".to_string()),
            )
        })
    }

    fn completed_result(&self) -> Result<Value> {
        let terminal_state = self.terminal_state.ok_or_else(|| {
            invalid_cook_batch_job("completed cook batch jobs require an observed terminal state")
        })?;
        Ok(json!({
            "phase": self.phase,
            "batch_id": self.request.batch_id,
            "children_total": self.declared_children,
            "children_terminal": self.observed_terminal_children.len(),
            "terminal_state": terminal_state,
        }))
    }

    /// The public projection.
    ///
    /// Withholds the coordinator's pid and kernel start identity, matching
    /// `AgentTaskCookJob::public_projection`: those are liveness tokens only
    /// supervision needs. It also withholds the child *id list*, projecting
    /// only counts. A child id names a branch and an issue, and the counts are
    /// what a reader of public job state actually needs; the full roster stays
    /// where it already lives, behind `agent-task fanout status <batch_id>`.
    fn public_projection(&self) -> Value {
        json!({
            "schema": self.schema,
            "idempotency_key": self.idempotency_key,
            "phase": self.phase,
            "batch_id": self.request.batch_id,
            "children_total": self.declared_children,
            "children_terminal": self.observed_terminal_children.len(),
            "terminal_state": self.terminal_state,
        })
    }

    fn progress_projection(&self) -> Value {
        json!({
            "phase": self.phase,
            "batch_id": self.request.batch_id,
            "children_total": self.declared_children,
            "children_terminal": self.observed_terminal_children.len(),
        })
    }

    /// Read the wave's durable outcome and complete the job.
    ///
    /// `agent_task_batch::status` is the single definition of a batch's
    /// aggregate state: it reconciles every child from its lifecycle record and
    /// converges the durable batch record. Calling it here means this driver
    /// never re-derives an aggregate of its own that could disagree with what
    /// `fanout status` reports.
    ///
    /// A coordinator that died before it could even write its batch record is
    /// reported as `Failed` rather than dressed up as an empty success, the
    /// same honesty `cook_job` applies to `exited_before_handoff`.
    fn observe_terminal(&mut self) -> Result<Value> {
        self.refresh_observations();
        self.terminal_state = Some(match agent_task_batch::status(&self.request.batch_id) {
            // The coordinator is gone but the wave still reads as running or
            // queued: it was interrupted, not finished. Reporting the
            // in-flight state as this job's terminal state would claim the
            // wave is still advancing when nothing owns it any more.
            //
            // `PartialFailure` is the honest aggregate — some children may have
            // completed and be salvageable — and it exits non-zero, which is
            // what an operator needs. The durable children are left exactly as
            // they are for `agent-task fanout resume` to harvest.
            Ok(report)
                if matches!(
                    report.batch.state,
                    AgentTaskBatchState::Running | AgentTaskBatchState::Queued
                ) =>
            {
                AgentTaskBatchState::PartialFailure
            }
            Ok(report) => report.batch.state,
            Err(_) => AgentTaskBatchState::Failed,
        });
        self.phase = AgentTaskCookBatchJobPhase::Completed;
        self.completed_result()
    }

    /// Re-read durable batch state. Returns true when the observation grew,
    /// which is the only condition that warrants a new checkpoint.
    ///
    /// # Why this reads one file and no lifecycle records
    ///
    /// The obvious implementation — walk `child_runs` and ask
    /// `agent_task_lifecycle::status` about each — is the wrong thing to do at
    /// supervision frequency. That read is not a read: it reconciles deferred
    /// candidates, projects runner events, rewrites the record, and for a child
    /// that is not controller-local it *probes the runner*. A ten-child wave
    /// would issue ten reconciling reads, and up to ten remote probes, every
    /// tick for the multi-hour life of the coordinator.
    ///
    /// So supervision reads what the coordinator itself published instead. A
    /// daemon-owned coordinator records each child's outcome into
    /// `metadata.child_finalizations` as it terminalizes — that is what
    /// `publish_child_terminalization` is for — keyed exactly as
    /// `resume_cook_batch` keys it. One file read per tick, no writes, no
    /// remote calls, and the provenance is right: this counts children the
    /// coordinator asserts it finished, not children this driver inferred.
    ///
    /// The observation can only ever under-report — a coordinator killed before
    /// it published a child leaves that child uncounted — which is the safe
    /// direction. `observe_terminal` reconciles authoritatively at the end.
    fn refresh_observations(&mut self) -> bool {
        let Ok(record) = agent_task_batch::read_batch_record(&self.request.batch_id) else {
            return false;
        };
        let mut changed = self.declared_children != record.child_runs.len();
        self.declared_children = record.child_runs.len();

        let published = record.metadata.get("child_finalizations");
        for child in &record.child_runs {
            if self.observed_terminal_children.contains(&child.run_id) {
                continue;
            }
            let finished = published
                .and_then(|published| published.get(&child.run_id))
                .is_some()
                || child.state.is_terminal();
            if finished {
                self.observed_terminal_children.insert(child.run_id.clone());
                changed = true;
            }
        }
        changed
    }
}

fn invalid_cook_batch_job(message: &str) -> homeboy_core::Error {
    homeboy_core::Error::validation_invalid_argument("cook_batch_job", message, None, None)
}

pub struct CookBatchJobDriver;

impl ControllerJobDriver for CookBatchJobDriver {
    fn job_type(&self) -> &'static str {
        AGENT_TASK_COOK_BATCH_JOB_TYPE
    }

    fn version(&self) -> u32 {
        AGENT_TASK_COOK_BATCH_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(AgentTaskCookBatchJob::parse(request.clone())?.public_projection())
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        // Projected field by field rather than passed through, so a future
        // private progress field cannot reach the public log by default.
        Ok(json!({
            "phase": progress.get("phase").cloned().unwrap_or(Value::Null),
            "batch_id": progress.get("batch_id").cloned().unwrap_or(Value::Null),
            "children_total": progress.get("children_total").cloned().unwrap_or(Value::Null),
            "children_terminal": progress.get("children_terminal").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        Ok(json!({
            "phase": result.get("phase").cloned().unwrap_or(Value::Null),
            "batch_id": result.get("batch_id").cloned().unwrap_or(Value::Null),
            "children_total": result.get("children_total").cloned().unwrap_or(Value::Null),
            "children_terminal": result.get("children_terminal").cloned().unwrap_or(Value::Null),
            "terminal_state": result.get("terminal_state").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_error(&self, error: &homeboy_core::Error) -> ControllerJobPublicError {
        // A batch's error text can quote a child cook's error, which can quote
        // provider output, which can quote the prompt. Only the typed code
        // crosses into public job state.
        ControllerJobPublicError {
            message: "controller-owned cook batch supervision failed".to_string(),
            data: json!({ "code": format!("{:?}", error.code) }),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        // `deny_unknown_fields` on both the job and its request makes any inline
        // secret — a plan, a prompt, an env block, a token — a parse failure
        // rather than an accepted field.
        AgentTaskCookBatchJob::parse(request.clone()).map(|_| ())
    }

    fn prepare(&self, request: Value) -> Result<Value> {
        let mut job = AgentTaskCookBatchJob::parse(request)?;
        if job.phase != AgentTaskCookBatchJobPhase::Queued || job.terminal_state.is_some() {
            return Err(invalid_cook_batch_job(
                "new cook batch jobs must start queued without a terminal state",
            ));
        }
        job.phase = AgentTaskCookBatchJobPhase::Supervising;
        job.to_checkpoint()
    }

    fn execute(&self, prepared: Value, handle: ControllerJobHandle) -> Result<Value> {
        let mut job = AgentTaskCookBatchJob::parse(prepared)?;
        self.supervise(&mut job, handle)
    }

    /// Re-adopt supervision after a daemon restart.
    ///
    /// Idempotent by construction: no branch below starts a coordinator or a
    /// child. A completed job short-circuits on its durable terminal state; an
    /// unfinished one either re-attaches to a coordinator still provably alive,
    /// or reads the durable outcome of one that is not.
    ///
    /// Note what is deliberately absent: there is no branch that relaunches an
    /// interrupted wave. A wave whose coordinator died is carried forward by
    /// `agent-task fanout resume`, which harvests terminal-but-unfinalized
    /// children through their original gates and finalization contract. Doing
    /// that from here would put two owners on the same children.
    fn resume(&self, checkpoint: Value, handle: ControllerJobHandle) -> Result<Value> {
        let mut job = AgentTaskCookBatchJob::parse(checkpoint)?;
        match job.resume_disposition() {
            // Replaying a finished job reports its durable result and touches
            // nothing. This is what makes repeated recovery safe.
            CookBatchJobResumeDisposition::AlreadyComplete => job.completed_result(),
            // Neither branch starts anything: supervision either re-attaches to
            // a coordinator that is still provably ours, or observes on its
            // first iteration that it is gone and terminalizes from durable
            // state.
            CookBatchJobResumeDisposition::ReadoptLiveCoordinator
            | CookBatchJobResumeDisposition::ObserveTerminalOutcome => {
                self.supervise(&mut job, handle)
            }
        }
    }

    /// Stop the wave through the one established cancellation path.
    ///
    /// Two things are true of a batch that are not true of a single Cook, and
    /// they need different mechanisms:
    ///
    /// * A child already in flight is stopped exactly as `cook_job` stops its
    ///   Cook — `agent_task_lifecycle::cancel_run`, which terminates a detached
    ///   child's process tree under an exact `ProcessStartIdentity` match before
    ///   an attempt exists, and marks the live attempt cancelled after one does,
    ///   which the running cook's own supervisor turns into a process-tree
    ///   termination. No second stop mechanism is introduced.
    /// * A child that was never claimed has no lifecycle record at all, so there
    ///   is nothing to cancel. Those are stopped by
    ///   `record_coordinator_cancellation`, which the coordinator's claim loop
    ///   reads before starting each child.
    ///
    /// The cancellation marker is written *first*, so a coordinator racing this
    /// call stops claiming while the per-child cancellations are still going out
    /// rather than starting fresh work behind them.
    ///
    /// The coordinator process itself is deliberately **not** signalled. It is
    /// the parent of every in-flight cook, and killing its process tree would
    /// take down children in the middle of committing, pushing, or opening a
    /// pull request — precisely the states `cancel_run`'s ordered path exists to
    /// terminalize cleanly. With no claimable work and no live children, the
    /// coordinator drains and exits on its own.
    fn cancel(&self, prepared: &Value) -> Result<()> {
        let job = AgentTaskCookBatchJob::parse(prepared.clone())?;
        if job.phase == AgentTaskCookBatchJobPhase::Completed {
            return Ok(());
        }
        stop_batch(&job.request.batch_id)
    }
}

/// Plant the coordinator's stop signal and terminalize its live children.
///
/// Reached from two places because cancellation can arrive at two very
/// different moments: from [`ControllerJobDriver::cancel`] when the daemon
/// cancels the job, and from supervision when it observes that cancellation.
///
/// A coordinator that has not yet written its batch record is not an error. It
/// has no children to stop and nothing durable to mark, and failing here would
/// make the job refuse to cancel over a startup race. Supervision calls this
/// again once it notices the cancellation, by which point the record exists —
/// that second call is what actually stops a wave cancelled during startup.
fn stop_batch(batch_id: &str) -> Result<()> {
    let Ok(record) = agent_task_batch::read_batch_record(batch_id) else {
        return Ok(());
    };
    // Written before the per-child cancellations go out, so a coordinator
    // racing this stops claiming while they are still in flight rather than
    // starting fresh children behind them.
    agent_task_batch::record_coordinator_cancellation(batch_id, "controller job cancelled")?;

    for child in &record.child_runs {
        // A child with no durable record was never claimed, so `cancel_run`
        // has nothing to terminalize and reports "not found". That is the
        // expected shape for an unstarted child, and the cancellation marker
        // above already covers it, so it must not fail the cancellation of the
        // siblings that *are* running.
        if child_run_is_terminal(&child.run_id) {
            continue;
        }
        let _ = agent_task_lifecycle::cancel_run(
            &child.run_id,
            Some("controller job cancelled its batch"),
        );
    }
    Ok(())
}

impl CookBatchJobDriver {
    /// Watch a detached coordinator to the wave's durable terminal state.
    ///
    /// Liveness is judged by PID *and* kernel start identity. An
    /// `IdentityMismatch` or `Unverifiable` reading is treated as "no longer our
    /// coordinator" rather than as death, so supervision can never attribute a
    /// stranger's process to this batch.
    fn supervise(
        &self,
        job: &mut AgentTaskCookBatchJob,
        handle: ControllerJobHandle,
    ) -> Result<Value> {
        job.phase = AgentTaskCookBatchJobPhase::Supervising;
        job.refresh_observations();
        handle.checkpoint(job.to_checkpoint()?)?;
        handle.progress(job.progress_projection())?;

        loop {
            // Cancellation is the daemon's to terminalize; this thread only
            // needs to stop supervising promptly so the supervisor can join it.
            //
            // It also plants the stop signal a second time. `cancel` runs the
            // moment the daemon cancels the job, which can be before the
            // coordinator has written its batch record — and with no record
            // there is nothing to mark and no children to stop. This is the
            // later chance, and it is the one that actually stops a wave
            // cancelled during its startup window.
            if handle.is_cancelled() {
                let _ = stop_batch(&job.request.batch_id);
                return job.observe_terminal();
            }

            // Checkpoint every time a child terminalizes, so a daemon restart
            // resumes against what the wave has actually finished rather than
            // re-observing it from zero.
            if job.refresh_observations() {
                handle.checkpoint(job.to_checkpoint()?)?;
                handle.progress(job.progress_projection())?;
            }

            if !coordinator_is_live(&job.request) {
                return job.observe_terminal();
            }

            std::thread::sleep(SUPERVISION_POLL);
        }
    }
}

/// What a resumed cook batch job must do, decided before any job handle is
/// needed.
///
/// Extracted so the idempotency property can be asserted directly: no variant
/// here starts a coordinator or a child, and the enum is exhaustive over the
/// states a recovered checkpoint can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookBatchJobResumeDisposition {
    /// The job already reached a durable terminal state. Replay it.
    AlreadyComplete,
    /// The supervised coordinator is still provably the process we were handed.
    ReadoptLiveCoordinator,
    /// The coordinator is gone. Read the wave's durable outcome; never re-run it.
    ObserveTerminalOutcome,
}

impl AgentTaskCookBatchJob {
    pub fn resume_disposition(&self) -> CookBatchJobResumeDisposition {
        if self.phase == AgentTaskCookBatchJobPhase::Completed {
            return CookBatchJobResumeDisposition::AlreadyComplete;
        }
        if coordinator_is_live(&self.request) {
            return CookBatchJobResumeDisposition::ReadoptLiveCoordinator;
        }
        CookBatchJobResumeDisposition::ObserveTerminalOutcome
    }
}

/// Whether one child run has reached a durable terminal state.
///
/// This is the reconciling read, and it is deliberately confined to
/// cancellation — a one-shot pass over the roster, where paying for accuracy is
/// right and paying it once is cheap. Supervision must not use it; see
/// [`AgentTaskCookBatchJob::refresh_observations`].
///
/// An unreadable record is *not* terminal. A child whose record cannot be read
/// has not been proven finished, and treating an IO failure as completion would
/// leave a running child un-cancelled.
fn child_run_is_terminal(run_id: &str) -> bool {
    agent_task_lifecycle::status(run_id)
        .map(|record| record.state.is_terminal())
        .unwrap_or(false)
}

/// Whether the supervised coordinator is still provably the process we were
/// handed.
fn coordinator_is_live(request: &AgentTaskCookBatchJobRequest) -> bool {
    matches!(
        process_identity_state_with_start_identity(
            request.child_pid,
            None,
            Some(&request.child_start_identity),
        ),
        ProcessIdentityState::Live
    )
}

/// Register the cook batch driver with core's generic controller-job lifecycle.
/// Registration is idempotent because CLI startup can run in test processes
/// that initialize the command runtime more than once.
pub fn register_cook_batch_job_driver() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        controller_job_driver::register_controller_job_driver(std::sync::Arc::new(
            CookBatchJobDriver,
        ))
        .expect("register cook batch controller job driver");
    });
}

/// Environment signal the detached launcher sets on the coordinator it spawns.
///
/// Its value is the durable batch id the launcher submitted to the daemon. It
/// is an environment variable rather than a CLI flag for the same reason
/// `HOMEBOY_RUNNER_HOSTED_EXEC` is: it describes the *execution context* a
/// coordinator was placed in, not a request an operator made, and it must not
/// become public command surface that anything else can assert.
///
/// It carries the batch id rather than a bare boolean so
/// [`detached_batch_coordinator_control`] can verify that the coordinator it is
/// arming is the one the daemon actually owns. An inherited variable from an
/// unrelated ancestor process therefore arms nothing.
pub const DETACHED_BATCH_COORDINATOR_ENV: &str = "HOMEBOY_FANOUT_CONTROLLER_JOB_BATCH_ID";

/// The control a batch coordinator should run under in this process.
///
/// Returns the daemon-owned control only when this process was spawned by the
/// detached launcher *for this batch*. Everything else — an attached
/// `fanout run-plan`, a Lab-placed wave, a coordinator running under a runner —
/// gets [`AgentTaskCookBatchControl::default`], which is today's behaviour
/// exactly. There is no durable owner in those cases, so a coordinator that
/// honoured cancellation or skipped durably-terminal children would be
/// answering to nobody.
pub fn detached_batch_coordinator_control(batch_id: &str) -> AgentTaskCookBatchControl {
    match std::env::var(DETACHED_BATCH_COORDINATOR_ENV) {
        Ok(owned) if owned == batch_id => AgentTaskCookBatchControl::daemon_owned(),
        _ => AgentTaskCookBatchControl::default(),
    }
}

/// Build the durable submit payload for one detached cook batch.
///
/// Lives here rather than in the launcher so the wire shape and the driver that
/// parses it cannot drift apart.
pub fn cook_batch_job_submission(
    batch_id: &str,
    child_pid: u32,
    child_start_identity: &ProcessStartIdentity,
) -> Result<Value> {
    let job = AgentTaskCookBatchJob::new(AgentTaskCookBatchJobRequest {
        schema: AGENT_TASK_COOK_BATCH_JOB_SCHEMA.to_string(),
        batch_id: batch_id.to_string(),
        child_pid,
        child_start_identity: child_start_identity.clone(),
    })?;
    Ok(json!({
        "type": AGENT_TASK_COOK_BATCH_JOB_TYPE,
        "version": AGENT_TASK_COOK_BATCH_JOB_VERSION,
        "idempotency_key": job.idempotency_key,
        "request": job.to_checkpoint()?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_batch::FanoutRunBatchChild;
    use homeboy_core::test_support::with_isolated_home;

    const IDENTITY: ProcessStartIdentity = ProcessStartIdentity::Linux {
        starttime_ticks: 4242,
    };

    fn submission(batch_id: &str, pid: u32) -> Value {
        cook_batch_job_submission(batch_id, pid, &IDENTITY).expect("build batch job submission")
    }

    fn request_of(batch_id: &str, pid: u32) -> Value {
        submission(batch_id, pid)
            .get("request")
            .cloned()
            .expect("submission carries a request")
    }

    fn persist_batch(batch_id: &str, children: &[&str]) {
        let children = children
            .iter()
            .map(|id| FanoutRunBatchChild {
                task_id: (*id).to_string(),
                run_id: format!("cook-{id}"),
            })
            .collect::<Vec<_>>();
        agent_task_batch::persist_fanout_run_batch(batch_id, batch_id, &children, json!({}))
            .expect("persist batch record");
    }

    /// The wire payload the launcher sends must be exactly what the driver
    /// admits, or detachment silently stops being daemon-owned.
    #[test]
    fn the_submission_round_trips_through_the_driver() {
        let submission = submission("fanout-round-trip", 4242);

        assert_eq!(submission["type"], AGENT_TASK_COOK_BATCH_JOB_TYPE);
        assert_eq!(submission["version"], AGENT_TASK_COOK_BATCH_JOB_VERSION);
        assert_eq!(
            submission["idempotency_key"],
            "agent-task-cook-batch:fanout-round-trip"
        );

        let job =
            AgentTaskCookBatchJob::parse(submission["request"].clone()).expect("parse request");
        assert_eq!(job.request.batch_id, "fanout-round-trip");
        assert_eq!(job.request.child_pid, 4242);
        assert_eq!(job.request.child_start_identity, IDENTITY);
        assert_eq!(job.phase, AgentTaskCookBatchJobPhase::Queued);
        assert!(job.observed_terminal_children.is_empty());
        assert_eq!(job.terminal_state, None);

        let driver = CookBatchJobDriver;
        driver
            .validate_secret_references(&submission["request"])
            .expect("a reference-only request validates");
        driver
            .public_request(&submission["request"])
            .expect("public projection");
    }

    /// The batch id is the durable identity of this wave, so a replayed submit
    /// must converge on one supervisor rather than spawn a second.
    #[test]
    fn the_idempotency_key_is_the_batch_id() {
        assert_eq!(
            submission("fanout-same", 1)["idempotency_key"],
            submission("fanout-same", 2)["idempotency_key"],
        );
        assert_ne!(
            submission("fanout-a", 1)["idempotency_key"],
            submission("fanout-b", 1)["idempotency_key"],
        );
    }

    /// The plan, its prompts, its provider selection and its gates are all
    /// sensitive and none of them belong in a job whose request is a reference.
    /// `deny_unknown_fields` is what enforces that, so prove it rejects.
    #[test]
    fn inline_secrets_are_refused_rather_than_carried() {
        let driver = CookBatchJobDriver;
        for smuggled in [
            json!({ "plan": { "cooks": [{ "prompt": "the private task text" }] } }),
            json!({ "prompt_template": "the private task text" }),
            json!({ "env": { "ANTHROPIC_API_KEY": "sk-live-secret" } }),
            json!({ "provider_config": { "token": "sk-live" } }),
            json!({ "notification_route": "opaque-destination" }),
            json!({ "coordinator_log": "/home/operator/.homeboy/fanout.log" }),
        ] {
            let mut request = request_of("fanout-secrets", 4242);
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

    /// Even a well-formed job must not project the coordinator's kernel
    /// identity, nor the roster of child ids — a child id names a branch and an
    /// issue, and counts are what public job state needs.
    #[test]
    fn public_projections_withhold_process_identity_and_the_child_roster() {
        let driver = CookBatchJobDriver;
        let mut job =
            AgentTaskCookBatchJob::parse(request_of("fanout-public", 4242)).expect("parse request");
        job.phase = AgentTaskCookBatchJobPhase::Completed;
        job.declared_children = 3;
        job.observed_terminal_children
            .insert("cook-fanout-public-issue-1".to_string());
        job.terminal_state = Some(AgentTaskBatchState::PartialFailure);
        let value = job.to_checkpoint().expect("serialize job");

        let public = driver.public_request(&value).expect("public request");
        let public_text = public.to_string();
        assert!(!public_text.contains("4242"), "{public_text}");
        assert!(!public_text.contains("starttime_ticks"), "{public_text}");
        assert!(!public_text.contains("child_pid"), "{public_text}");
        assert!(!public_text.contains("issue-1"), "{public_text}");
        assert_eq!(public["batch_id"], "fanout-public");
        assert_eq!(public["children_total"], 3);
        assert_eq!(public["children_terminal"], 1);
        assert_eq!(public["terminal_state"], "partial_failure");

        // Progress and result are projected field by field, so a private field
        // added to either payload later cannot escape by default.
        let private = json!({
            "phase": "supervising",
            "batch_id": "fanout-public",
            "children_total": 3,
            "children_terminal": 1,
            "prompt": "the private task text",
        });
        let progress = driver.public_progress(&private).expect("public progress");
        assert!(!progress.to_string().contains("private task text"));
        let result = driver.public_result(&private).expect("public result");
        assert!(!result.to_string().contains("private task text"));
    }

    /// A batch error can quote a child cook's error, which can quote provider
    /// output, which can quote the prompt. Only the typed code may cross.
    #[test]
    fn the_public_error_carries_only_a_code() {
        let public =
            CookBatchJobDriver.public_error(&invalid_cook_batch_job("prompt: the private text"));

        assert_eq!(
            public.message,
            "controller-owned cook batch supervision failed"
        );
        assert!(!public.data.to_string().contains("private text"));
    }

    /// The single most important correctness property: a daemon restart must
    /// not be able to re-run a wave. No disposition starts work, and a finished
    /// job replays rather than re-executes.
    #[test]
    fn resume_never_re_runs_a_completed_job() {
        let mut job = AgentTaskCookBatchJob::parse(request_of("fanout-complete", 4242))
            .expect("parse request");
        job.phase = AgentTaskCookBatchJobPhase::Completed;
        job.declared_children = 2;
        job.observed_terminal_children
            .insert("cook-fanout-complete-a".to_string());
        job.observed_terminal_children
            .insert("cook-fanout-complete-b".to_string());
        job.terminal_state = Some(AgentTaskBatchState::Succeeded);

        assert_eq!(
            job.resume_disposition(),
            CookBatchJobResumeDisposition::AlreadyComplete
        );
        // Replay is byte-identical however many times recovery happens.
        let first = job.completed_result().expect("first replay");
        let second = job.completed_result().expect("second replay");
        assert_eq!(first, second);
        assert_eq!(first["terminal_state"], "succeeded");
        assert_eq!(first["children_terminal"], 2);
    }

    /// A checkpoint whose coordinator is gone resolves to observation, never to
    /// a re-execution.
    #[test]
    fn resume_observes_rather_than_restarts_a_dead_coordinator() {
        let mut job =
            AgentTaskCookBatchJob::parse(request_of("fanout-dead", 4242)).expect("parse request");
        job.phase = AgentTaskCookBatchJobPhase::Supervising;
        // u32::MAX is not a live pid, and the recorded start identity cannot
        // match, so liveness is provably false.
        job.request.child_pid = u32::MAX;

        assert_eq!(
            job.resume_disposition(),
            CookBatchJobResumeDisposition::ObserveTerminalOutcome
        );
    }

    /// A coordinator that died before writing its batch record is not a
    /// vacuously successful empty wave.
    #[test]
    fn a_coordinator_that_never_wrote_a_batch_record_terminalizes_as_failed() {
        with_isolated_home(|_| {
            let mut job = AgentTaskCookBatchJob::parse(request_of("fanout-never-started", 4242))
                .expect("parse request");
            job.phase = AgentTaskCookBatchJobPhase::Supervising;

            let result = job.observe_terminal().expect("observe terminal");

            assert_eq!(job.phase, AgentTaskCookBatchJobPhase::Completed);
            assert_eq!(result["terminal_state"], "failed");
        });
    }

    /// Observation is monotone and is what the checkpoint records. A child that
    /// has not terminalized is never counted, and re-observing the same durable
    /// state must not report a change — otherwise supervision would checkpoint
    /// on every poll.
    #[test]
    fn observation_grows_only_with_durable_child_terminality() {
        with_isolated_home(|_| {
            persist_batch("fanout-observe", &["a", "b"]);
            let mut job = AgentTaskCookBatchJob::parse(request_of("fanout-observe", 4242))
                .expect("parse request");

            // First read learns the roster.
            assert!(job.refresh_observations());
            assert_eq!(job.declared_children, 2);
            // Neither child has a lifecycle record, so neither is terminal, and
            // a repeat read is not a change.
            assert!(job.observed_terminal_children.is_empty());
            assert!(!job.refresh_observations());
        });
    }

    /// Cancelling a finished job is a no-op rather than an error, so a
    /// cancellation racing completion cannot fail the durable job.
    #[test]
    fn cancelling_a_completed_job_is_a_no_op() {
        let mut job = AgentTaskCookBatchJob::parse(request_of("fanout-cancel-complete", 4242))
            .expect("parse");
        job.phase = AgentTaskCookBatchJobPhase::Completed;
        job.terminal_state = Some(AgentTaskBatchState::Succeeded);

        CookBatchJobDriver
            .cancel(&job.to_checkpoint().expect("serialize"))
            .expect("cancelling a completed cook batch job is a no-op");
    }

    /// Cancellation must record the durable marker the coordinator's claim loop
    /// reads, and must not fail because some children were never claimed and so
    /// have no lifecycle record to cancel. That is the normal shape of
    /// cancelling a wave early.
    #[test]
    fn cancelling_marks_the_batch_and_tolerates_unstarted_children() {
        with_isolated_home(|_| {
            persist_batch("fanout-cancel", &["a", "b", "c"]);
            let request = request_of("fanout-cancel", 4242);

            CookBatchJobDriver
                .cancel(&request)
                .expect("cancellation tolerates children that never started");

            assert!(
                agent_task_batch::coordinator_is_cancelled("fanout-cancel"),
                "the claim loop's stop signal must be durable"
            );
            // Replaying cancellation converges rather than erroring.
            CookBatchJobDriver
                .cancel(&request)
                .expect("cancellation is idempotent");
        });
    }

    /// Cancellation can arrive before the coordinator has written its batch
    /// record. Failing there would make the job refuse to cancel over a startup
    /// race; supervision plants the signal again once the record exists.
    #[test]
    fn cancelling_before_the_batch_record_exists_is_not_an_error() {
        with_isolated_home(|_| {
            CookBatchJobDriver
                .cancel(&request_of("fanout-cancel-early", 4242))
                .expect("a wave cancelled during startup must still cancel");

            assert!(
                !agent_task_batch::coordinator_is_cancelled("fanout-cancel-early"),
                "there is no durable batch to mark yet, and none may be invented"
            );
        });
    }

    #[test]
    fn driver_registration_is_idempotent() {
        register_cook_batch_job_driver();
        register_cook_batch_job_driver();
    }
}

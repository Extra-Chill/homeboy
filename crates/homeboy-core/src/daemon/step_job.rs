//! `step`: one daemon-supervised child process with an admitted environment.
//!
//! A step is the unit of background work: an absolute program, its arguments,
//! a working directory, and an [`EnvMaterializationPlan`] admitted by the
//! caller. The daemon spawns the child with `env_clear()` plus that plan, so
//! the daemon's own environment never becomes credential or toolchain
//! authority for the work it runs. Public values are carried by value; secrets
//! are carried only as references and resolved at spawn time by the registered
//! secret resolver.
//!
//! Supervision is by PID plus kernel start identity. A restarted daemon
//! re-adopts a still-running child from its checkpoint instead of spawning it
//! again, and cancellation terminates the child's whole process tree. When the
//! daemon owns a delegated cgroup subtree (`crate::job_cgroup`), the child runs
//! in its own `jobs/<job-id>/` cgroup from its first instruction, and
//! cancelling, replacing or finishing the step stops that whole cgroup:
//! `cgroup.kill`, wait until it is empty, then remove it. The
//! child's combined output is written below `artifacts/jobs/<job_id>/`.
//!
//! A step may wait before it spawns (`not_before`, `runner_eligible`). Exit is
//! only a wake-up signal: when a step is linked to a durable run, that run's
//! record remains the outcome authority, and durable terminal consumers let the
//! owning domain reconcile it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobExecutionOwner, ControllerJobHandle,
    ControllerJobPublicError,
};
use crate::env_materialization_plan::{EnvMaterializationPlan, EnvSecretRef};
use crate::error::{Error, Result};
use crate::job_cgroup::JobCgroup;
use crate::process::{ProcessIdentityState, ProcessStartIdentity};

pub const STEP_JOB_TYPE: &str = "step";
pub const STEP_JOB_VERSION: u32 = 1;
pub const STEP_JOB_REQUEST_SCHEMA: &str = "homeboy/step-job-request/v1";
const STEP_JOB_CHECKPOINT_SCHEMA: &str = "homeboy/step-job-checkpoint/v1";
const STEP_EXIT_SCHEMA: &str = "homeboy/step-exit/v1";

/// The daemon job id, injected into every step child so the child can bind
/// its own durable records to the job that supervises it.
pub const STEP_JOB_ID_ENV: &str = "HOMEBOY_STEP_JOB_ID";
/// The step job supervising a local Cook. Set by the bound step child in place
/// of [`STEP_JOB_ID_ENV`] and read when that Cook's attempt materializes. It is
/// private to the supervising process: children it spawns (step jobs, gates)
/// must not inherit it, or a nested submission is stamped with a supervisor
/// that does not own it.
pub const LOCAL_COOK_SUPERVISOR_JOB_ID_ENV: &str = "HOMEBOY_LOCAL_COOK_SUPERVISOR_JOB_ID";
/// Exact path of the supervising daemon job store.
pub const STEP_JOB_STORE_ENV: &str = "HOMEBOY_STEP_JOB_STORE";
/// Combined stdout/stderr of the child, below the job's artifact directory.
pub const STEP_LOG_FILE: &str = "step.log";
/// The exit status atomically recorded by the child wrapper, including when
/// its supervising daemon has restarted.
pub const STEP_EXIT_FILE: &str = "exit.json";
/// The step's exact usage from its job cgroup, read before the cgroup goes.
const STEP_USAGE_FILE: &str = "usage.json";

const WAIT_POLL: Duration = Duration::from_millis(250);
const RUNNER_WAIT_INITIAL: Duration = Duration::from_secs(1);
const RUNNER_WAIT_CAP: Duration = Duration::from_secs(30);
/// High-lane waiters re-probe sooner so they claim a freed slot promptly.
const RUNNER_WAIT_HIGH_CAP: Duration = Duration::from_secs(5);
const RUNNER_WAIT_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// The suffix Linux appends to `/proc/self/exe` once the running binary has
/// been unlinked or replaced (for example by a Homeboy self-upgrade).
pub const DELETED_EXECUTABLE_SUFFIX: &str = " (deleted)";

/// The path a durable step program must be launched from.
///
/// A program recorded by a process whose binary had already been replaced can
/// carry Linux's ` (deleted)` suffix; spawning that string fails with ENOENT.
/// The installed binary lives at the path without the suffix, so launching
/// that path runs the upgraded build (#18844).
pub fn launchable_step_program(program: &str) -> String {
    program
        .strip_suffix(DELETED_EXECUTABLE_SUFFIX)
        .unwrap_or(program)
        .to_string()
}

/// The Homeboy executable path to record as a durable step program.
///
/// Durable step programs outlive the recording process and are spawned later,
/// possibly after an upgrade replaced the binary. They must never come from a
/// raw `std::env::current_exe()`, which names `<path> (deleted)` at that
/// point; this resolves the launchable startup path instead.
pub fn durable_step_program() -> Result<PathBuf> {
    let program = crate::daemon::launchable_executable().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve launchable Homeboy executable for a durable step".to_string()),
        )
    })?;
    Ok(PathBuf::from(launchable_step_program(
        &program.to_string_lossy(),
    )))
}

/// Non-transient runner refusals have a bounded convergence budget.
pub fn runner_wait_budget() -> Duration {
    Duration::from_secs(
        std::env::var("HOMEBOY_RUNNER_WAIT_BUDGET_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(RUNNER_WAIT_DEADLINE.as_secs()),
    )
}

pub fn runner_wait_is_bounded(reason: &str) -> bool {
    matches!(
        reason,
        "daemon_incompatible" | "daemon_not_fresh" | "stale_jobs"
    )
}

/// The admission lane a step declared through its public environment.
fn step_priority(request: &StepJobRequest) -> crate::lab_capacity::LabPriority {
    crate::lab_capacity::LabPriority::from_env_value(
        request
            .env
            .public_env
            .get(crate::lab_capacity::LAB_PRIORITY_ENV)
            .map(String::as_str),
    )
}

/// The step's fairness unit: its workflow instance, else its linked run.
fn step_owner(request: &StepJobRequest) -> &str {
    request
        .root_durable_run_id
        .as_deref()
        .or(request.linked_durable_run_id.as_deref())
        .unwrap_or(&request.key)
}

/// Re-evaluate the same eligibility authority used by running steps.
pub fn runner_wait_reason(runner: &str) -> Option<String> {
    runner_eligibility()
        .read()
        .expect("step runner eligibility lock")
        .clone()
        .and_then(|probe| probe(runner).err())
}
const SUPERVISE_POLL: Duration = Duration::from_millis(100);
const CANCEL_STOP_DEADLINE: Duration = Duration::from_secs(15);

/// Conditions that must hold before the child is spawned.
///
/// Persisted inside step rows that every daemon sharing a job store reads, so
/// decoding tolerates fields added by newer Homeboy versions. Admission stays
/// strict through [`StepJobRequest::parse_admission`] (homeboy#17735).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepWait {
    /// RFC 3339 instant before which the child must not start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<String>,
    /// Runner id that must be eligible to accept work before the child starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_eligible: Option<String>,
}

impl StepWait {
    fn is_empty(&self) -> bool {
        self.not_before.is_none() && self.runner_eligible.is_none()
    }
}

/// The admitted, immutable request for one step.
///
/// Decoding is forward-compatible: a row written by a newer Homeboy (for
/// example with `root_durable_run_id`) must not make an older daemon's job
/// store unreadable. New submissions are still checked strictly by
/// [`StepJobRequest::parse_admission`] (homeboy#17735).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepJobRequest {
    pub schema: String,
    pub terminal_consumer: String,
    /// Deterministic idempotency key. Submitting the same key returns the
    /// existing job instead of running the child again.
    pub key: String,
    /// Absolute path of the executable.
    pub program: String,
    /// Spawns without a Lab capacity slot: an orchestrator waiting on its own
    /// child steps (which need that capacity), or a declared light step.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waits_for_children: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Absolute working directory of the child.
    pub cwd: String,
    /// The complete child environment: public values plus secret references.
    #[serde(default)]
    pub env: EnvMaterializationPlan,
    /// Durable run this step executes for; recovery reads its record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_durable_run_id: Option<String>,
    /// Intentionally targets a terminal linked run instead of treating it as stale.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allows_terminal_linked_run: bool,
    /// Owning workflow instance, distinct from the child run this step starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_durable_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "StepWait::is_empty")]
    pub wait: StepWait,
}

impl StepJobRequest {
    pub fn new(
        key: impl Into<String>,
        program: impl Into<String>,
        args: Vec<String>,
        cwd: impl Into<String>,
        env: EnvMaterializationPlan,
        terminal_consumer: impl Into<String>,
    ) -> Self {
        Self {
            schema: STEP_JOB_REQUEST_SCHEMA.to_string(),
            terminal_consumer: terminal_consumer.into(),
            key: key.into(),
            program: launchable_step_program(&program.into()),
            waits_for_children: false,
            args,
            cwd: cwd.into(),
            env,
            linked_durable_run_id: None,
            allows_terminal_linked_run: false,
            root_durable_run_id: None,
            wait: StepWait::default(),
        }
    }

    /// Spawn without queueing for a Lab capacity slot.
    pub fn without_capacity_slot(mut self) -> Self {
        self.waits_for_children = true;
        self
    }

    pub fn with_linked_durable_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.linked_durable_run_id = Some(run_id.into());
        self
    }

    pub fn with_wait(mut self, wait: StepWait) -> Self {
        self.wait = wait;
        self
    }

    /// The controller-job submission envelope for this request.
    pub fn submission(&self) -> Result<Value> {
        self.validate()?;
        Ok(json!({
            "type": STEP_JOB_TYPE,
            "version": STEP_JOB_VERSION,
            "idempotency_key": self.key,
            "request": to_value(self)?,
        }))
    }

    /// Forward-compatible decode for persisted rows (see the type docs).
    fn parse(value: &Value) -> Result<Self> {
        let request: Self = serde_json::from_value(value.clone())
            .map_err(|error| invalid(format!("invalid step request: {error}")))?;
        request.validate()?;
        Ok(request)
    }

    /// Strict decode for a new submission: unknown fields are rejected so a
    /// misspelled or unsupported field never silently changes what runs.
    fn parse_admission(value: &Value) -> Result<Self> {
        reject_unknown_fields(value, STEP_JOB_REQUEST_FIELDS, "step request")?;
        if let Some(wait) = value.get("wait") {
            reject_unknown_fields(wait, STEP_WAIT_FIELDS, "step request wait")?;
        }
        Self::parse(value)
    }

    fn validate(&self) -> Result<()> {
        if self.schema != STEP_JOB_REQUEST_SCHEMA {
            return Err(invalid(format!(
                "step requests require schema `{STEP_JOB_REQUEST_SCHEMA}`"
            )));
        }
        if self.terminal_consumer.trim().is_empty() {
            return Err(invalid("step requests require a terminal consumer"));
        }
        if self.key.trim().is_empty() {
            return Err(invalid("step requests require a non-empty key"));
        }
        if !Path::new(&self.program).is_absolute() {
            return Err(invalid("step program must be an absolute path"));
        }
        if !Path::new(&self.cwd).is_absolute() {
            return Err(invalid("step working directory must be an absolute path"));
        }
        // The daemon's environment is never authority for a step. A plan that
        // asks to inherit or source values from it cannot be honored.
        if !self.env.source_env_bindings.is_empty()
            || !self.env.inherited_allowed_env_names.is_empty()
            || self.env.materialized_handoff.is_some()
        {
            return Err(invalid(
                "step environments carry public values and secret references only",
            ));
        }
        let policy = crate::redaction::RedactionPolicy::default();
        for name in self.env.public_env.keys() {
            validate_env_name(name)?;
            if policy.is_credential_key_name(name) {
                return Err(invalid(format!(
                    "step public environment cannot carry credential `{name}`; declare it as a secret reference"
                )));
            }
        }
        for secret in &self.env.secret_refs {
            validate_env_name(&secret.name)?;
            if self.env.public_env.contains_key(&secret.name) {
                return Err(invalid(format!(
                    "step environment declares `{}` as both public and secret",
                    secret.name
                )));
            }
        }
        if let Some(not_before) = &self.wait.not_before {
            chrono::DateTime::parse_from_rfc3339(not_before)
                .map_err(|error| invalid(format!("step wait.not_before is invalid: {error}")))?;
        }
        if self
            .wait
            .runner_eligible
            .as_deref()
            .is_some_and(|runner| runner.trim().is_empty())
        {
            return Err(invalid("step wait.runner_eligible must name a runner"));
        }
        Ok(())
    }

    fn public_projection(&self) -> Value {
        json!({
            "schema": self.schema,
            "terminal_consumer": self.terminal_consumer,
            "key": self.key,
            "program": self.program,
            "waits_for_children": self.waits_for_children,
            "args": crate::redaction::redact_argv(&self.args),
            "cwd": self.cwd,
            "env": {
                "public_env_names": self.env.public_env.keys().collect::<Vec<_>>(),
                "secret_ref_names": self.env.secret_refs.iter().map(|secret| &secret.name).collect::<Vec<_>>(),
            },
            "linked_durable_run_id": self.linked_durable_run_id,
            "root_durable_run_id": self.root_durable_run_id,
            "wait": self.wait,
        })
    }
}

fn validate_env_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('=') || name.contains('\0') {
        return Err(invalid(format!("invalid step environment name `{name}`")));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StepPhase {
    /// Admitted; waiting for `wait` to be satisfied.
    Waiting,
    /// Spawn started but the child identity was not yet checkpointed.
    Spawning,
    /// The child identity is durable and the child is supervised.
    Running,
    /// The child is gone.
    Exited,
}

/// One supervised child, identified by PID and kernel start identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepChild {
    pub pid: u32,
    pub start_identity: ProcessStartIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StepJobCheckpoint {
    schema: String,
    request: StepJobRequest,
    phase: StepPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child: Option<StepChild>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

impl StepJobCheckpoint {
    fn waiting(request: StepJobRequest) -> Self {
        Self {
            schema: STEP_JOB_CHECKPOINT_SCHEMA.to_string(),
            request,
            phase: StepPhase::Waiting,
            child: None,
            exit_code: None,
        }
    }

    fn parse(value: &Value) -> Result<Self> {
        let checkpoint: Self = serde_json::from_value(value.clone())
            .map_err(|error| invalid(format!("invalid step checkpoint: {error}")))?;
        if checkpoint.schema != STEP_JOB_CHECKPOINT_SCHEMA {
            return Err(invalid(format!(
                "step checkpoints require schema `{STEP_JOB_CHECKPOINT_SCHEMA}`"
            )));
        }
        checkpoint.request.validate()?;
        if checkpoint.phase == StepPhase::Running && checkpoint.child.is_none() {
            return Err(invalid("running step checkpoints require a child identity"));
        }
        Ok(checkpoint)
    }

    fn result(&self) -> Value {
        json!({
            "phase": self.phase,
            "exit_code": self.exit_code,
            "linked_durable_run_id": self.request.linked_durable_run_id,
        })
    }

    fn progress(&self) -> Value {
        json!({
            "phase": self.phase,
            "pid": self.child.as_ref().map(|child| child.pid),
            "exit_code": self.exit_code,
        })
    }
}

/// Private durable evidence delivered at least once until the consumer ACKs.
/// Consumers must make their domain mutations idempotent by job id/key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepTerminalDelivery {
    pub job_id: String,
    pub key: String,
    pub request: StepJobRequest,
    pub checkpoint: Option<Value>,
    pub linked_durable_run_id: Option<String>,
    pub child: Option<StepChild>,
    pub outcome: StepTerminalOutcome,
    pub error: Option<Value>,
    /// Known only when this daemon reaped the child itself.
    pub exit_code: Option<i32>,
    /// The child was stopped because the step job was cancelled.
    pub cancelled: bool,
    pub log_path: PathBuf,
    /// The step job's daemon scope; only that scope's daemon delivers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_scope: Option<crate::api_jobs::DaemonScope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepTerminalOutcome {
    Exited,
    /// Recovery settled the job from durable outcome authority, without an exit checkpoint.
    Succeeded,
    Failed,
    Cancelled,
    SpawnInterrupted,
}

/// Resolves secret references to values at spawn time. Implementations must
/// not read the daemon's own process environment.
pub type StepSecretResolver =
    Arc<dyn Fn(&[EnvSecretRef]) -> Result<Vec<(String, String)>> + Send + Sync>;
/// Reports whether a runner can accept work now, or its refusal reason.
pub type StepRunnerEligibility = Arc<dyn Fn(&str) -> std::result::Result<(), String> + Send + Sync>;
/// Lets the domain that owns a linked run reconcile it after its child exits.
pub type StepTerminalConsumer = Arc<dyn Fn(&StepTerminalDelivery) -> Result<Value> + Send + Sync>;

fn secret_resolver() -> &'static RwLock<Option<StepSecretResolver>> {
    static RESOLVER: OnceLock<RwLock<Option<StepSecretResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| RwLock::new(None))
}

fn runner_eligibility() -> &'static RwLock<Option<StepRunnerEligibility>> {
    static PROBE: OnceLock<RwLock<Option<StepRunnerEligibility>>> = OnceLock::new();
    PROBE.get_or_init(|| RwLock::new(None))
}

/// Probe the runner selected by an execution step before spawning its child.
pub(crate) fn admission_reason(request: &Value) -> Option<String> {
    let runner = request.pointer("/wait/runner_eligible")?.as_str()?;
    let probe = runner_eligibility()
        .read()
        .expect("step runner eligibility lock")
        .clone()?;
    probe(runner).err().filter(|reason| reason == "pressure")
}

pub type StepRunnerWaitContext = Arc<dyn Fn(&str) -> Value + Send + Sync>;
static WAIT_CONTEXT: OnceLock<StepRunnerWaitContext> = OnceLock::new();

pub fn register_step_runner_wait_context(context: StepRunnerWaitContext) {
    let _ = WAIT_CONTEXT.set(context);
}

pub fn step_runner_wait_context(runner: &str) -> Value {
    WAIT_CONTEXT
        .get()
        .map(|context| context(runner))
        .unwrap_or(Value::Null)
}

/// Registers, renews, or releases a waiting step with the runner's Lab daemon
/// (`POST /capacity/waiters`) and returns its reply: Lab-wide standing plus
/// holder budgets. `None` when the Lab is unreachable or predates the route,
/// in which case the wait keeps its controller-local ranking.
pub type StepRunnerLabWaiter =
    Arc<dyn Fn(&str, &crate::lab_capacity::RemoteWaiterHeartbeat) -> Option<Value> + Send + Sync>;
static LAB_WAITER: OnceLock<StepRunnerLabWaiter> = OnceLock::new();

pub fn register_step_runner_lab_waiter(waiter: StepRunnerLabWaiter) {
    let _ = LAB_WAITER.set(waiter);
}

/// A step's waiter reservation on its runner's Lab; dropping it releases the
/// reservation, and a lost controller's reservation expires on the Lab.
struct LabWaiterReservation {
    hook: StepRunnerLabWaiter,
    runner: String,
    beat: crate::lab_capacity::RemoteWaiterHeartbeat,
}

impl LabWaiterReservation {
    fn renew(&self) -> Option<Value> {
        (self.hook)(&self.runner, &self.beat).filter(|reply| reply["registered"] == true)
    }
}

impl Drop for LabWaiterReservation {
    fn drop(&mut self) {
        let release = crate::lab_capacity::RemoteWaiterHeartbeat {
            release: true,
            ..self.beat.clone()
        };
        let _ = (self.hook)(&self.runner, &release);
    }
}

/// What a runner wait reports from its Lab reservation reply: Lab-wide
/// position when the Lab ranked it, else the controller-local one, and an
/// upper-bound ETA from the Lab holders' declared budgets.
fn lab_wait_standing(
    lab: Option<&Value>,
    local: Option<crate::lab_capacity::RunnerWaitStanding>,
) -> Value {
    let lab_position = lab
        .and_then(|reply| reply["queue_position"].as_u64())
        .map(|position| position as usize);
    let position = lab_position.or(local.map(|standing| standing.queue_position));
    let eta = lab
        .zip(position)
        .and_then(|(reply, position)| crate::lab_capacity::eta_from_step_queue(reply, position));
    json!({
        "queue_position": position,
        "waiters_ahead": lab
            .and_then(|reply| reply["waiters_ahead"].as_u64())
            .map(|ahead| ahead as usize)
            .or(local.map(|standing| standing.waiters_ahead)),
        "queue_scope": if lab_position.is_some() { "lab" } else { "controller" },
        "may_proceed": lab
            .and_then(|reply| reply["may_proceed"].as_bool())
            .unwrap_or(true),
        "eta_seconds": eta,
        "eta_kind": eta.map(|_| "upper_bound_from_declared_budgets"),
        "effective_budget": lab.map(|reply| reply["effective_budget"].clone()),
        "active_slots": lab.map(|reply| reply["active_slots"].clone()),
        "load_average_1m": lab.map(|reply| reply["load_average_1m"].clone()),
    })
}

fn terminal_consumers() -> &'static RwLock<BTreeMap<String, StepTerminalConsumer>> {
    static CONSUMERS: OnceLock<RwLock<BTreeMap<String, StepTerminalConsumer>>> = OnceLock::new();
    CONSUMERS.get_or_init(|| RwLock::new(BTreeMap::new()))
}

pub fn register_step_secret_resolver(resolver: StepSecretResolver) {
    *secret_resolver()
        .write()
        .expect("step secret resolver lock") = Some(resolver);
}

pub fn register_step_runner_eligibility(probe: StepRunnerEligibility) {
    *runner_eligibility()
        .write()
        .expect("step runner eligibility lock") = Some(probe);
}

pub fn register_step_terminal_consumer(id: &str, consumer: StepTerminalConsumer) {
    terminal_consumers()
        .write()
        .expect("step terminal consumers lock")
        .insert(id.to_string(), consumer);
}

pub(crate) fn deliver_terminal(delivery: &StepTerminalDelivery) -> Result<Value> {
    let consumer = terminal_consumers()
        .read()
        .expect("step terminal consumers lock")
        .get(&delivery.request.terminal_consumer)
        .cloned()
        .ok_or_else(|| {
            invalid(format!(
                "terminal consumer `{}` is not registered",
                delivery.request.terminal_consumer
            ))
        })?;
    consumer(delivery)
}

/// Consumer for steps whose result needs no domain mutation.
pub const STEP_RECEIPT_CONSUMER: &str = "homeboy.step.receipt";

/// Register the `step` controller-job driver. Idempotent.
pub fn register_step_job_driver() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        register_step_terminal_consumer(
            STEP_RECEIPT_CONSUMER,
            Arc::new(|delivery| Ok(json!({"job_id": delivery.job_id}))),
        );
        controller_job_driver::register_controller_job_driver(Arc::new(StepJobDriver))
            .expect("register step controller job driver");
    });
}

/// Directory holding one step job's log and exit record.
pub fn step_job_dir(job_id: &str) -> Result<PathBuf> {
    Ok(crate::paths::artifact_root()?
        .join("jobs")
        .join(crate::paths::sanitize_path_segment(job_id)))
}

/// The exit code recorded by a step's child wrapper, if any.
pub fn recorded_step_exit_code(job_id: &str) -> Option<i32> {
    let bytes = std::fs::read(step_job_dir(job_id).ok()?.join(STEP_EXIT_FILE)).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    (value["schema"] == STEP_EXIT_SCHEMA)
        .then(|| value["exit_code"].as_i64())
        .flatten()
        .and_then(|code| i32::try_from(code).ok())
}

/// The step's usage block: its job cgroup's counters when it ran in one,
/// otherwise its process tree's CPU totals reaped by the wrapper at finish
/// (`times`), marked `estimated`.
pub fn recorded_step_usage(
    job_id: &str,
    wall_ms: u64,
) -> Option<homeboy_engine_primitives::resource_usage::ResourceUsage> {
    let dir = step_job_dir(job_id).ok()?;
    if let Some(mut usage) = std::fs::read(dir.join(STEP_USAGE_FILE))
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<homeboy_engine_primitives::resource_usage::ResourceUsage>(
                &bytes,
            )
            .ok()
        })
    {
        usage.wall_ms = wall_ms;
        return Some(usage);
    }
    let bytes = std::fs::read(dir.join(STEP_EXIT_FILE)).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if value["schema"] != STEP_EXIT_SCHEMA {
        return None;
    }
    let (user, system) = value["cpu"].as_str()?.split_once(' ')?;
    Some(
        homeboy_engine_primitives::resource_usage::ResourceUsage::from_process_tree(
            shell_times_usec(user)?,
            shell_times_usec(system)?,
            wall_ms,
        ),
    )
}

/// A POSIX `times` duration (`1m2.345s`, `0m0.004000s`) in microseconds.
fn shell_times_usec(raw: &str) -> Option<u64> {
    let (minutes, seconds) = raw.trim().strip_suffix('s')?.split_once('m')?;
    let seconds = minutes.parse::<f64>().ok()? * 60.0 + seconds.parse::<f64>().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| (seconds * 1_000_000.0).round() as u64)
}

/// The step's combined child log, when it exists: the evidence behind a
/// recorded step failure cause.
pub fn recorded_step_log_path(job_id: &str) -> Option<PathBuf> {
    let path = step_job_dir(job_id).ok()?.join(STEP_LOG_FILE);
    path.is_file().then_some(path)
}

/// The cause a step's child reported in its log, if it failed with one.
pub fn recorded_step_failure_cause(job_id: &str) -> Option<String> {
    let log = std::fs::read_to_string(step_job_dir(job_id).ok()?.join(STEP_LOG_FILE)).ok()?;
    child_failure_cause(&log)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepChildAuthorization {
    Waiting,
    Authorized,
    Inert,
}

/// Read only: a child may act only after its exact kernel identity is durable.
pub fn step_child_authorization(
    path: &Path,
    job_id: uuid::Uuid,
    child: &StepChild,
) -> Result<StepChildAuthorization> {
    let Some((status, cancelled, value)) =
        crate::api_jobs::JobStore::controller_checkpoint_at_path(path, job_id)?
    else {
        return Ok(StepChildAuthorization::Inert);
    };
    if status.is_terminal() || cancelled {
        return Ok(StepChildAuthorization::Inert);
    }
    let checkpoint = StepJobCheckpoint::parse(&value)?;
    Ok(match checkpoint.phase {
        StepPhase::Spawning => StepChildAuthorization::Waiting,
        StepPhase::Running if checkpoint.child.as_ref() == Some(child) => {
            StepChildAuthorization::Authorized
        }
        _ => StepChildAuthorization::Inert,
    })
}

/// The ancestors of this process, nearest first, whose kernel identity is
/// readable.
///
/// A step child's work inherits the step environment, so every process it
/// runs reaches the start gate too: its own re-execution (a Cook delegates to
/// its pinned controller runtime), and any tool it runs that invokes Homeboy
/// in turn. Such a process acts on the authorized child's behalf; the
/// relationship is proven by PID and start identity, not by an inheritable
/// value.
fn ancestor_step_children() -> Vec<StepChild> {
    #[cfg(unix)]
    {
        crate::process::ancestor_pids(std::process::id())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|pid| {
                let start_identity = crate::process::process_start_identity(pid).ok()??;
                Some(StepChild {
                    pid,
                    start_identity,
                })
            })
            .collect()
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// Called at child entry before side effects. Foreground invocations pass;
/// a step child waits for Running, or returns false to exit without work.
/// A descendant of the authorized child acts on its behalf and passes as well.
pub fn await_step_child_start() -> Result<bool> {
    let Some(job_id) = std::env::var_os(STEP_JOB_ID_ENV) else {
        return Ok(true);
    };
    let job_id = uuid::Uuid::parse_str(&job_id.to_string_lossy())
        .map_err(|_| invalid("invalid step job id"))?;
    let path = std::env::var_os(STEP_JOB_STORE_ENV)
        .ok_or_else(|| invalid("step child lacks supervising store"))?;
    let pid = std::process::id();
    let start_identity = crate::process::process_start_identity(pid)
        .map_err(invalid)?
        .ok_or_else(|| invalid("step child lacks start identity"))?;
    let child = StepChild {
        pid,
        start_identity,
    };
    loop {
        match step_child_authorization(Path::new(&path), job_id, &child)? {
            StepChildAuthorization::Authorized => return Ok(true),
            StepChildAuthorization::Inert => {
                for ancestor in ancestor_step_children() {
                    if step_child_authorization(Path::new(&path), job_id, &ancestor)?
                        == StepChildAuthorization::Authorized
                    {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
            StepChildAuthorization::Waiting => std::thread::sleep(SUPERVISE_POLL),
        }
    }
}

pub struct StepJobDriver;

impl ControllerJobDriver for StepJobDriver {
    fn job_type(&self) -> &'static str {
        STEP_JOB_TYPE
    }

    fn version(&self) -> u32 {
        STEP_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(StepJobRequest::parse(request)?.public_projection())
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        Ok(json!({
            "phase": progress.get("phase").cloned().unwrap_or(Value::Null),
            "pid": progress.get("pid").cloned().unwrap_or(Value::Null),
            "runner_eligible": progress.get("runner_eligible").cloned().unwrap_or(Value::Null),
            "reason": progress.get("reason").cloned().unwrap_or(Value::Null),
            "waiting_since": progress.get("waiting_since").cloned().unwrap_or(Value::Null),
            "builds": progress.get("builds").cloned().unwrap_or(Value::Null),
            "exit_code": progress.get("exit_code").cloned().unwrap_or(Value::Null),
            "waiting_for": progress.get("waiting_for").cloned().unwrap_or(Value::Null),
            "queue_position": progress.get("queue_position").cloned().unwrap_or(Value::Null),
            "waiters_ahead": progress.get("waiters_ahead").cloned().unwrap_or(Value::Null),
            "priority": progress.get("priority").cloned().unwrap_or(Value::Null),
            "eta_seconds": progress.get("eta_seconds").cloned().unwrap_or(Value::Null),
            "queue_scope": progress.get("queue_scope").cloned().unwrap_or(Value::Null),
            "estimated_start_at": progress.get("estimated_start_at").cloned().unwrap_or(Value::Null),
            "effective_budget": progress.get("effective_budget").cloned().unwrap_or(Value::Null),
            "active_slots": progress.get("active_slots").cloned().unwrap_or(Value::Null),
            "load_average_1m": progress.get("load_average_1m").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        Ok(json!({
            "phase": result.get("phase").cloned().unwrap_or(Value::Null),
            "exit_code": result.get("exit_code").cloned().unwrap_or(Value::Null),
            "linked_durable_run_id": result.get("linked_durable_run_id").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_error(&self, error: &Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: crate::redaction::redact_string(&error.message),
            data: json!({
                "code": error.code.as_str(),
                "diagnostic": controller_job_driver::public_io_diagnostic(error),
                "exit_code": error.details.get("exit_code").cloned().unwrap_or(Value::Null),
                "step_outcome": error.details.get("step_outcome").cloned().unwrap_or(Value::Null),
                "wait_reason": error.details.get("wait_reason").cloned().unwrap_or(Value::Null),
                "runner_id": error.details.get("runner_id").cloned().unwrap_or(Value::Null),
                "step_child": error.details.get("step_child").cloned().unwrap_or(Value::Null),
            }),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        StepJobRequest::parse_admission(request).map(|_| ())
    }

    fn linked_durable_run_id(&self, request: &Value) -> Option<String> {
        StepJobRequest::parse(request)
            .ok()?
            .linked_durable_run_id
            .filter(|run_id| !run_id.trim().is_empty())
    }

    fn execution_owner(
        &self,
        request: &Value,
        checkpoint: Option<&Value>,
    ) -> Result<Option<ControllerJobExecutionOwner>> {
        let Some(checkpoint) = checkpoint else {
            StepJobRequest::parse(request)?;
            return Ok(Some(ControllerJobExecutionOwner::None));
        };
        let checkpoint = StepJobCheckpoint::parse(checkpoint)?;
        Ok(Some(match (checkpoint.phase, checkpoint.child) {
            (StepPhase::Running, Some(child)) => {
                ControllerJobExecutionOwner::supervised(child.pid, child.start_identity)
            }
            _ => ControllerJobExecutionOwner::None,
        }))
    }

    fn recovery_checkpoint(&self, request: &Value) -> Result<Option<Value>> {
        Ok(Some(to_value(StepJobCheckpoint::waiting(
            StepJobRequest::parse(request)?,
        ))?))
    }

    fn prepare(&self, request: Value) -> Result<Value> {
        to_value(StepJobCheckpoint::waiting(StepJobRequest::parse(&request)?))
    }

    fn execute(&self, prepared: Value, job: ControllerJobHandle) -> Result<Value> {
        run_step(StepJobCheckpoint::parse(&prepared)?, &job)
    }

    fn resume(&self, checkpoint: Value, job: ControllerJobHandle) -> Result<Value> {
        run_step(StepJobCheckpoint::parse(&checkpoint)?, &job)
    }

    fn cancel(&self, prepared: &Value) -> Result<()> {
        // A live execute loop observes the durable cancellation itself and
        // stops the child it owns. This path covers a recovered checkpoint
        // that names a child no live loop is supervising.
        let checkpoint = StepJobCheckpoint::parse(prepared)?;
        match (checkpoint.phase, checkpoint.child) {
            (StepPhase::Running, Some(child)) => {
                stop_child(&child, adopted_cgroup(&child).as_ref())
            }
            _ => Ok(()),
        }
    }
}

fn write_launch_error(path: &std::path::Path, error: &Error) {
    use std::io::Write;
    let text = serde_json::json!({"message": error.message, "details": error.details}).to_string();
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        if writeln!(log, "{text}").is_ok() {
            let _ = log.flush();
            return;
        }
    }
    println!("{text}");
}

fn run_step(mut checkpoint: StepJobCheckpoint, job: &ControllerJobHandle) -> Result<Value> {
    let job_id = job.job_id();
    match checkpoint.phase {
        StepPhase::Exited => return finish(&job_id, checkpoint),
        StepPhase::Spawning => {
            // The daemon stopped between starting a spawn and recording the
            // child. Its identity is unknowable, so never spawn a second one.
            let mut error = Error::internal_unexpected(
                "step spawn was interrupted before its child identity was recorded",
            );
            error.details["step_outcome"] = json!("spawn_interrupted");
            return Err(error);
        }
        StepPhase::Running => {
            let child = checkpoint.child.clone().expect("validated running child");
            job.progress(checkpoint.progress())?;
            // Recovery owns this job's named cgroup, not whichever cgroup a
            // reused or migrated PID might currently inhabit.
            let cgroup = JobCgroup::for_job(&job_id).ok().flatten();
            supervise_adopted(&child, cgroup.as_ref(), job)?;
            stop_finished_cgroup(&job_id, cgroup.as_ref());
            checkpoint.exit_code = recorded_step_exit_code(&job_id);
            checkpoint.phase = StepPhase::Exited;
            job.checkpoint(to_value(&checkpoint)?)?;
            return finish(&job_id, checkpoint);
        }
        StepPhase::Waiting => {}
    }

    // Preserve a durable wait observation across daemon adoption.
    if job
        .job()
        .store()
        .step_runner_wait(&checkpoint.request.key)?
        .is_none()
    {
        job.progress(checkpoint.progress())?;
    }
    if !await_wait(&checkpoint.request, job)? {
        return Ok(checkpoint.result());
    }

    // Launcher/continuation steps can submit execution steps themselves. They
    // must not hold heavy-work slots while waiting on their own children.
    let limit = checkpoint
        .request
        .env
        .public_env
        .get("HOMEBOY_LAB_CONCURRENCY_LIMIT")
        .and_then(|limit| limit.parse().ok());
    let owner = step_owner(&checkpoint.request);
    let public_env = &checkpoint.request.env.public_env;
    let claim = crate::lab_capacity::StepClaim {
        owner,
        priority: step_priority(&checkpoint.request),
        budget: public_env
            .get(crate::lab_capacity::LAB_BUDGET_ENV)
            .and_then(|seconds| seconds.trim().parse::<u64>().ok())
            .filter(|seconds| *seconds > 0)
            .map(Duration::from_secs),
        job_id: Some(job_id.as_str()),
    };
    let capacity_permit = if !checkpoint.request.waits_for_children {
        let mut last_standing = None;
        let Some(permit) =
            crate::lab_capacity::acquire_step(limit, claim, |standing, capacity| {
                if job.is_cancelled() {
                    return Ok(false);
                }
                // Re-publish when the standing (position, holders, ETA) changes.
                if last_standing.as_ref() != Some(standing) {
                    job.progress(json!({
                        "waiting_for": "lab_capacity",
                        "queue_position": standing.queue_position,
                        "waiters_ahead": standing.waiters_ahead,
                        "active_slots": standing.active_slots,
                        "priority": standing.priority,
                        "owner_slot_cap": standing.owner_slot_cap,
                        "load_average_1m": capacity.load_average_1m,
                        "effective_budget": capacity.effective_budget,
                        "eta_seconds": standing.eta_seconds,
                        "eta_kind": "upper_bound_from_declared_budgets",
                    }))?;
                    last_standing = Some(standing.clone());
                }
                Ok(true)
            })?
        else {
            return Ok(checkpoint.result());
        };
        Some(permit)
    } else {
        None
    };
    let request = checkpoint.request.clone();
    let secrets = resolve_secrets(&request.env.secret_refs)?;
    let dir = step_job_dir(&job_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| Error::internal_io(error.to_string(), Some(dir.display().to_string())))?;
    let log_path = dir.join(STEP_LOG_FILE);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(log_path.display().to_string()))
        })?;
    let log_err = log.try_clone().map_err(|error| {
        Error::internal_io(error.to_string(), Some(log_path.display().to_string()))
    })?;
    // With a delegated cgroup subtree the step runs in its own
    // `jobs/<job-id>/` cgroup; dropping it kills and removes the cgroup.
    let cgroup = JobCgroup::create_unlimited(&job_id).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create step job cgroup".to_string()),
        )
    })?;

    checkpoint.phase = StepPhase::Spawning;
    job.checkpoint(to_value(&checkpoint)?)?;

    let mut command = step_command(&request, &dir);
    command
        .current_dir(&request.cwd)
        .env_clear()
        .envs(&request.env.public_env)
        .envs(secrets)
        .env(STEP_JOB_ID_ENV, &job_id)
        .env(
            STEP_JOB_STORE_ENV,
            job.job()
                .store()
                .persistence_path()
                .ok_or_else(|| invalid("step requires a durable job store"))?,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // The child leads its own session: a daemon restart must not take the
    // step down, and cancellation addresses the child's whole tree.
    crate::process::detach_from_caller_session(&mut command);
    if let Some(cgroup) = &cgroup {
        cgroup.join_before_exec(&mut command);
    }
    if let Some(cpus) = capacity_permit.as_ref().and_then(|permit| {
        crate::lab_capacity::cpu_slice(&crate::lab_capacity::allowed_cpus(), limit, permit.slot())
    }) {
        crate::lab_capacity::pin_command(&mut command, &cpus);
        job.progress(json!({
            "phase": "cpu_affinity",
            "cpu_affinity": { "cpus": cpus, "slot": capacity_permit.as_ref().map(|permit| permit.slot()), "concurrency_limit": limit },
        }))?;
    }
    // Preserve launch errors for a missing program rather than launching a
    // wrapper that can only report shell status 127.
    let program = launchable_step_program(&request.program);
    let process = std::fs::metadata(&program)
        .and_then(|_| command.spawn())
        .map_err(|error| {
            let mut error = Error::internal_io(
                error.to_string(),
                Some(format!("spawn step program {program}")),
            );
            error.details["classification"] = serde_json::json!("detached_launch_failed");
            write_launch_error(&log_path, &error);
            error
        })?;
    let mut process = crate::child_reaper::ReapingChild::from(process);
    // Capture the start identity while the child is still unreaped.
    let start_identity = match crate::process::process_start_identity(process.id()) {
        Ok(Some(identity)) => identity,
        other => {
            let _ = crate::process::terminate_process_tree(
                process.id(),
                crate::process::PROCESS_TREE_SIGTERM_GRACE,
            );
            let _ = process.wait();
            return Err(Error::internal_unexpected(format!(
                "step child start identity is unavailable: {other:?}"
            )));
        }
    };
    let child = StepChild {
        pid: process.id(),
        start_identity,
    };
    checkpoint.phase = StepPhase::Running;
    checkpoint.child = Some(child.clone());
    if let Err(mut error) = job.checkpoint(to_value(&checkpoint)?) {
        let _ = crate::process::terminate_process_tree(
            child.pid,
            crate::process::PROCESS_TREE_SIGTERM_GRACE,
        );
        let _ = process.wait();
        error.details["step_child"] = to_value(&child)?;
        return Err(error);
    }
    job.progress(checkpoint.progress())?;

    loop {
        if job.is_cancelled() {
            record_cgroup_usage(&job_id, cgroup.as_ref());
            let _ = stop_child(&child, cgroup.as_ref());
            let _ = process.wait();
            checkpoint.phase = StepPhase::Exited;
            let _ = job.checkpoint(to_value(&checkpoint)?);
            return Ok(checkpoint.result());
        }
        match process.try_wait() {
            Ok(Some(status)) => {
                checkpoint.exit_code = status.code();
                break;
            }
            Ok(None) => std::thread::sleep(SUPERVISE_POLL),
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("observe step child".to_string()),
                ))
            }
        }
    }
    // Descendants the child left behind end with their step.
    stop_finished_cgroup(&job_id, cgroup.as_ref());
    checkpoint.phase = StepPhase::Exited;
    job.checkpoint(to_value(&checkpoint)?)?;
    job.progress(checkpoint.progress())?;
    finish(&job_id, checkpoint)
}

fn finish(job_id: &str, checkpoint: StepJobCheckpoint) -> Result<Value> {
    match checkpoint.exit_code {
        Some(0) => Ok(checkpoint.result()),
        Some(code) => {
            let cause = step_job_dir(job_id)
                .ok()
                .and_then(|dir| std::fs::read_to_string(dir.join(STEP_LOG_FILE)).ok())
                .and_then(|log| child_failure_cause(&log));
            let mut error = Error::internal_unexpected(match &cause {
                Some(cause) => format!("step child exited with status {code}: {cause}"),
                None => format!("step child exited with status {code}"),
            });
            error.details["exit_code"] = json!(code);
            Err(error)
        }
        None => {
            let mut error = Error::internal_unexpected("step child exit status is unknown");
            error.details["step_outcome"] = json!("exit_status_unknown");
            Err(error)
        }
    }
}

/// The cause a failed step child reported (#16273). A Homeboy command child
/// ends its log with a command-result envelope whose summary names the cause;
/// any other child is described by the tail of what it wrote.
fn child_failure_cause(log: &str) -> Option<String> {
    const TAIL_LINES: usize = 3;
    const MAX_CHARS: usize = 600;
    let envelope_start = log
        .rfind("\n{\n")
        .map(|index| index + 1)
        .or_else(|| log.starts_with("{\n").then_some(0));
    let envelope =
        envelope_start.and_then(|start| serde_json::from_str::<Value>(&log[start..]).ok());
    let output_tail = |text: &str| {
        let lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        let tail = &lines[lines.len().saturating_sub(TAIL_LINES)..];
        (!tail.is_empty()).then(|| tail.join(" | "))
    };
    let cause = match envelope.as_ref() {
        Some(value) => {
            let summary = value["summary"]
                .as_str()
                .or_else(|| value["diagnostics"]["message"].as_str())
                .map(str::to_string);
            // A command that failed without naming a cause gets the generic
            // "exited N without reporting a failure cause" fallback. That is
            // not a cause: whatever the child wrote before its envelope (its
            // stderr) is the evidence that explains the exit.
            if envelope_declares_no_cause(value) {
                let before = envelope_start.map_or("", |start| &log[..start]);
                match (summary, output_tail(before)) {
                    (Some(summary), Some(tail)) => Some(format!("{summary}; last output: {tail}")),
                    (summary, tail) => summary.or(tail),
                }
            } else {
                summary
            }
        }
        None => output_tail(log),
    }?;
    let cause = cause.trim();
    if cause.is_empty() {
        return None;
    }
    Some(if cause.chars().count() > MAX_CHARS {
        let kept = cause.chars().take(MAX_CHARS).collect::<String>();
        format!("{kept}…")
    } else {
        cause.to_string()
    })
}

/// The envelope carries only the shared generic fallback for an unnamed
/// nonzero exit, not a cause the command declared.
fn envelope_declares_no_cause(envelope: &Value) -> bool {
    envelope["diagnostics"]["code"] == "command.failed"
        && envelope["data"].get("failure").is_none()
        && envelope["diagnostics"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("without reporting a failure cause"))
}

/// Returns `false` when cancelled, or when a declared wait (time or runner)
/// would outlive its linked durable run. A step with nothing to wait for goes
/// straight to its child, which owns authorization: recovery steps exist
/// precisely for runs that are already terminal.
fn await_wait(request: &StepJobRequest, job: &ControllerJobHandle) -> Result<bool> {
    let probe = runner_eligibility()
        .read()
        .expect("step runner eligibility lock")
        .clone();
    let started = Instant::now();
    await_wait_with(
        request,
        job,
        probe,
        || started.elapsed(),
        std::thread::sleep,
        |run_id| {
            crate::api_jobs::agent_task_terminal_recovery::linked_durable_run_state(run_id)
                == Some(crate::api_jobs::DaemonLinkedDurableRunState::Terminal)
        },
    )
}

fn await_wait_with(
    request: &StepJobRequest,
    job: &ControllerJobHandle,
    probe: Option<StepRunnerEligibility>,
    now: impl Fn() -> Duration,
    sleep: impl Fn(Duration),
    linked_terminal: impl Fn(&str) -> bool,
) -> Result<bool> {
    let wait = &request.wait;
    if wait.is_empty() {
        return Ok(!job.is_cancelled());
    }
    let not_before = wait
        .not_before
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|error| invalid(format!("step wait.not_before is invalid: {error}")))?;
    let runner_probe = wait
        .runner_eligible
        .as_deref()
        .map(|runner| {
            probe.map(|probe| (runner, probe)).ok_or_else(|| {
                Error::internal_unexpected(
                    "no runner eligibility probe is registered for step waits",
                )
            })
        })
        .transpose()?;
    // Rank this waiter among the same runner's waiters on this controller so a
    // higher lane takes a freed slot first and status can report a position.
    let runner_ticket = runner_probe
        .as_ref()
        .map(|(runner, _)| crate::lab_capacity::join_runner_wait(runner, step_priority(request)));
    let mut last_estimated_start: Option<chrono::DateTime<chrono::Utc>> = None;
    let wait_cap = if step_priority(request) == crate::lab_capacity::LabPriority::High {
        RUNNER_WAIT_HIGH_CAP
    } else {
        RUNNER_WAIT_CAP
    };
    let mut last_position = None;
    let budget = runner_wait_budget();
    let mut blocked_since = None;
    let prior_wait = job.job().store().step_runner_wait(&request.key)?;
    let waiting_since = prior_wait
        .as_ref()
        .and_then(|wait| wait["waiting_since"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let prior_elapsed = prior_wait
        .as_ref()
        .and_then(|wait| wait["waiting_since"].as_str())
        .and_then(|since| chrono::DateTime::parse_from_rfc3339(since).ok())
        .and_then(|since| {
            (chrono::Utc::now() - since.with_timezone(&chrono::Utc))
                .to_std()
                .ok()
        })
        .unwrap_or_default();
    // The Lab ranks this waiter against its own steps and other controllers'
    // waiters by lane, owner share, then the Cook's first-wait time (stable
    // across controller restarts), and reports holder budgets for an ETA.
    let lab_waiter = runner_probe.as_ref().and_then(|(runner, _)| {
        LAB_WAITER.get().map(|hook| LabWaiterReservation {
            hook: hook.clone(),
            runner: runner.to_string(),
            beat: crate::lab_capacity::RemoteWaiterHeartbeat {
                waiter_id: request.key.clone(),
                owner: step_owner(request).to_string(),
                priority: step_priority(request),
                first_waited_at: Some(waiting_since.clone()),
                release: false,
            },
        })
    });
    let mut next_probe = now();
    let mut backoff = RUNNER_WAIT_INITIAL;
    let mut last_observation: Option<std::result::Result<(), String>> = None;
    loop {
        if job.is_cancelled()
            || (!request.allows_terminal_linked_run
                && request
                    .linked_durable_run_id
                    .as_deref()
                    .is_some_and(&linked_terminal))
        {
            return Ok(false);
        }
        let time_ready = not_before.is_none_or(|instant| chrono::Utc::now() >= instant);
        if let Some((runner, probe)) = &runner_probe {
            if let Some(Err(reason)) = &last_observation {
                if blocked_since.is_some_and(|since| now() + prior_elapsed >= since + budget)
                    && runner_wait_is_bounded(reason)
                {
                    let mut error = Error::internal_unexpected(format!(
                        "runner {runner} eligibility wait budget exhausted: {reason}"
                    ));
                    error.details["step_outcome"] = json!("runner_eligibility_timeout");
                    error.details["runner_id"] = json!(runner);
                    error.details["wait_reason"] = json!(reason);
                    return Err(error);
                }
            }
            if now() >= next_probe {
                let standing = runner_ticket.as_ref().map(|ticket| ticket.standing());
                let lab_reply = lab_waiter.as_ref().and_then(LabWaiterReservation::renew);
                let queue = lab_wait_standing(lab_reply.as_ref(), standing);
                // A free slot belongs to a higher-lane waiter first, here or on
                // another controller waiting on the same Lab.
                let observation = match probe(runner) {
                    Ok(())
                        if standing.is_some_and(|standing| !standing.may_proceed)
                            || queue["may_proceed"] == false =>
                    {
                        Err("capacity_reached".to_string())
                    }
                    observation => observation,
                };
                if observation
                    .as_ref()
                    .err()
                    .is_some_and(|reason| runner_wait_is_bounded(reason))
                {
                    blocked_since.get_or_insert_with(&now);
                } else if observation.is_ok() {
                    blocked_since = None;
                }
                // A different refusal is still ineligible: don't reset backoff
                // until eligibility itself changes.
                if last_observation.as_ref().is_some_and(
                    |last: &std::result::Result<(), String>| last.is_ok() != observation.is_ok(),
                ) {
                    backoff = RUNNER_WAIT_INITIAL;
                }
                let position = queue["queue_position"].as_u64();
                let estimated_start = queue["eta_seconds"]
                    .as_u64()
                    .map(|seconds| chrono::Utc::now() + chrono::Duration::seconds(seconds as i64));
                // Re-publish when the estimated start moves by over a minute.
                let eta_moved = match (estimated_start, last_estimated_start) {
                    (Some(now), Some(last)) => (now - last).num_seconds().abs() > 60,
                    (now, last) => now.is_some() != last.is_some(),
                };
                if last_observation.as_ref() != Some(&observation)
                    || last_position != position
                    || eta_moved
                {
                    last_estimated_start = estimated_start;
                    if let Err(reason) = &observation {
                        job.progress(json!({
                            "phase": "waiting",
                            "runner_eligible": runner,
                            "reason": reason,
                            "waiting_since": waiting_since,
                            "builds": step_runner_wait_context(runner),
                            "queue_position": position,
                            "waiters_ahead": queue["waiters_ahead"],
                            "queue_scope": queue["queue_scope"],
                            "eta_seconds": queue["eta_seconds"],
                            "eta_kind": queue["eta_kind"],
                            "estimated_start_at": estimated_start.map(|at| at.to_rfc3339()),
                            "effective_budget": queue["effective_budget"],
                            "active_slots": queue["active_slots"],
                            "load_average_1m": queue["load_average_1m"],
                            "priority": step_priority(request),
                            "next_attempt_at": (chrono::Utc::now()
                                + chrono::Duration::from_std(backoff)
                                    .expect("runner backoff fits chrono"))
                            .to_rfc3339(),
                        }))?;
                    }
                }
                // The Lab says this waiter is next: re-probe within seconds so
                // the slot it holds for us is claimed, not left idle.
                let next_in_line = queue["queue_scope"] == "lab" && queue["may_proceed"] == true;
                let cap = if next_in_line {
                    wait_cap.min(RUNNER_WAIT_HIGH_CAP)
                } else {
                    wait_cap
                };
                last_position = position;
                last_observation = Some(observation);
                next_probe = now() + backoff.min(cap);
                backoff = (backoff * 2).min(cap);
            }
        }
        let runner_ready = runner_probe.is_none()
            || last_observation
                .as_ref()
                .is_some_and(|result| result.is_ok());
        if time_ready && runner_ready {
            return Ok(true);
        }
        let mut delay = next_probe.saturating_sub(now());
        if let Some(since) = blocked_since.filter(|_| {
            last_observation
                .as_ref()
                .and_then(|value| value.as_ref().err())
                .is_some_and(|reason| runner_wait_is_bounded(reason))
        }) {
            delay = delay.min((since + budget).saturating_sub(now() + prior_elapsed));
        }
        if !time_ready {
            delay = if runner_probe.is_some() {
                delay.min(WAIT_POLL)
            } else {
                WAIT_POLL
            };
        }
        sleep(delay);
    }
}

fn resolve_secrets(secret_refs: &[EnvSecretRef]) -> Result<Vec<(String, String)>> {
    if secret_refs.is_empty() {
        return Ok(Vec::new());
    }
    let resolver = secret_resolver()
        .read()
        .expect("step secret resolver lock")
        .clone()
        .ok_or_else(|| {
            Error::internal_unexpected("no secret resolver is registered for step environments")
        })?;
    let resolved = resolver(secret_refs)?;
    let declared = secret_refs
        .iter()
        .map(|secret| secret.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let values = resolved
        .into_iter()
        .filter(|(name, _)| declared.contains(name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let missing = declared
        .iter()
        .filter(|name| !values.contains_key(**name))
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let mut error = Error::validation_invalid_argument(
            "step.env.secret_refs",
            format!(
                "step secret references could not be resolved: {}",
                missing.join(", ")
            ),
            None,
            Some(vec![
                "Declare each secret in the agent-task secret configuration; a step never reads the daemon environment.".to_string(),
            ]),
        );
        error.details["missing_secret_env"] = json!(missing);
        return Err(error);
    }
    Ok(values.into_iter().collect())
}

/// Supervise a re-adopted child that this daemon did not spawn and cannot reap.
fn supervise_adopted(
    child: &StepChild,
    cgroup: Option<&JobCgroup>,
    job: &ControllerJobHandle,
) -> Result<()> {
    loop {
        if job.is_cancelled() {
            return stop_child(child, cgroup);
        }
        match child_state(child) {
            ProcessIdentityState::Dead | ProcessIdentityState::IdentityMismatch => return Ok(()),
            ProcessIdentityState::Live | ProcessIdentityState::Unverifiable => {
                std::thread::sleep(SUPERVISE_POLL)
            }
        }
    }
}

fn child_state(child: &StepChild) -> ProcessIdentityState {
    crate::process::process_identity_state_with_start_identity(
        child.pid,
        None,
        Some(&child.start_identity),
    )
}

/// The job cgroup a re-adopted child still runs in, when it is still the
/// admitted child and this daemon owns a delegated cgroup subtree.
fn adopted_cgroup(child: &StepChild) -> Option<JobCgroup> {
    if child_state(child) != ProcessIdentityState::Live {
        return None;
    }
    JobCgroup::of_process(child.pid).ok().flatten()
}

/// Stop the step: with its own cgroup, `cgroup.kill` reaches every member
/// whatever its session or process group, then the emptied cgroup is removed.
/// Without one, terminate the child's process tree.
fn stop_child(child: &StepChild, cgroup: Option<&JobCgroup>) -> Result<()> {
    match cgroup {
        Some(cgroup) => cgroup.stop().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("stop step child {} cgroup", child.pid)),
            )
        }),
        None => stop_child_tree(child),
    }
}

/// Record the step's exact usage from its cgroup's counters, which go with
/// the cgroup (#18461). Steps without a cgroup keep the wrapper's estimate.
fn record_cgroup_usage(job_id: &str, cgroup: Option<&JobCgroup>) {
    let Some(usage) = cgroup.and_then(|cgroup| cgroup.usage(0)) else {
        return;
    };
    let written = step_job_dir(job_id).and_then(|dir| {
        homeboy_engine_primitives::local_files::write_json_file(&dir.join(STEP_USAGE_FILE), &usage)
    });
    if let Err(error) = written {
        crate::log_status!("step", "Could not record a step's cgroup usage: {}", error);
    }
}

/// A finished step's cgroup holds only descendants that outlived the child.
/// Its counters are read first: they cover the whole step.
fn stop_finished_cgroup(job_id: &str, cgroup: Option<&JobCgroup>) {
    record_cgroup_usage(job_id, cgroup);
    if let Some(cgroup) = cgroup {
        if let Err(error) = cgroup.stop() {
            crate::log_status!("step", "Could not stop a finished step's cgroup: {}", error);
        }
    }
}

/// Terminate the child's process tree when it is still the admitted child,
/// then wait until it is provably gone.
fn stop_child_tree(child: &StepChild) -> Result<()> {
    if child_state(child) == ProcessIdentityState::Live {
        crate::process::terminate_process_tree(
            child.pid,
            crate::process::PROCESS_TREE_SIGTERM_GRACE,
        )?;
    }
    let deadline = Instant::now() + CANCEL_STOP_DEADLINE;
    loop {
        match child_state(child) {
            ProcessIdentityState::Dead | ProcessIdentityState::IdentityMismatch => return Ok(()),
            _ if Instant::now() >= deadline => {
                return Err(Error::internal_unexpected(format!(
                    "step child {} did not stop after cancellation",
                    child.pid
                )))
            }
            _ => std::thread::sleep(SUPERVISE_POLL),
        }
    }
}

/// The wrapper survives the daemon and publishes the status before it exits.
/// Pass paths and argv as arguments, never interpolate them into shell source.
/// The wrapper every step child runs in: it records the child's exit code and
/// the CPU its process tree used in `dir`'s exit record.
pub fn step_command(request: &StepJobRequest, dir: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!(
            // `times` reports the reaped process tree's CPU at finish; it must
            // run in this shell, not a command-substitution subshell.
            "record=$1; shift; \"$@\"; code=$?; times > \"$record.cpu\" 2>/dev/null; {{ read _ _; read cu cs; }} < \"$record.cpu\"; /bin/rm -f \"$record.cpu\"; printf '{{\"schema\":\"{STEP_EXIT_SCHEMA}\",\"exit_code\":%s,\"cpu\":\"%s %s\"}}\\n' \"$code\" \"$cu\" \"$cs\" > \"$record.tmp\" && /bin/mv -f \"$record.tmp\" \"$record\"; exit \"$code\""
        ))
        .arg("sh")
        .arg(dir.join(STEP_EXIT_FILE))
        .arg(launchable_step_program(&request.program))
        .args(&request.args);
    command
}

pub(crate) fn terminal_delivery(
    job_id: &str,
    request: &Value,
    checkpoint: Option<&Value>,
    status: Option<crate::api_jobs::JobStatus>,
    error: Option<Value>,
    cancelled: bool,
    daemon_scope: Option<&crate::api_jobs::DaemonScope>,
) -> Result<Option<StepTerminalDelivery>> {
    // No synthesis for old in-flight rows that did not name a consumer.
    if request.get("terminal_consumer").is_none() {
        return Ok(None);
    }
    let request = StepJobRequest::parse(request)?;
    let parsed = checkpoint.map(StepJobCheckpoint::parse).transpose()?;
    if status.is_none()
        && !parsed
            .as_ref()
            .is_some_and(|cp| cp.phase == StepPhase::Exited)
    {
        return Ok(None);
    }
    let outcome = if cancelled {
        StepTerminalOutcome::Cancelled
    } else if error
        .as_ref()
        .is_some_and(|error| error["data"]["step_outcome"] == "spawn_interrupted")
    {
        StepTerminalOutcome::SpawnInterrupted
    } else if parsed
        .as_ref()
        .is_some_and(|cp| cp.phase == StepPhase::Exited)
    {
        StepTerminalOutcome::Exited
    } else if status == Some(crate::api_jobs::JobStatus::Succeeded) {
        StepTerminalOutcome::Succeeded
    } else {
        StepTerminalOutcome::Failed
    };
    let child = parsed.as_ref().and_then(|cp| cp.child.clone()).or_else(|| {
        error
            .as_ref()
            .and_then(|error| error.pointer("/data/step_child"))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    });
    Ok(Some(StepTerminalDelivery {
        job_id: job_id.to_string(),
        key: request.key.clone(),
        linked_durable_run_id: request.linked_durable_run_id.clone(),
        request,
        checkpoint: checkpoint.cloned(),
        child,
        outcome,
        error,
        // Recovered terminal winners can lack an exited checkpoint. The shell
        // writes exit.json before the supervisor publishes completion; retain
        // that numeric evidence on delivery rather than dropping it with the
        // checkpoint (and do not invent a child status from the job outcome).
        exit_code: parsed
            .as_ref()
            .and_then(|cp| cp.exit_code)
            .or_else(|| recorded_step_exit_code(job_id)),
        cancelled,
        log_path: step_job_dir(job_id)
            .map(|dir| dir.join(STEP_LOG_FILE))
            .unwrap_or_default(),
        daemon_scope: daemon_scope.cloned(),
    }))
}

fn to_value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| {
        Error::internal_json(error.to_string(), Some("serialize step job".to_string()))
    })
}

fn invalid(message: impl Into<String>) -> Error {
    Error::validation_invalid_argument("step", message, None, None)
}

/// Every serialized `StepJobRequest` field. Kept in sync by
/// `admission_field_lists_cover_every_serialized_field`.
const STEP_JOB_REQUEST_FIELDS: &[&str] = &[
    "schema",
    "terminal_consumer",
    "key",
    "program",
    "waits_for_children",
    "args",
    "cwd",
    "env",
    "linked_durable_run_id",
    "allows_terminal_linked_run",
    "root_durable_run_id",
    "wait",
];

/// Every serialized `StepWait` field.
const STEP_WAIT_FIELDS: &[&str] = &["not_before", "runner_eligible"];

fn reject_unknown_fields(value: &Value, known: &[&str], what: &str) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let unknown = object
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(invalid(format!(
        "invalid {what}: unknown field(s) {}, expected one of {}",
        unknown
            .iter()
            .map(|key| format!("`{key}`"))
            .collect::<Vec<_>>()
            .join(", "),
        known
            .iter()
            .map(|key| format!("`{key}`"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

#[cfg(test)]
mod forward_compat_tests {
    use super::*;

    fn request() -> StepJobRequest {
        StepJobRequest::new(
            "key",
            "/bin/true",
            vec!["arg".to_string()],
            "/tmp",
            EnvMaterializationPlan::default(),
            STEP_RECEIPT_CONSUMER,
        )
    }

    #[test]
    fn persisted_rows_from_newer_versions_still_decode() {
        // homeboy#17735: a newer controller's row must not brick an older
        // daemon's shared job store.
        let mut value = to_value(request()).unwrap();
        value["field_from_a_newer_homeboy"] = json!("x");
        value["wait"] = json!({ "runner_eligible": "lab", "newer_wait_field": 1 });
        let parsed = StepJobRequest::parse(&value).expect("persisted decode");
        assert_eq!(parsed.key, "key");
        assert_eq!(parsed.wait.runner_eligible.as_deref(), Some("lab"));

        let mut checkpoint = to_value(StepJobCheckpoint::waiting(parsed)).unwrap();
        checkpoint["newer_checkpoint_field"] = json!(true);
        checkpoint["request"]["field_from_a_newer_homeboy"] = json!("x");
        StepJobCheckpoint::parse(&checkpoint).expect("persisted checkpoint decode");

        let delivery = terminal_delivery(
            "job",
            &value,
            Some(&checkpoint),
            Some(crate::api_jobs::JobStatus::Succeeded),
            None,
            false,
            None,
        );
        assert!(delivery.is_ok(), "{delivery:?}");
    }

    #[test]
    fn terminal_delivery_retains_exit_record_without_exit_checkpoint() {
        crate::test_support::with_isolated_home(|_| {
            let job_id = uuid::Uuid::new_v4().to_string();
            let dir = step_job_dir(&job_id).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(STEP_EXIT_FILE),
                json!({"schema": STEP_EXIT_SCHEMA, "exit_code": 0}).to_string(),
            )
            .unwrap();
            let request = to_value(request()).unwrap();
            let delivery = terminal_delivery(
                &job_id,
                &request,
                None,
                Some(crate::api_jobs::JobStatus::Succeeded),
                None,
                false,
                None,
            )
            .unwrap()
            .unwrap();
            assert_eq!(delivery.outcome, StepTerminalOutcome::Succeeded);
            assert_eq!(delivery.exit_code, Some(0));
        });
    }

    #[test]
    fn admission_still_rejects_unknown_fields() {
        let mut value = to_value(request()).unwrap();
        value["programm"] = json!("/bin/false");
        let error = StepJobDriver
            .validate_secret_references(&value)
            .expect_err("unknown top-level field");
        assert!(error.message.contains("`programm`"), "{}", error.message);

        let mut value = to_value(request()).unwrap();
        value["wait"] = json!({ "not_befor": "2026-01-01T00:00:00Z" });
        let error = StepJobDriver
            .validate_secret_references(&value)
            .expect_err("unknown wait field");
        assert!(error.message.contains("`not_befor`"), "{}", error.message);

        StepJobDriver
            .validate_secret_references(&to_value(request()).unwrap())
            .expect("known fields admit");
    }

    #[test]
    fn admission_field_lists_cover_every_serialized_field() {
        let mut full = request().with_linked_durable_run_id("linked");
        full.root_durable_run_id = Some("root".to_string());
        full.waits_for_children = true;
        full.allows_terminal_linked_run = true;
        full.wait = StepWait {
            not_before: Some("2026-01-01T00:00:00Z".to_string()),
            runner_eligible: Some("lab".to_string()),
        };
        let value = to_value(&full).unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(
                STEP_JOB_REQUEST_FIELDS.contains(&key.as_str()),
                "add `{key}` to STEP_JOB_REQUEST_FIELDS"
            );
        }
        for key in value["wait"].as_object().unwrap().keys() {
            assert!(
                STEP_WAIT_FIELDS.contains(&key.as_str()),
                "add `{key}` to STEP_WAIT_FIELDS"
            );
        }
        StepJobRequest::parse_admission(&value).expect("fully populated request admits");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::{with_isolated_home, ControllerJobHarness};

    #[test]
    fn runner_wait_backs_off_records_reasons_and_stops_with_linked_run() {
        with_isolated_home(|_| {
            use std::cell::{Cell, RefCell};
            let mut request = request("true", Default::default());
            request.wait.runner_eligible = Some("runner-a".into());
            request.linked_durable_run_id = Some("run-a".into());
            let harness =
                ControllerJobHarness::new(Arc::new(StepJobDriver), to_value(&request).unwrap())
                    .unwrap();
            let elapsed = Cell::new(Duration::ZERO);
            let sleeps = RefCell::new(Vec::new());
            let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = probes.clone();
            let probe: StepRunnerEligibility = Arc::new(move |_| {
                let n = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(if n < 3 {
                    "build mismatch"
                } else {
                    "capacity full"
                }
                .into())
            });
            let result = await_wait_with(
                &request,
                &harness.handle(),
                Some(probe),
                || elapsed.get(),
                |delay| {
                    sleeps.borrow_mut().push(delay);
                    elapsed.set(elapsed.get() + delay);
                },
                |run| {
                    assert_eq!(run, "run-a");
                    elapsed.get() >= Duration::from_secs(600)
                },
            )
            .unwrap();
            assert!(!result);
            assert!(probes.load(std::sync::atomic::Ordering::SeqCst) <= 40);
            let sleeps = sleeps.borrow();
            assert_eq!(&sleeps[..6], &[1, 2, 4, 8, 16, 30].map(Duration::from_secs));
            assert!(sleeps.iter().all(|delay| *delay <= RUNNER_WAIT_CAP));
            let reasons: Vec<_> = harness
                .events()
                .unwrap()
                .into_iter()
                .filter_map(|event| {
                    event.data.and_then(|data| {
                        data.get("reason")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                })
                .collect();
            assert_eq!(reasons, ["build mismatch", "capacity full"]);
        });
    }

    #[test]
    fn runner_wait_deadline_names_runner_and_last_reason() {
        with_isolated_home(|_| {
            use std::cell::Cell;
            let mut request = request("true", Default::default());
            request.wait.runner_eligible = Some("runner-a".into());
            let harness =
                ControllerJobHarness::new(Arc::new(StepJobDriver), to_value(&request).unwrap())
                    .unwrap();
            let elapsed = Cell::new(Duration::ZERO);
            let error = await_wait_with(
                &request,
                &harness.handle(),
                Some(Arc::new(|_| Err("daemon_incompatible".into()))),
                || elapsed.get(),
                |delay| elapsed.set(elapsed.get() + delay),
                |_| false,
            )
            .unwrap_err();
            assert!(error.to_string().contains(
                "runner runner-a eligibility wait budget exhausted: daemon_incompatible"
            ));
            assert_eq!(elapsed.get(), RUNNER_WAIT_DEADLINE);
        });
    }

    #[test]
    fn terminal_linked_run_only_allows_intentional_recovery() {
        with_isolated_home(|_| {
            for (allows_terminal_linked_run, expected) in [(true, true), (false, false)] {
                let mut request = request("true", Default::default());
                request.wait.runner_eligible = Some("runner-a".into());
                request.allows_terminal_linked_run = allows_terminal_linked_run;
                let run_id = uuid::Uuid::new_v4().to_string();
                request.linked_durable_run_id = Some(run_id.clone());
                let fixtures = crate::test_support::AgentTaskTerminalRunFixtures::install();
                fixtures.terminal_run(&run_id, crate::api_jobs::JobStatus::Cancelled);
                let harness =
                    ControllerJobHarness::new(Arc::new(StepJobDriver), to_value(&request).unwrap())
                        .unwrap();
                assert_eq!(
                    await_wait_with(
                        &request,
                        &harness.handle(),
                        Some(Arc::new(move |_| {
                            assert!(
                                allows_terminal_linked_run,
                                "ordinary stale work must not probe"
                            );
                            Ok(())
                        })),
                        || Duration::ZERO,
                        |_| panic!("terminal run must not sleep"),
                        |run_id| {
                            crate::api_jobs::agent_task_terminal_recovery::linked_durable_run_state(
                                run_id,
                            ) == Some(crate::api_jobs::DaemonLinkedDurableRunState::Terminal)
                        },
                    )
                    .unwrap(),
                    expected,
                    "allows_terminal_linked_run={allows_terminal_linked_run}"
                );
                fixtures.remove(&run_id);
            }
        });
    }

    /// A recovery step targets a terminal run by design; with nothing to wait
    /// for it must spawn its child rather than finish without running.
    #[test]
    fn terminal_linked_run_without_a_wait_still_spawns() {
        with_isolated_home(|_| {
            let mut request = request("true", Default::default());
            let run_id = uuid::Uuid::new_v4().to_string();
            request.linked_durable_run_id = Some(run_id.clone());
            let harness =
                ControllerJobHarness::new(Arc::new(StepJobDriver), to_value(&request).unwrap())
                    .unwrap();
            assert!(await_wait_with(
                &request,
                &harness.handle(),
                None,
                || Duration::ZERO,
                |_| panic!("an empty wait must not sleep"),
                |_| true,
            )
            .unwrap());
        });
    }

    #[test]
    fn eligible_runner_does_not_sleep() {
        with_isolated_home(|_| {
            let mut request = request("true", Default::default());
            request.wait.runner_eligible = Some("runner-a".into());
            let harness =
                ControllerJobHarness::new(Arc::new(StepJobDriver), to_value(&request).unwrap())
                    .unwrap();
            assert!(await_wait_with(
                &request,
                &harness.handle(),
                Some(Arc::new(|_| Ok(()))),
                || Duration::ZERO,
                |_| panic!("eligible runner must spawn immediately"),
                |_| false,
            )
            .unwrap());
        });
    }

    struct PrepareFailure;
    impl ControllerJobDriver for PrepareFailure {
        fn job_type(&self) -> &'static str {
            STEP_JOB_TYPE
        }
        fn version(&self) -> u32 {
            2001
        }
        fn public_request(&self, request: &Value) -> Result<Value> {
            StepJobDriver.public_request(request)
        }
        fn public_progress(&self, progress: &Value) -> Result<Value> {
            StepJobDriver.public_progress(progress)
        }
        fn public_result(&self, result: &Value) -> Result<Value> {
            StepJobDriver.public_result(result)
        }
        fn public_error(&self, error: &Error) -> ControllerJobPublicError {
            StepJobDriver.public_error(error)
        }
        fn validate_secret_references(&self, request: &Value) -> Result<()> {
            StepJobDriver.validate_secret_references(request)
        }
        fn prepare(&self, _: Value) -> Result<Value> {
            Err(invalid("injected prepare failure"))
        }
        fn execute(&self, _: Value, _: ControllerJobHandle) -> Result<Value> {
            panic!("failed preparation must not execute")
        }
        fn cancel(&self, _: &Value) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn prepare_failure_has_durable_terminal_custody_without_checkpoint_or_child() {
        with_isolated_home(|home| {
            controller_job_driver::register_controller_job_driver(Arc::new(PrepareFailure))
                .unwrap();
            let request = request("true", Default::default());
            let harness = ControllerJobHarness::durable_queued(
                Arc::new(PrepareFailure),
                to_value(&request).unwrap(),
                &home.path().join("prepare.json"),
                "prepare-lease",
            )
            .unwrap();
            harness.start_via_controller_boundary().unwrap();
            assert_eq!(
                harness.wait_until_terminal().unwrap().status,
                crate::api_jobs::JobStatus::Failed
            );
            let pending = harness.store().pending_step_terminal_deliveries().unwrap();
            assert_eq!(pending.len(), 1);
            assert!(pending[0].child.is_none());
            assert!(pending[0].checkpoint.is_none());
            assert_eq!(pending[0].outcome, StepTerminalOutcome::Failed);
            assert!(pending[0].error.as_ref().unwrap()["message"]
                .as_str()
                .unwrap()
                .contains("injected prepare failure"));
        });
    }

    fn request(script: &str, env: EnvMaterializationPlan) -> StepJobRequest {
        StepJobRequest::new(
            format!("test-step:{}", uuid::Uuid::new_v4()),
            "/bin/sh",
            vec!["-c".to_string(), script.to_string()],
            std::env::temp_dir().display().to_string(),
            env,
            STEP_RECEIPT_CONSUMER,
        )
    }

    fn step_harness(request: &StepJobRequest, root: &Path) -> ControllerJobHarness {
        register_step_job_driver();
        let driver: Arc<dyn ControllerJobDriver> = Arc::new(StepJobDriver);
        ControllerJobHarness::durable_queued(
            driver,
            to_value(request).unwrap(),
            &root.join(format!("step-jobs-{}.json", uuid::Uuid::new_v4())),
            "step-test-lease",
        )
        .unwrap()
    }

    fn job_log(harness: &ControllerJobHarness) -> String {
        let job_id = harness.job().unwrap().id.to_string();
        std::fs::read_to_string(step_job_dir(&job_id).unwrap().join(STEP_LOG_FILE))
            .unwrap_or_default()
    }

    #[test]
    fn workflow_exec_waits_for_lab_capacity_unless_declared_uncapped() {
        for uncapped in [false, true] {
            workflow_exec_capacity_case(uncapped);
        }
    }

    fn workflow_exec_capacity_case(uncapped: bool) {
        with_isolated_home(|home| {
            let workflow = serde_json::from_value(json!({
                "schema": "homeboy/workflow-job-request/v1",
                "instance": "wf-capacity-test",
                "definition": {"schema": "homeboy/workflow/v1", "id": "capacity", "steps": []},
                "definition_digest": "unused",
                "inputs": {},
                "homeboy_program": "/bin/true",
                "cwd": home.path(),
                "env": {"public_env": {"HOMEBOY_LAB_CONCURRENCY_LIMIT": "1"}}
            }))
            .unwrap();
            let marker = home.path().join("spawned");
            let submission = crate::workflow::advance::StepSubmission {
                key: "workflow-exec-capacity".into(),
                step: "import".into(),
                iteration: 0,
                attempt: 0,
                kind: crate::workflow::advance::SubmissionKind::Exec {
                    program: "/bin/sh".into(),
                    args: vec![
                        "-c".into(),
                        "printf ran > \"$1\"".into(),
                        "sh".into(),
                        marker.display().to_string(),
                    ],
                    cwd: None,
                },
                not_before_ms: None,
                uncapped,
            };
            let request = super::super::workflow_job::step_request(&workflow, &submission).unwrap();
            let permit = crate::lab_capacity::acquire_step(
                Some(1),
                crate::lab_capacity::StepClaim::owner("capacity-test"),
                |_, _| Ok(true),
            )
            .unwrap()
            .unwrap();
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            if uncapped {
                // A declared light step spawns while every slot is held.
                assert_eq!(
                    harness.wait_until_terminal().unwrap().status,
                    crate::api_jobs::JobStatus::Succeeded
                );
                assert_eq!(std::fs::read_to_string(marker).unwrap(), "ran");
                return;
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            while crate::lab_capacity::queued_steps() == 0 {
                assert!(!marker.exists(), "workflow exec spawned without a permit");
                assert!(
                    Instant::now() < deadline,
                    "workflow exec never queued for capacity"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                !marker.exists(),
                "workflow exec spawned while capacity was held"
            );
            // Queued time is not execution: the workflow sees no spawn yet.
            let job_id = harness.job().unwrap().id;
            let spawned_at = || harness.store().step_job_spawned_at_ms(job_id);
            assert_eq!(spawned_at(), None);
            drop(permit);
            assert_eq!(
                harness.wait_until_terminal().unwrap().status,
                crate::api_jobs::JobStatus::Succeeded
            );
            assert_eq!(std::fs::read_to_string(marker).unwrap(), "ran");
            assert!(spawned_at().is_some(), "the spawn is observable");
        });
    }

    /// #18844: a step queued before an upgrade, whose program was recorded by
    /// a process running the already-replaced binary (`<path> (deleted)`),
    /// runs the binary installed at that path when it is admitted.
    #[cfg(unix)]
    #[test]
    fn queued_step_runs_the_binary_installed_after_an_upgrade() {
        use std::os::unix::fs::PermissionsExt;
        with_isolated_home(|home| {
            let bin = home.path().join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let program = bin.join("homeboy");
            let marker = home.path().join("ran-build");
            let install = |build: &str| {
                let staged = bin.join(format!(".homeboy-{build}"));
                std::fs::write(&staged, format!("#!/bin/sh\nprintf {build} > \"$1\"\n")).unwrap();
                std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).unwrap();
                // Installers rename the new build over the old path.
                std::fs::rename(&staged, &program).unwrap();
            };
            install("old");

            let deleted = format!("{}{DELETED_EXECUTABLE_SUFFIX}", program.display());
            let normalized = StepJobRequest::new(
                "normalized",
                deleted.clone(),
                Vec::new(),
                "/",
                Default::default(),
                STEP_RECEIPT_CONSUMER,
            );
            assert_eq!(normalized.program, program.display().to_string());

            let mut request = request("unused", Default::default());
            // A row persisted before #18844 carries the deleted suffix.
            request.program = deleted;
            request.args = vec![marker.display().to_string()];
            let harness = step_harness(&request, home.path());
            install("new");
            harness.start_via_controller_boundary().unwrap();
            assert_eq!(
                harness.wait_until_terminal().unwrap().status,
                crate::api_jobs::JobStatus::Succeeded,
                "{}",
                job_log(&harness)
            );
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "new");
        });
    }

    /// #18844 guard: production code that records a durable step program must
    /// resolve it through [`durable_step_program`] (or the daemon's launchable
    /// executable), never a raw `std::env::current_exe()`, which names
    /// `<path> (deleted)` once an upgrade has replaced the running binary.
    #[test]
    fn durable_step_programs_never_come_from_raw_current_exe() {
        fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|name| name == "tests") {
                        continue;
                    }
                    rust_files(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && !path.to_string_lossy().ends_with("tests.rs")
                {
                    out.push(path);
                }
            }
        }
        /// Production source: comment lines and a trailing inline test
        /// module are excluded.
        fn production_source(source: &str) -> String {
            let lines: Vec<&str> = source.lines().collect();
            let mut kept = Vec::new();
            for (index, line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                if trimmed == "#[cfg(test)]"
                    && lines.get(index + 1).is_some_and(|next| {
                        let next = next.trim();
                        next.contains("mod ") && next.ends_with('{')
                    })
                {
                    break;
                }
                if !trimmed.starts_with("//") {
                    kept.push(*line);
                }
            }
            kept.join("\n")
        }

        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        rust_files(&workspace.join("crates"), &mut files);
        rust_files(&workspace.join("src"), &mut files);
        let mut offenders = Vec::new();
        let mut scanned = 0;
        for file in files {
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            if !source.contains("StepJobRequest::new(") && !source.contains("homeboy_program") {
                continue;
            }
            scanned += 1;
            if production_source(&source).contains("current_exe()") {
                offenders.push(file.display().to_string());
            }
        }
        assert!(scanned > 0, "guard found no durable step recorders");
        assert!(
            offenders.is_empty(),
            "durable step programs must use step_job::durable_step_program(), not current_exe(): {offenders:?}"
        );
    }

    /// #16273: a failed step names the child's own cause, not only its status.
    #[test]
    fn failed_step_names_the_cause_its_child_reported() {
        with_isolated_home(|home| {
            let envelope = r#"printf 'progress line\n{\n  "success": false,\n  "summary": "Invalid argument repo: not registered"\n}\n'; exit 2"#;
            // A child whose envelope carries only the generic unnamed-exit
            // fallback: the error it printed before the envelope is the cause.
            let unnamed = r#"echo 'fatal: gate rejected the candidate' >&2; printf '{\n  "success": false,\n  "summary": "agent-task execution-step exited 1 without reporting a failure cause",\n  "data": {"exit_code": 1},\n  "diagnostics": {"code": "command.failed", "message": "agent-task execution-step exited 1 without reporting a failure cause"}\n}\n'; exit 1"#;
            for (script, expected) in [
                (
                    unnamed,
                    "step child exited with status 1: agent-task execution-step exited 1 without reporting a failure cause; last output: fatal: gate rejected the candidate",
                ),
                (
                    envelope,
                    "step child exited with status 2: Invalid argument repo: not registered",
                ),
                (
                    "echo first >&2; echo 'fatal: no such checkout' >&2; exit 3",
                    "step child exited with status 3: first | fatal: no such checkout",
                ),
                ("exit 4", "step child exited with status 4"),
            ] {
                let harness = step_harness(&request(script, Default::default()), home.path());
                harness.start_via_controller_boundary().unwrap();
                let job = harness.wait_until_terminal().unwrap();
                assert_eq!(job.status, crate::api_jobs::JobStatus::Failed, "{script}");
                let pending = harness.store().pending_step_terminal_deliveries().unwrap();
                let error = pending[0].error.as_ref().expect("step error");
                assert_eq!(error["message"].as_str(), Some(expected), "{script}");
                assert!(pending[0].log_path.ends_with(STEP_LOG_FILE));
            }
        });
    }

    #[test]
    fn child_sees_only_the_admitted_environment() {
        with_isolated_home(|home| {
            std::env::set_var("HOMEBOY_STEP_TEST_AMBIENT", "daemon-only");
            let mut env = EnvMaterializationPlan::default();
            env.public_env
                .insert("STEP_DECLARED".to_string(), "declared".to_string());
            let request = request("env | sort", env);
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            let job = harness.wait_until_terminal().unwrap();
            std::env::remove_var("HOMEBOY_STEP_TEST_AMBIENT");
            assert_eq!(job.status, crate::api_jobs::JobStatus::Succeeded);
            let log = job_log(&harness);
            let names = log
                .lines()
                .filter_map(|line| line.split_once('=').map(|(name, _)| name))
                .filter(|name| {
                    !matches!(*name, "PWD" | "SHLVL" | "_") && *name != STEP_JOB_STORE_ENV
                })
                .collect::<Vec<_>>();
            assert_eq!(names, vec![STEP_JOB_ID_ENV, "STEP_DECLARED"], "{log}");
            assert!(log.contains("STEP_DECLARED=declared"));
            assert!(!log.contains("daemon-only"));
            assert_eq!(recorded_step_exit_code(&job.id.to_string()), Some(0));
        });
    }

    /// #17679: a caller's PATH travels in the admitted plan and reaches the
    /// child verbatim, never reordered or replaced by the daemon's PATH.
    #[test]
    fn caller_path_reaches_the_child_verbatim() {
        with_isolated_home(|home| {
            let caller_path = "/caller/node/bin:/usr/local/bin:/usr/bin:/bin";
            let mut env = EnvMaterializationPlan::default();
            env.public_env
                .insert("PATH".to_string(), caller_path.to_string());
            let request = request("printf 'child-path=%s\\n' \"$PATH\"", env);
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            let job = harness.wait_until_terminal().unwrap();
            assert_eq!(job.status, crate::api_jobs::JobStatus::Succeeded);
            let log = job_log(&harness);
            assert!(
                log.lines()
                    .any(|line| line == format!("child-path={caller_path}")),
                "{log}"
            );
        });
    }

    #[test]
    fn secret_references_resolve_through_the_registered_resolver_only() {
        with_isolated_home(|home| {
            register_step_secret_resolver(Arc::new(|refs: &[EnvSecretRef]| {
                Ok(refs
                    .iter()
                    .filter(|secret| secret.name == "STEP_TEST_SECRET")
                    .map(|secret| (secret.name.clone(), "resolved-value".to_string()))
                    .collect())
            }));
            let mut env = EnvMaterializationPlan::default();
            env.secret_refs.push(EnvSecretRef {
                name: "STEP_TEST_SECRET".to_string(),
                owner: None,
            });
            let resolved = request("test \"$STEP_TEST_SECRET\" = resolved-value", env);
            let harness = step_harness(&resolved, home.path());
            harness.start_via_controller_boundary().unwrap();
            assert_eq!(
                harness.wait_until_terminal().unwrap().status,
                crate::api_jobs::JobStatus::Succeeded
            );

            let mut missing = EnvMaterializationPlan::default();
            missing.secret_refs.push(EnvSecretRef {
                name: "STEP_TEST_UNDECLARED_SECRET".to_string(),
                owner: None,
            });
            std::env::set_var("STEP_TEST_UNDECLARED_SECRET", "ambient");
            let harness = step_harness(&request("true", missing), home.path());
            harness.start_via_controller_boundary().unwrap();
            let job = harness.wait_until_terminal().unwrap();
            std::env::remove_var("STEP_TEST_UNDECLARED_SECRET");
            assert_eq!(job.status, crate::api_jobs::JobStatus::Failed);
        });
    }

    #[test]
    fn credentials_cannot_travel_as_public_values() {
        let mut env = EnvMaterializationPlan::default();
        env.public_env
            .insert("GITHUB_TOKEN".to_string(), "inline".to_string());
        assert!(request("true", env).validate().is_err());
        let mut inherited = EnvMaterializationPlan::default();
        inherited
            .inherited_allowed_env_names
            .push("PATH".to_string());
        assert!(request("true", inherited).validate().is_err());
    }

    #[test]
    fn nonzero_exit_fails_the_job_and_records_the_code() {
        with_isolated_home(|home| {
            let request = request("exit 7", EnvMaterializationPlan::default());
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            let job = harness.wait_until_terminal().unwrap();
            assert_eq!(job.status, crate::api_jobs::JobStatus::Failed);
            assert_eq!(recorded_step_exit_code(&job.id.to_string()), Some(7));
            // The wrapper records the reaped tree's CPU; it is estimated.
            let usage = recorded_step_usage(&job.id.to_string(), 5).expect("step usage");
            assert!(usage.estimated);
            assert_eq!(usage.wall_ms, 5);
        });
    }

    #[test]
    fn shell_times_durations_parse_to_microseconds() {
        assert_eq!(shell_times_usec("0m0.004000s"), Some(4_000));
        assert_eq!(shell_times_usec("1m2.5s"), Some(62_500_000));
        assert_eq!(shell_times_usec(""), None);
    }

    #[test]
    fn cancellation_terminates_the_child_process_tree() {
        with_isolated_home(|home| {
            let marker = home.path().join("grandchild.pid");
            let script = format!("sleep 60 & echo $! > {}; wait", marker.display());
            let request = request(&script, EnvMaterializationPlan::default());
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !marker.exists() {
                assert!(Instant::now() < deadline, "grandchild never started");
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_millis(50));
            let grandchild: u32 = std::fs::read_to_string(&marker)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(crate::process::pid_is_running(grandchild));
            crate::daemon::cancel_controller_job(
                harness.job().unwrap().id,
                "test cancellation",
                harness.store(),
            )
            .unwrap();
            let job = harness.wait_until_terminal().unwrap();
            assert_eq!(job.status, crate::api_jobs::JobStatus::Cancelled);
            let deadline = Instant::now() + Duration::from_secs(10);
            while crate::process::pid_is_running(grandchild) {
                assert!(
                    Instant::now() < deadline,
                    "grandchild survived cancellation"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    }

    /// #18449: a cancelled step stops its whole cgroup, so a `setsid`-detached
    /// descendant that left the step's session and process tree dies too.
    #[cfg(target_os = "linux")]
    #[test]
    fn cancelling_a_step_kills_its_whole_cgroup_with_zero_survivors() {
        crate::job_cgroup::test_support::run_in_delegated_scope(
            "daemon::step_job::tests::delegated_step_cancel_kills_detached_descendants",
            &[],
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "inner half of cancelling_a_step_kills_its_whole_cgroup_with_zero_survivors"]
    fn delegated_step_cancel_kills_detached_descendants() {
        use crate::job_cgroup::test_support::{
            alive, assert_job_cgroups_removed, inside_delegated_scope, survivors, unique_sleep,
        };
        if !inside_delegated_scope() {
            return;
        }
        let jobs =
            crate::job_cgroup::initialize(0).expect("a delegated scope initializes job cgroups");
        with_isolated_home(|home| {
            let detached_pid = home.path().join("detached.pid");
            let sleep = unique_sleep();
            // The subshell exits at once, so the `setsid` child leaves the
            // step's session, process group and process tree.
            let script = format!(
                "(setsid sh -c 'trap \"\" TERM; echo $$ > {pid}; exec sleep {sleep}' &); \
                 exec sleep {sleep}",
                pid = detached_pid.display(),
            );
            let request = request(&script, EnvMaterializationPlan::default());
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            let job_id = harness.job().unwrap().id.to_string();
            let detached = || {
                std::fs::read_to_string(&detached_pid)
                    .ok()
                    .and_then(|pid| pid.trim().parse::<u32>().ok())
            };
            let deadline = Instant::now() + Duration::from_secs(30);
            while detached().is_none() {
                assert!(Instant::now() < deadline, "detached child never started");
                std::thread::sleep(Duration::from_millis(20));
            }
            let detached = detached().unwrap();
            let cgroup = jobs.join(&job_id);
            assert!(cgroup.is_dir(), "the step's cgroup is named for its job id");
            let members = std::fs::read_to_string(cgroup.join("cgroup.procs")).unwrap();
            assert!(
                members
                    .lines()
                    .any(|pid| pid.trim() == detached.to_string()),
                "the detached child stays in the step's cgroup: {members}"
            );

            crate::daemon::cancel_controller_job(
                harness.job().unwrap().id,
                "test cancellation",
                harness.store(),
            )
            .unwrap();
            let job = harness.wait_until_terminal().unwrap();

            assert_eq!(job.status, crate::api_jobs::JobStatus::Cancelled);
            assert!(!alive(detached), "cancel left detached child {detached}");
            assert_eq!(survivors(&sleep), Vec::<String>::new());
            assert_job_cgroups_removed(jobs);
        });
    }

    /// #18461: a step in its own cgroup records the cgroup's exact counters,
    /// including CPU a detached descendant burned that the wrapper's `times`
    /// never reaps.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_finished_step_records_its_cgroups_exact_usage() {
        crate::job_cgroup::test_support::run_in_delegated_scope(
            "daemon::step_job::tests::delegated_step_records_cgroup_usage",
            &[],
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "inner half of a_finished_step_records_its_cgroups_exact_usage"]
    fn delegated_step_records_cgroup_usage() {
        if !crate::job_cgroup::test_support::inside_delegated_scope() {
            return;
        }
        crate::job_cgroup::initialize(0).expect("a delegated scope initializes job cgroups");
        with_isolated_home(|home| {
            let done = home.path().join("burned");
            let script = format!(
                "(setsid sh -c 'end=$(($(date +%s) + 2)); while [ $(date +%s) -lt $end ]; do :; done; touch {done}' &); \
                 while [ ! -e {done} ]; do sleep 0.1; done",
                done = done.display(),
            );
            let request = request(&script, EnvMaterializationPlan::default());
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            let job = harness.wait_until_terminal().unwrap();
            assert_eq!(job.status, crate::api_jobs::JobStatus::Succeeded);
            let usage = recorded_step_usage(&job.id.to_string(), 7).expect("step usage");
            assert!(!usage.estimated, "{usage:?}");
            assert_eq!(
                usage.source,
                homeboy_engine_primitives::resource_usage::USAGE_SOURCE_JOB_CGROUP
            );
            assert_eq!(usage.wall_ms, 7);
            assert!(usage.cpu_usage_usec >= 500_000, "{usage:?}");
        });
    }

    #[test]
    fn a_running_checkpoint_is_re_adopted_by_each_successor_without_a_second_spawn() {
        with_isolated_home(|home| {
            let path = home.path().join("step-jobs.sqlite");
            let counter = home.path().join("spawns");
            let script = format!("echo spawn >> {}; sleep 2", counter.display());
            let request = request(&script, EnvMaterializationPlan::default());
            register_step_job_driver();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(StepJobDriver);
            let harness = ControllerJobHarness::durable(
                Arc::clone(&driver),
                to_value(&request).unwrap(),
                &path,
                "step-lease",
            )
            .unwrap();
            // Spawn the child the way the driver does, then record it as the
            // checkpoint a daemon would have published before it died.
            let dir = step_job_dir(&harness.handle().job_id()).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let mut process = step_command(&request, &dir).spawn().unwrap();
            let child = StepChild {
                pid: process.id(),
                start_identity: crate::process::process_start_identity(process.id())
                    .unwrap()
                    .unwrap(),
            };
            let mut checkpoint = StepJobCheckpoint::waiting(request.clone());
            checkpoint.phase = StepPhase::Running;
            checkpoint.child = Some(child);
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            // A first successor claims the running step, then dies before it
            // ends. Upgrades can replace the daemon again during one long step.
            let job_id = uuid::Uuid::parse_str(&harness.handle().job_id()).unwrap();
            crate::api_jobs::JobStore::open_without_reconciliation(&path)
                .unwrap()
                .with_daemon_lease("first-successor".to_string())
                .claim_controller_execution(job_id, true)
                .unwrap();
            let restarted = harness.reopen(&path, "replacement-lease").unwrap();
            let reaper = std::thread::spawn(move || process.wait());
            restarted.recover_after_restart();
            let job = restarted.wait_until_terminal().unwrap();
            reaper.join().unwrap().unwrap();
            assert_eq!(job.status, crate::api_jobs::JobStatus::Succeeded);
            assert_eq!(
                std::fs::read_to_string(&counter).unwrap().lines().count(),
                1,
                "re-adoption must not spawn the child again"
            );
        });
    }

    #[test]
    fn re_adopted_child_nonzero_exit_fails_with_its_cause() {
        with_isolated_home(|home| {
            let path = home.path().join("step-jobs.sqlite");
            let request = request(
                "sleep 1; echo 'adopted child crashed' >&2; exit 1",
                EnvMaterializationPlan::default(),
            );
            register_step_job_driver();
            let harness = ControllerJobHarness::durable(
                Arc::new(StepJobDriver),
                to_value(&request).unwrap(),
                &path,
                "original-lease",
            )
            .unwrap();
            let job_id = harness.handle().job_id();
            let dir = step_job_dir(&job_id).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let log = std::fs::File::create(dir.join(STEP_LOG_FILE)).unwrap();
            let mut process = step_command(&request, &dir)
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap();
            let mut checkpoint = StepJobCheckpoint::waiting(request);
            checkpoint.phase = StepPhase::Running;
            checkpoint.child = Some(StepChild {
                pid: process.id(),
                start_identity: crate::process::process_start_identity(process.id())
                    .unwrap()
                    .unwrap(),
            });
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            let restarted = harness.reopen(&path, "replacement-lease").unwrap();
            let reaper = std::thread::spawn(move || process.wait());
            restarted.recover_after_restart();
            let job = restarted.wait_until_terminal().unwrap();
            reaper.join().unwrap().unwrap();
            assert_eq!(job.status, crate::api_jobs::JobStatus::Failed);
            assert_eq!(recorded_step_exit_code(&job_id), Some(1));
            let pending = restarted
                .store()
                .pending_step_terminal_deliveries()
                .unwrap();
            let error = pending[0].error.as_ref().expect("step error");
            assert_eq!(
                error["message"].as_str(),
                Some("step child exited with status 1: adopted child crashed")
            );
        });
    }

    #[test]
    fn execution_step_pressure_waits_in_daemon_admission_queue() {
        with_isolated_home(|home| {
            let pressure = crate::test_support::EnvVarGuard::set(
                crate::runner_pressure::SAMPLE_ENV,
                r#"{"memory":9000}"#,
            );
            let marker = home.path().join("started");
            let mut request = request(
                &format!("touch {}", marker.display()),
                EnvMaterializationPlan::default(),
            );
            request.terminal_consumer = "agent-task.execution-step".into();
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            let job = harness.job().unwrap();
            assert_eq!(job.status, crate::api_jobs::JobStatus::Queued);
            assert_eq!(
                harness.store().pending_admission_report(job.id).unwrap()
                    ["pending_admission_reason"],
                "pressure"
            );
            for _ in 0..3 {
                harness.store().reconcile_direct_admissions().unwrap();
            }
            assert!(!marker.exists());
            assert_eq!(
                harness.job().unwrap().status,
                crate::api_jobs::JobStatus::Queued
            );
            let events = harness.store().events(job.id).unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.message.as_deref() == Some("waiting on Lab capacity"))
                    .count(),
                1
            );
            drop(pressure);
            let _clear =
                crate::test_support::EnvVarGuard::set(crate::runner_pressure::SAMPLE_ENV, "{}");
            harness.store().reconcile_direct_admissions().unwrap();
            assert_eq!(
                harness.wait_until_terminal().unwrap().status,
                crate::api_jobs::JobStatus::Succeeded
            );
            assert!(marker.exists());
        });
    }

    #[test]
    fn not_before_holds_the_spawn() {
        with_isolated_home(|home| {
            let marker = home.path().join("started");
            let at = chrono::Utc::now() + chrono::Duration::milliseconds(800);
            let request = request(
                &format!("touch {}", marker.display()),
                EnvMaterializationPlan::default(),
            )
            .with_wait(StepWait {
                not_before: Some(at.to_rfc3339()),
                runner_eligible: None,
            });
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            std::thread::sleep(Duration::from_millis(300));
            assert!(!marker.exists(), "child started before not_before");
            harness.wait_until_terminal().unwrap();
            assert!(marker.exists());
        });
    }

    #[test]
    fn terminal_consumer_failure_retries_and_ack_is_idempotent() {
        with_isolated_home(|home| {
            let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = attempts.clone();
            let domain_path = home.path().join("terminal-consumer.sqlite");
            rusqlite::Connection::open(&domain_path)
                .unwrap()
                .execute_batch(
                    "CREATE TABLE applied (job_id TEXT PRIMARY KEY, receipt TEXT NOT NULL);",
                )
                .unwrap();
            let consumer_path = domain_path.clone();
            register_step_terminal_consumer(
                "test.retry",
                Arc::new(move |delivery| {
                    // The domain commit and daemon ACK are separate databases.
                    // Losing the ACK after this commit must replay idempotently.
                    rusqlite::Connection::open(&consumer_path).unwrap().execute(
                        "INSERT OR IGNORE INTO applied(job_id, receipt) VALUES (?1, 'committed')",
                        [&delivery.job_id],
                    ).unwrap();
                    if observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        Err(invalid("interrupted after domain commit before ACK"))
                    } else {
                        Ok(json!({"committed": true}))
                    }
                }),
            );
            let mut request = request("exit 7", Default::default());
            request.terminal_consumer = "test.retry".to_string();
            let harness = step_harness(&request, home.path());
            harness.start_via_controller_boundary().unwrap();
            let job = harness.wait_until_terminal().unwrap();
            assert!(harness.store().deliver_pending_step_terminals().is_err());
            let pending = harness.store().pending_step_terminal_deliveries().unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].exit_code, Some(7));
            harness.store().deliver_pending_step_terminals().unwrap();
            assert_eq!(
                attempts.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "backoff must bound retries"
            );
            std::thread::sleep(Duration::from_millis(1100));
            let fresh = crate::api_jobs::JobStore::open_without_reconciliation(
                harness.store().persistence_path().unwrap(),
            )
            .unwrap();
            fresh.deliver_pending_step_terminals().unwrap();
            fresh.deliver_pending_step_terminals().unwrap();
            assert!(harness
                .store()
                .pending_step_terminal_deliveries()
                .unwrap()
                .is_empty());
            assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
            let applied: usize = rusqlite::Connection::open(domain_path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM applied", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                applied, 1,
                "lost ACK cannot repeat the committed domain mutation"
            );
            harness
                .store()
                .acknowledge_step_terminal_delivery(
                    &job.id.to_string(),
                    &json!({"committed": true}),
                )
                .unwrap();
            assert!(harness
                .store()
                .acknowledge_step_terminal_delivery(
                    &job.id.to_string(),
                    &json!({"different": true})
                )
                .is_err());
        });
    }

    #[test]
    fn exited_custody_waits_for_terminal_winner_and_cancellation_supersedes_exit() {
        with_isolated_home(|home| {
            let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = delivered.clone();
            register_step_terminal_consumer(
                "test.cancel-winner",
                Arc::new(move |delivery| {
                    assert!(delivery.cancelled);
                    assert_eq!(delivery.outcome, StepTerminalOutcome::Cancelled);
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(json!({"cancelled": true}))
                }),
            );
            register_step_job_driver();
            let mut request = request("true", Default::default());
            request.terminal_consumer = "test.cancel-winner".to_string();
            let harness = ControllerJobHarness::durable(
                Arc::new(StepJobDriver),
                to_value(&request).unwrap(),
                &home.path().join("cancel-winner.json"),
                "cancel-lease",
            )
            .unwrap();
            let mut checkpoint = StepJobCheckpoint::waiting(request);
            checkpoint.phase = StepPhase::Exited;
            checkpoint.exit_code = Some(0);
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            harness.store().deliver_pending_step_terminals().unwrap();
            assert_eq!(
                delivered.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "checkpoint custody is provisional until terminality"
            );
            let id = harness.job().unwrap().id;
            harness
                .store()
                .request_controller_cancellation(id, "cancel wins".to_string())
                .unwrap();
            harness
                .store()
                .complete_controller_cancellation(id)
                .unwrap();
            harness.store().deliver_pending_step_terminals().unwrap();
            harness.store().deliver_pending_step_terminals().unwrap();
            assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn current_lease_claim_fences_stale_recovery_before_runtime_publication() {
        with_isolated_home(|home| {
            register_step_job_driver();
            let path = home.path().join("claim-race.json");
            let request = request("true", Default::default());
            let harness = ControllerJobHarness::durable(
                Arc::new(StepJobDriver),
                to_value(&request).unwrap(),
                &path,
                "current-lease",
            )
            .unwrap();
            harness
                .handle()
                .checkpoint(to_value(StepJobCheckpoint::waiting(request)).unwrap())
                .unwrap();
            let id = harness.job().unwrap().id;
            // No runtime has been published, but startup cannot steal a worker
            // that this very lease durably claimed from a queued snapshot.
            assert!(harness
                .store()
                .claim_controller_execution(id, true)
                .is_err());
            let restarted = harness.reopen(&path, "replacement-lease").unwrap();
            assert!(restarted
                .store()
                .claim_controller_execution(id, true)
                .is_ok());
        });
    }

    #[test]
    fn exited_checkpoint_retains_delivery_across_fresh_store_open_and_pruning() {
        with_isolated_home(|home| {
            register_step_job_driver();
            let path = home.path().join("crash-jobs.sqlite");
            let mut request = request("true", Default::default());
            request.terminal_consumer = "test.crash".to_string();
            let harness = ControllerJobHarness::durable(
                Arc::new(StepJobDriver),
                to_value(&request).unwrap(),
                &path,
                "original",
            )
            .unwrap();
            let mut checkpoint = StepJobCheckpoint::waiting(request);
            checkpoint.phase = StepPhase::Exited;
            checkpoint.exit_code = Some(0);
            harness.store().fail_next_durable_writes(1);
            assert!(harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .is_err());
            assert!(
                harness
                    .store()
                    .pending_step_terminal_deliveries()
                    .unwrap()
                    .is_empty(),
                "a failed checkpoint cannot publish delivery"
            );
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            // No callback or completion has run: crash precisely at Exited.
            let restarted = harness.reopen(&path, "replacement").unwrap();
            assert_eq!(
                restarted
                    .store()
                    .pending_step_terminal_deliveries()
                    .unwrap()
                    .len(),
                1
            );
            restarted.recover_after_restart();
            let job = restarted.wait_until_terminal().unwrap();
            restarted
                .store()
                .prune_terminal_controller_jobs(STEP_JOB_TYPE, STEP_JOB_VERSION, &[job.id])
                .unwrap();
            let fresh = crate::api_jobs::JobStore::open_without_reconciliation(&path).unwrap();
            assert_eq!(fresh.pending_step_terminal_deliveries().unwrap().len(), 1);
            assert_eq!(
                crate::api_jobs::JobStore::active_count_at_path(&path, None).unwrap(),
                1,
                "pending custody keeps its job active in the daemon store"
            );
            let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = delivered.clone();
            register_step_terminal_consumer(
                "test.crash",
                Arc::new(move |_| {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(json!({"committed": true}))
                }),
            );
            std::thread::sleep(Duration::from_millis(1100));
            fresh.deliver_pending_step_terminals().unwrap();
            fresh.deliver_pending_step_terminals().unwrap();
            assert!(fresh.pending_step_terminal_deliveries().unwrap().is_empty());
            assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                crate::api_jobs::JobStore::active_count_at_path(&path, None).unwrap(),
                0
            );
        });
    }

    #[test]
    fn pre_spawn_and_spawn_failures_deliver_without_a_child() {
        with_isolated_home(|home| {
            for before_spawn in [true, false] {
                let mut request = request("true", Default::default());
                if before_spawn {
                    request.env.secret_refs.push(EnvSecretRef {
                        name: "UNRESOLVABLE_STEP_SECRET".to_string(),
                        owner: None,
                    });
                    register_step_secret_resolver(Arc::new(|_| Ok(vec![])));
                } else {
                    request.program = "/nonexistent/step-program".to_string();
                }
                let harness = step_harness(&request, home.path());
                harness.start_via_controller_boundary().unwrap();
                assert_eq!(
                    harness.wait_until_terminal().unwrap().status,
                    crate::api_jobs::JobStatus::Failed
                );
                let pending = harness.store().pending_step_terminal_deliveries().unwrap();
                assert_eq!(pending.len(), 1);
                assert!(pending[0].child.is_none());
                assert_eq!(pending[0].outcome, StepTerminalOutcome::Failed);
                assert!(pending[0].error.is_some());
                if !before_spawn {
                    let log = job_log(&harness);
                    assert!(log.contains("detached_launch_failed"), "{log}");
                    assert!(log.contains("spawn step program"), "{log}");
                }
            }
        });
    }

    /// Subprocess entry: its only side effect is behind the real start gate.
    #[test]
    fn gated_child_entry() {
        let Some(marker) = std::env::var_os("STEP_GATE_TEST_MARKER") else {
            return;
        };
        if await_step_child_start().unwrap() {
            std::fs::write(marker, "executed").unwrap();
        }
    }

    #[test]
    fn child_gate_requires_exact_running_identity_and_rejects_cancellation() {
        with_isolated_home(|home| {
            register_step_job_driver();
            let path = home.path().join("authorization.json");
            let request = request("true", Default::default());
            let harness = ControllerJobHarness::durable(
                Arc::new(StepJobDriver),
                to_value(&request).unwrap(),
                &path,
                "gate",
            )
            .unwrap();
            let pid = std::process::id();
            let child = StepChild {
                pid,
                start_identity: crate::process::process_start_identity(pid)
                    .unwrap()
                    .unwrap(),
            };
            let id = harness.job().unwrap().id;
            let mut checkpoint = StepJobCheckpoint::waiting(request);
            checkpoint.phase = StepPhase::Spawning;
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            assert_eq!(
                step_child_authorization(&path, id, &child).unwrap(),
                StepChildAuthorization::Waiting
            );
            checkpoint.phase = StepPhase::Running;
            checkpoint.child = Some(child.clone());
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            assert_eq!(
                step_child_authorization(&path, id, &child).unwrap(),
                StepChildAuthorization::Authorized
            );
            let before = crate::api_jobs::JobStore::store_generation_at_path(&path).unwrap();
            let mut other_pid = child.clone();
            other_pid.pid = pid + 1;
            assert_eq!(
                step_child_authorization(&path, id, &other_pid).unwrap(),
                StepChildAuthorization::Inert
            );
            assert_eq!(
                crate::api_jobs::JobStore::store_generation_at_path(&path).unwrap(),
                before,
                "authorization must be read-only"
            );
            let mut reused = child.clone();
            reused.start_identity = match child.start_identity {
                ProcessStartIdentity::Linux { starttime_ticks } => ProcessStartIdentity::Linux {
                    starttime_ticks: starttime_ticks + 1,
                },
                ProcessStartIdentity::Macos {
                    start_seconds,
                    start_microseconds,
                } => ProcessStartIdentity::Macos {
                    start_seconds: start_seconds + 1,
                    start_microseconds,
                },
            };
            assert_eq!(
                step_child_authorization(&path, id, &reused).unwrap(),
                StepChildAuthorization::Inert
            );
            harness
                .store()
                .request_controller_cancellation(id, "gate cancelled".to_string())
                .unwrap();
            assert_eq!(
                step_child_authorization(&path, id, &child).unwrap(),
                StepChildAuthorization::Inert
            );
        });
    }

    /// A step child's own re-execution and any deeper process it runs (a tool
    /// that invokes Homeboy) inherit the step environment. They pass the gate
    /// because they descend from the authorized child, and only then: a
    /// process that does not descend from the named child stays inert.
    #[test]
    fn child_gate_admits_descendants_of_the_authorized_child() {
        with_isolated_home(|home| {
            register_step_job_driver();
            let path = home.path().join("reexec-jobs.sqlite");
            let request = request("true", Default::default());
            let harness = ControllerJobHarness::durable(
                Arc::new(StepJobDriver),
                to_value(&request).unwrap(),
                &path,
                "reexec",
            )
            .unwrap();
            let pid = std::process::id();
            let this_process = StepChild {
                pid,
                start_identity: crate::process::process_start_identity(pid)
                    .unwrap()
                    .unwrap(),
            };
            let run_gated_child = |marker: &Path| {
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "daemon::step_job::tests::gated_child_entry",
                        "--nocapture",
                    ])
                    .env(STEP_JOB_ID_ENV, harness.job().unwrap().id.to_string())
                    .env(STEP_JOB_STORE_ENV, &path)
                    .env("STEP_GATE_TEST_MARKER", marker)
                    .status()
                    .unwrap()
            };

            // `; exit` keeps the shell from exec-ing, so the gated process is
            // a grandchild of this one.
            let run_nested_gated_child = |marker: &Path| {
                Command::new("/bin/sh")
                    .args([
                        "-c",
                        "\"$0\" --exact daemon::step_job::tests::gated_child_entry --nocapture; exit $?",
                    ])
                    .arg(std::env::current_exe().unwrap())
                    .env(STEP_JOB_ID_ENV, harness.job().unwrap().id.to_string())
                    .env(STEP_JOB_STORE_ENV, &path)
                    .env("STEP_GATE_TEST_MARKER", marker)
                    .status()
                    .unwrap()
            };

            let mut checkpoint = StepJobCheckpoint::waiting(request);
            checkpoint.phase = StepPhase::Running;
            checkpoint.child = Some(this_process.clone());
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            let reexecuted = home.path().join("reexecuted");
            assert!(run_gated_child(&reexecuted).success());
            assert!(
                reexecuted.exists(),
                "the authorized child's re-execution must act"
            );
            // A tool the child runs that invokes Homeboy in turn is a deeper
            // descendant; it acts on the child's behalf too.
            let nested = home.path().join("nested");
            assert!(run_nested_gated_child(&nested).success());
            assert!(
                nested.exists(),
                "a nested descendant of the authorized child must act"
            );

            let mut unrelated = this_process;
            unrelated.pid += 1;
            checkpoint.child = Some(unrelated);
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            let stranger = home.path().join("stranger");
            assert!(run_gated_child(&stranger).success());
            assert!(
                !stranger.exists(),
                "a process that does not descend from the named child must stay inert"
            );
            let nested_stranger = home.path().join("nested-stranger");
            assert!(run_nested_gated_child(&nested_stranger).success());
            assert!(
                !nested_stranger.exists(),
                "a nested process that does not descend from the named child must stay inert"
            );
        });
    }

    #[test]
    fn interrupted_spawning_delivers_typed_outcome_and_child_exits_inertly() {
        with_isolated_home(|home| {
            register_step_job_driver();
            let path = home.path().join("interrupted-jobs.sqlite");
            let request = request("true", Default::default());
            let harness = ControllerJobHarness::durable(
                Arc::new(StepJobDriver),
                to_value(&request).unwrap(),
                &path,
                "original",
            )
            .unwrap();
            let mut checkpoint = StepJobCheckpoint::waiting(request);
            checkpoint.phase = StepPhase::Spawning;
            harness
                .handle()
                .checkpoint(to_value(&checkpoint).unwrap())
                .unwrap();
            let marker = home.path().join("side-effect");
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "daemon::step_job::tests::gated_child_entry",
                    "--nocapture",
                ])
                .env(STEP_JOB_ID_ENV, harness.job().unwrap().id.to_string())
                .env(STEP_JOB_STORE_ENV, &path)
                .env("STEP_GATE_TEST_MARKER", &marker)
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(250));
            assert!(
                child.try_wait().unwrap().is_none(),
                "Spawning child must wait"
            );
            assert!(!marker.exists());
            let restarted = harness.reopen(&path, "replacement").unwrap();
            restarted.recover_after_restart();
            assert_eq!(
                restarted.wait_until_terminal().unwrap().status,
                crate::api_jobs::JobStatus::Failed
            );
            assert!(child.wait().unwrap().success());
            assert!(!marker.exists(), "unknown child must never act");
            let delivery = restarted
                .store()
                .pending_step_terminal_deliveries()
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(delivery.outcome, StepTerminalOutcome::SpawnInterrupted);
            assert!(delivery.child.is_none());
        });
    }
}

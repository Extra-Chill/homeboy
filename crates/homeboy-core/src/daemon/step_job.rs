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
//! again, and cancellation terminates the child's whole process tree. The
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
use crate::process::{ProcessIdentityState, ProcessStartIdentity};

pub const STEP_JOB_TYPE: &str = "step";
pub const STEP_JOB_VERSION: u32 = 1;
pub const STEP_JOB_REQUEST_SCHEMA: &str = "homeboy/step-job-request/v1";
const STEP_JOB_CHECKPOINT_SCHEMA: &str = "homeboy/step-job-checkpoint/v1";
const STEP_EXIT_SCHEMA: &str = "homeboy/step-exit/v1";

/// The daemon job id, injected into every step child so the child can bind
/// its own durable records to the job that supervises it.
pub const STEP_JOB_ID_ENV: &str = "HOMEBOY_STEP_JOB_ID";
/// Exact path of the supervising daemon job store.
pub const STEP_JOB_STORE_ENV: &str = "HOMEBOY_STEP_JOB_STORE";
/// Combined stdout/stderr of the child, below the job's artifact directory.
pub const STEP_LOG_FILE: &str = "step.log";
/// The observed exit of a child this daemon reaped, below the job's artifact
/// directory. Absent when the child was re-adopted after a daemon restart.
pub const STEP_EXIT_FILE: &str = "exit.json";

const WAIT_POLL: Duration = Duration::from_millis(250);
const SUPERVISE_POLL: Duration = Duration::from_millis(100);
const CANCEL_STOP_DEADLINE: Duration = Duration::from_secs(15);

/// Conditions that must hold before the child is spawned.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StepJobRequest {
    pub schema: String,
    pub terminal_consumer: String,
    /// Deterministic idempotency key. Submitting the same key returns the
    /// existing job instead of running the child again.
    pub key: String,
    /// Absolute path of the executable.
    pub program: String,
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
            program: program.into(),
            args,
            cwd: cwd.into(),
            env,
            linked_durable_run_id: None,
            wait: StepWait::default(),
        }
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

    fn parse(value: &Value) -> Result<Self> {
        let request: Self = serde_json::from_value(value.clone())
            .map_err(|error| invalid(format!("invalid step request: {error}")))?;
        request.validate()?;
        Ok(request)
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
            "args": crate::redaction::redact_argv(&self.args),
            "cwd": self.cwd,
            "env": {
                "public_env_names": self.env.public_env.keys().collect::<Vec<_>>(),
                "secret_ref_names": self.env.secret_refs.iter().map(|secret| &secret.name).collect::<Vec<_>>(),
            },
            "linked_durable_run_id": self.linked_durable_run_id,
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
#[serde(deny_unknown_fields)]
pub struct StepChild {
    pub pid: u32,
    pub start_identity: ProcessStartIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
/// Reports whether a runner can accept work now.
pub type StepRunnerEligibility = Arc<dyn Fn(&str) -> bool + Send + Sync>;
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

/// The exit code a daemon recorded for a step child it reaped, if any.
pub fn recorded_step_exit_code(job_id: &str) -> Option<i32> {
    let bytes = std::fs::read(step_job_dir(job_id).ok()?.join(STEP_EXIT_FILE)).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    (value["schema"] == STEP_EXIT_SCHEMA)
        .then(|| value["exit_code"].as_i64())
        .flatten()
        .and_then(|code| i32::try_from(code).ok())
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
            "exit_code": progress.get("exit_code").cloned().unwrap_or(Value::Null),
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
                "exit_code": error.details.get("exit_code").cloned().unwrap_or(Value::Null),
                "step_outcome": error.details.get("step_outcome").cloned().unwrap_or(Value::Null),
                "step_child": error.details.get("step_child").cloned().unwrap_or(Value::Null),
            }),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        StepJobRequest::parse(request).map(|_| ())
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
            (StepPhase::Running, Some(child)) => stop_child_tree(&child),
            _ => Ok(()),
        }
    }
}

fn run_step(mut checkpoint: StepJobCheckpoint, job: &ControllerJobHandle) -> Result<Value> {
    let job_id = job.job_id();
    match checkpoint.phase {
        StepPhase::Exited => return finish(checkpoint),
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
            supervise_adopted(&child, job)?;
            checkpoint.exit_code = recorded_step_exit_code(&job_id);
            checkpoint.phase = StepPhase::Exited;
            job.checkpoint(to_value(&checkpoint)?)?;
            return finish(checkpoint);
        }
        StepPhase::Waiting => {}
    }

    job.progress(checkpoint.progress())?;
    if !await_wait(&checkpoint.request.wait, job)? {
        return Ok(checkpoint.result());
    }

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

    checkpoint.phase = StepPhase::Spawning;
    job.checkpoint(to_value(&checkpoint)?)?;

    let mut command = Command::new(&request.program);
    command
        .args(&request.args)
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
    let mut process = command.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("spawn step program {}", request.program)),
        )
    })?;
    // Capture the start identity while the child is still unreaped.
    let start_identity = match crate::process::process_start_identity(process.id()) {
        Ok(Some(identity)) => identity,
        other => {
            let _ = crate::process::terminate_process_tree(process.id());
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
        let _ = crate::process::terminate_process_tree(child.pid);
        let _ = process.wait();
        error.details["step_child"] = to_value(&child)?;
        return Err(error);
    }
    job.progress(checkpoint.progress())?;

    loop {
        if job.is_cancelled() {
            let _ = stop_child_tree(&child);
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
    checkpoint.phase = StepPhase::Exited;
    write_exit_record(&dir, &checkpoint);
    job.checkpoint(to_value(&checkpoint)?)?;
    job.progress(checkpoint.progress())?;
    finish(checkpoint)
}

fn finish(checkpoint: StepJobCheckpoint) -> Result<Value> {
    match checkpoint.exit_code {
        Some(code) if code != 0 => {
            let mut error =
                Error::internal_unexpected(format!("step child exited with status {code}"));
            error.details["exit_code"] = json!(code);
            Err(error)
        }
        _ => Ok(checkpoint.result()),
    }
}

/// Returns `false` when the job was cancelled while waiting.
fn await_wait(wait: &StepWait, job: &ControllerJobHandle) -> Result<bool> {
    let not_before = wait
        .not_before
        .as_deref()
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|error| invalid(format!("step wait.not_before is invalid: {error}")))?;
    let probe = match wait.runner_eligible.as_deref() {
        Some(runner) => Some((
            runner.to_string(),
            runner_eligibility()
                .read()
                .expect("step runner eligibility lock")
                .clone()
                .ok_or_else(|| {
                    Error::internal_unexpected(
                        "no runner eligibility probe is registered for step waits",
                    )
                })?,
        )),
        None => None,
    };
    loop {
        if job.is_cancelled() {
            return Ok(false);
        }
        let time_ready = not_before.is_none_or(|instant| chrono::Utc::now() >= instant);
        let runner_ready = probe.as_ref().is_none_or(|(runner, probe)| probe(runner));
        if time_ready && runner_ready {
            return Ok(true);
        }
        std::thread::sleep(WAIT_POLL);
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
fn supervise_adopted(child: &StepChild, job: &ControllerJobHandle) -> Result<()> {
    loop {
        if job.is_cancelled() {
            return stop_child_tree(child);
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

/// Terminate the child's process tree when it is still the admitted child,
/// then wait until it is provably gone.
fn stop_child_tree(child: &StepChild) -> Result<()> {
    if child_state(child) == ProcessIdentityState::Live {
        crate::process::terminate_process_tree(child.pid)?;
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

fn write_exit_record(dir: &Path, checkpoint: &StepJobCheckpoint) {
    let record = json!({
        "schema": STEP_EXIT_SCHEMA,
        "exit_code": checkpoint.exit_code,
        "observed_at": chrono::Utc::now().to_rfc3339(),
    });
    let _ = std::fs::write(
        dir.join(STEP_EXIT_FILE),
        serde_json::to_vec(&record).unwrap_or_default(),
    );
}

pub(crate) fn terminal_delivery(
    job_id: &str,
    request: &Value,
    checkpoint: Option<&Value>,
    status: Option<crate::api_jobs::JobStatus>,
    error: Option<Value>,
    cancelled: bool,
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
        exit_code: parsed.as_ref().and_then(|cp| cp.exit_code),
        cancelled,
        log_path: step_job_dir(job_id)
            .map(|dir| dir.join(STEP_LOG_FILE))
            .unwrap_or_default(),
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::{with_isolated_home, ControllerJobHarness};

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
            let request = request("test \"$STEP_TEST_SECRET\" = resolved-value", env);
            let harness = step_harness(&request, home.path());
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
            let harness = step_harness(&request_with(&missing), home.path());
            harness.start_via_controller_boundary().unwrap();
            let job = harness.wait_until_terminal().unwrap();
            std::env::remove_var("STEP_TEST_UNDECLARED_SECRET");
            assert_eq!(job.status, crate::api_jobs::JobStatus::Failed);
        });
    }

    fn request_with(env: &EnvMaterializationPlan) -> StepJobRequest {
        request("true", env.clone())
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
        });
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

    #[test]
    fn a_running_checkpoint_is_re_adopted_without_a_second_spawn() {
        with_isolated_home(|home| {
            let path = home.path().join("step-jobs.json");
            let counter = home.path().join("spawns");
            let script = format!("echo spawn >> {}; sleep 1", counter.display());
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
            let mut process = Command::new("/bin/sh").args(&request.args).spawn().unwrap();
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
            let path = home.path().join("crash-jobs.json");
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
                crate::api_jobs::JobStore::active_count_at_path(&path).unwrap(),
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
                crate::api_jobs::JobStore::active_count_at_path(&path).unwrap(),
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
            let path = home.path().join("reexec-jobs.json");
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
            let path = home.path().join("interrupted-jobs.json");
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

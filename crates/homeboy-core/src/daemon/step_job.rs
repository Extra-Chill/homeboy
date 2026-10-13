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
//! record remains the outcome authority, and registered exit observers let the
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
    ) -> Self {
        Self {
            schema: STEP_JOB_REQUEST_SCHEMA.to_string(),
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

/// What an exit observer learns when a supervised child is gone.
#[derive(Debug, Clone)]
pub struct StepExit {
    pub job_id: String,
    pub key: String,
    pub linked_durable_run_id: Option<String>,
    pub child: StepChild,
    /// Known only when this daemon reaped the child itself.
    pub exit_code: Option<i32>,
    /// The child was stopped because the step job was cancelled.
    pub cancelled: bool,
    pub log_path: PathBuf,
}

/// Resolves secret references to values at spawn time. Implementations must
/// not read the daemon's own process environment.
pub type StepSecretResolver =
    Arc<dyn Fn(&[EnvSecretRef]) -> Result<Vec<(String, String)>> + Send + Sync>;
/// Reports whether a runner can accept work now.
pub type StepRunnerEligibility = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// Lets the domain that owns a linked run reconcile it after its child exits.
pub type StepExitObserver = Arc<dyn Fn(&StepExit) + Send + Sync>;

fn secret_resolver() -> &'static RwLock<Option<StepSecretResolver>> {
    static RESOLVER: OnceLock<RwLock<Option<StepSecretResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| RwLock::new(None))
}

fn runner_eligibility() -> &'static RwLock<Option<StepRunnerEligibility>> {
    static PROBE: OnceLock<RwLock<Option<StepRunnerEligibility>>> = OnceLock::new();
    PROBE.get_or_init(|| RwLock::new(None))
}

fn exit_observers() -> &'static RwLock<Vec<StepExitObserver>> {
    static OBSERVERS: OnceLock<RwLock<Vec<StepExitObserver>>> = OnceLock::new();
    OBSERVERS.get_or_init(|| RwLock::new(Vec::new()))
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

pub fn register_step_exit_observer(observer: StepExitObserver) {
    exit_observers()
        .write()
        .expect("step exit observers lock")
        .push(observer);
}

/// Register the `step` controller-job driver. Idempotent.
pub fn register_step_job_driver() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
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
        StepPhase::Exited => return Ok(checkpoint.result()),
        StepPhase::Spawning => {
            // The daemon stopped between starting a spawn and recording the
            // child. Its identity is unknowable, so never spawn a second one.
            return Err(Error::internal_unexpected(
                "step spawn was interrupted before its child identity was recorded",
            ));
        }
        StepPhase::Running => {
            let child = checkpoint.child.clone().expect("validated running child");
            job.progress(checkpoint.progress())?;
            supervise_adopted(&child, job)?;
            checkpoint.phase = StepPhase::Exited;
            job.checkpoint(to_value(&checkpoint)?)?;
            notify_exit(&job_id, &checkpoint, &child, job.is_cancelled());
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
    if let Err(error) = job.checkpoint(to_value(&checkpoint)?) {
        let _ = crate::process::terminate_process_tree(child.pid);
        let _ = process.wait();
        return Err(error);
    }
    job.progress(checkpoint.progress())?;

    loop {
        if job.is_cancelled() {
            let _ = stop_child_tree(&child);
            let _ = process.wait();
            checkpoint.phase = StepPhase::Exited;
            let _ = job.checkpoint(to_value(&checkpoint)?);
            notify_exit(&job_id, &checkpoint, &child, true);
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
    notify_exit(&job_id, &checkpoint, &child, false);
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

fn notify_exit(job_id: &str, checkpoint: &StepJobCheckpoint, child: &StepChild, cancelled: bool) {
    let observers = exit_observers()
        .read()
        .expect("step exit observers lock")
        .clone();
    if observers.is_empty() {
        return;
    }
    let exit = StepExit {
        job_id: job_id.to_string(),
        key: checkpoint.request.key.clone(),
        linked_durable_run_id: checkpoint.request.linked_durable_run_id.clone(),
        child: child.clone(),
        exit_code: checkpoint.exit_code,
        cancelled,
        log_path: step_job_dir(job_id)
            .map(|dir| dir.join(STEP_LOG_FILE))
            .unwrap_or_default(),
    };
    for observer in observers {
        observer(&exit);
    }
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

    fn request(script: &str, env: EnvMaterializationPlan) -> StepJobRequest {
        StepJobRequest::new(
            format!("test-step:{}", uuid::Uuid::new_v4()),
            "/bin/sh",
            vec!["-c".to_string(), script.to_string()],
            std::env::temp_dir().display().to_string(),
            env,
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
                .filter(|name| !matches!(*name, "PWD" | "SHLVL" | "_"))
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
}

//! `workflow`: one instance of a `homeboy/workflow/v1` definition.
//!
//! The instance is a daemon job whose checkpoint is the instance state. It
//! never runs work itself. Each pass it derives events from the job ledger
//! (finished child step jobs, recorded approvals, time), feeds them to the pure
//! [`crate::workflow::advance`], and commits the new state together with the
//! step jobs it submits and cancels in one store transaction
//! ([`JobStore::commit_controller_advance`]). Only after that commit does it
//! start the new step jobs, so a crash at any boundary either loses a whole
//! decision or replays it against existing submission keys.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::api_jobs::{JobEventKind, JobStatus, JobStore};
use crate::daemon::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobExecutionOwner, ControllerJobHandle,
    ControllerJobPublicError,
};
use crate::daemon::step_job::{StepJobRequest, StepWait};
use crate::env_materialization_plan::EnvMaterializationPlan;
use crate::error::{Error, Result};
use crate::workflow::{
    advance, Command, Event, FinishedStatus, InstanceState, InstanceStatus, StepPhase, StepResult,
    StepSubmission, SubmissionKind, WorkflowDefinition,
};

pub const WORKFLOW_JOB_TYPE: &str = "workflow";
pub const WORKFLOW_JOB_VERSION: u32 = 1;
pub const WORKFLOW_JOB_REQUEST_SCHEMA: &str = "homeboy/workflow-job-request/v1";
/// The file an `exec` step writes its JSON output to.
pub const STEP_OUTPUT_ENV: &str = "HOMEBOY_STEP_OUTPUT";
const OUTPUT_FILE: &str = "output.json";
/// Outputs larger than this are not carried into the instance state.
const MAX_OUTPUT_BYTES: u64 = 256 * 1024;
const POLL: Duration = Duration::from_millis(500);

/// The admitted, immutable request for one workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowJobRequest {
    pub schema: String,
    /// `wf-<definition-id>-<hash>`; also the job's idempotency key.
    pub instance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    /// The definition, pinned at admission.
    pub definition: WorkflowDefinition,
    pub definition_digest: String,
    pub inputs: BTreeMap<String, Value>,
    /// Absolute path of the Homeboy executable `homeboy` and `cook` steps run.
    pub homeboy_program: String,
    /// Absolute working directory of every step without its own `cwd`.
    pub cwd: String,
    /// The workflow's declared environment: public values and secret refs.
    #[serde(default)]
    pub env: EnvMaterializationPlan,
    /// Homeboy's own admitted runtime environment for `homeboy`/`cook` steps.
    #[serde(default)]
    pub homeboy_env: EnvMaterializationPlan,
    /// `exec` and command-gate programs resolved to absolute paths at admission.
    #[serde(default)]
    pub programs: BTreeMap<String, String>,
}

impl WorkflowJobRequest {
    pub fn parse(value: &Value) -> Result<Self> {
        let request: Self = serde_json::from_value(value.clone())
            .map_err(|error| invalid(format!("invalid workflow request: {error}")))?;
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<()> {
        if self.schema != WORKFLOW_JOB_REQUEST_SCHEMA {
            return Err(invalid(format!(
                "workflow requests require schema `{WORKFLOW_JOB_REQUEST_SCHEMA}`"
            )));
        }
        // Cook flags were checked against the CLI's clap tree at admission.
        self.definition.validate(&|_| true)?;
        if self.definition.digest() != self.definition_digest {
            return Err(invalid(
                "workflow definition digest does not match its body",
            ));
        }
        if self
            .definition
            .instance_id(&self.inputs, self.instance_name.as_deref())
            != self.instance
        {
            return Err(invalid(
                "workflow instance id does not match its definition and inputs",
            ));
        }
        for path in [&self.homeboy_program, &self.cwd]
            .into_iter()
            .chain(self.programs.values())
        {
            if !Path::new(path).is_absolute() {
                return Err(invalid(format!("`{path}` must be an absolute path")));
            }
        }
        Ok(())
    }

    /// The controller-job submission envelope for this request.
    pub fn submission(&self) -> Result<Value> {
        self.validate()?;
        Ok(json!({
            "type": WORKFLOW_JOB_TYPE,
            "version": WORKFLOW_JOB_VERSION,
            "idempotency_key": self.instance,
            "request": to_value(self)?,
        }))
    }

    fn initial_state(&self) -> InstanceState {
        InstanceState::new(&self.definition, &self.instance, self.inputs.clone())
    }
}

/// Resolves a Cook step's durable outcome (its run state) once its run is
/// terminal. Registered by the agent-task layer.
pub type WorkflowRunOutcomeResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

fn run_outcome_resolver() -> &'static RwLock<Option<WorkflowRunOutcomeResolver>> {
    static RESOLVER: OnceLock<RwLock<Option<WorkflowRunOutcomeResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| RwLock::new(None))
}

pub fn register_workflow_run_outcome_resolver(resolver: WorkflowRunOutcomeResolver) {
    *run_outcome_resolver()
        .write()
        .expect("workflow run outcome resolver lock") = Some(resolver);
}

/// Register the `workflow` controller-job driver. Idempotent.
pub fn register_workflow_job_driver() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        crate::daemon::step_job::register_step_job_driver();
        controller_job_driver::register_controller_job_driver(Arc::new(WorkflowJobDriver))
            .expect("register workflow controller job driver");
    });
}

pub struct WorkflowJobDriver;

impl ControllerJobDriver for WorkflowJobDriver {
    fn job_type(&self) -> &'static str {
        WORKFLOW_JOB_TYPE
    }

    fn version(&self) -> u32 {
        WORKFLOW_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        let request = WorkflowJobRequest::parse(request)?;
        Ok(json!({
            "schema": request.schema,
            "instance": request.instance,
            "workflow": request.definition.id,
            "definition_digest": request.definition_digest,
            "inputs": request.inputs.keys().collect::<Vec<_>>(),
            "env": {
                "public_env_names": request.env.public_env.keys().collect::<Vec<_>>(),
                "secret_ref_names": request.env.secret_refs.iter().map(|secret| &secret.name).collect::<Vec<_>>(),
            },
        }))
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        Ok(progress.clone())
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        Ok(result.clone())
    }

    fn public_error(&self, error: &Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: crate::redaction::redact_string(&error.message),
            data: json!({
                "code": error.code.as_str(),
                "instance_status": error.details.get("instance_status").cloned().unwrap_or(Value::Null),
                "workflow": error.details.get("workflow").cloned().unwrap_or(Value::Null),
            }),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        WorkflowJobRequest::parse(request).map(|_| ())
    }

    fn execution_owner(
        &self,
        request: &Value,
        _checkpoint: Option<&Value>,
    ) -> Result<Option<ControllerJobExecutionOwner>> {
        // The instance runs no process: its work is its child step jobs.
        WorkflowJobRequest::parse(request)?;
        Ok(Some(ControllerJobExecutionOwner::None))
    }

    fn recovery_checkpoint(&self, request: &Value) -> Result<Option<Value>> {
        Ok(Some(to_value(
            WorkflowJobRequest::parse(request)?.initial_state(),
        )?))
    }

    fn prepare(&self, request: Value) -> Result<Value> {
        to_value(WorkflowJobRequest::parse(&request)?.initial_state())
    }

    fn execute(&self, prepared: Value, job: ControllerJobHandle) -> Result<Value> {
        self.resume(prepared, job)
    }

    fn resume(&self, checkpoint: Value, job: ControllerJobHandle) -> Result<Value> {
        let store = job.job().store().clone();
        let state = store.controller_job_state(job.job().job_id())?;
        let request = WorkflowJobRequest::parse(&state.request)?;
        let instance: InstanceState = serde_json::from_value(checkpoint)
            .map_err(|error| invalid(format!("invalid workflow checkpoint: {error}")))?;
        run_instance(&request, instance, &job, &store)
    }

    fn cancel(&self, _prepared: &Value) -> Result<()> {
        // Cancelling the instance job cancels its child step jobs in the
        // daemon (`cancel_controller_job`); the run loop observes the durable
        // cancellation and returns.
        Ok(())
    }
}

fn run_instance(
    request: &WorkflowJobRequest,
    mut state: InstanceState,
    job: &ControllerJobHandle,
    store: &JobStore,
) -> Result<Value> {
    let job_id = job.job().job_id();
    job.progress(projection(&state, Some(store)))?;
    loop {
        if job.is_cancelled() {
            return Ok(json!({ "instance": request.instance, "status": "cancelled" }));
        }
        if state.status.is_terminal() {
            return finish(request, &state, store);
        }
        let now_ms = now_ms();
        for event in observe(request, &state, job_id, store) {
            let (next, commands) = advance(&request.definition, &state, &event, now_ms);
            if next == state && commands.is_empty() {
                continue;
            }
            match commit(request, job_id, &next, &commands, store) {
                Ok(()) => {}
                Err(_) if job.is_cancelled() => {
                    return Ok(json!({ "instance": request.instance, "status": "cancelled" }));
                }
                Err(error) => return Err(error),
            }
            state = next;
            job.progress(projection(&state, Some(store)))?;
            if state.status.is_terminal() {
                break;
            }
        }
        // A commit that lands before a crash leaves its step jobs queued;
        // start any awaited job that is not yet running.
        for key in state.awaited_keys() {
            if let Some(child) = store.controller_job_for_key(&key) {
                if child.status == JobStatus::Queued {
                    let _ = crate::daemon::start_controller_job(child.id, store);
                }
            }
        }
        if !state.status.is_terminal() {
            std::thread::sleep(POLL);
        }
    }
}

fn finish(request: &WorkflowJobRequest, state: &InstanceState, store: &JobStore) -> Result<Value> {
    let result = projection(state, Some(store));
    match state.status {
        InstanceStatus::Succeeded => Ok(result),
        status => {
            let mut error = Error::internal_unexpected(format!(
                "workflow `{}` {}: {}",
                request.instance,
                status.as_str(),
                state.reason.as_deref().unwrap_or("no reason recorded")
            ));
            error.details["instance_status"] = json!(status.as_str());
            error.details["workflow"] = result;
            Err(error)
        }
    }
}

/// Every event the ledger holds for this state, in a deterministic order.
fn observe(
    request: &WorkflowJobRequest,
    state: &InstanceState,
    job_id: Uuid,
    store: &JobStore,
) -> Vec<Event> {
    let mut events = Vec::new();
    if state.started_at_ms.is_none() {
        events.push(Event::Start);
    }
    for key in state.awaited_keys() {
        if let Some(result) = step_result(request, state, &key, store) {
            events.push(Event::StepFinished(result));
        }
    }
    let awaiting = state
        .steps
        .iter()
        .filter(|(_, step)| step.phase == StepPhase::AwaitingApproval)
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    if !awaiting.is_empty() {
        for event in store.events(job_id).unwrap_or_default() {
            let Some(approval) = event
                .data
                .as_ref()
                .and_then(|data| data.get("workflow_approval"))
            else {
                continue;
            };
            let (Some(step), Some(iteration)) =
                (approval["step"].as_str(), approval["iteration"].as_u64())
            else {
                continue;
            };
            if awaiting.contains(&step) && iteration == u64::from(state.iteration) {
                events.push(Event::Approved {
                    step: step.to_string(),
                    iteration: state.iteration,
                });
            }
        }
    }
    events.push(Event::Tick);
    events
}

/// The result of one awaited step job, once its job is terminal.
fn step_result(
    request: &WorkflowJobRequest,
    state: &InstanceState,
    key: &str,
    store: &JobStore,
) -> Option<StepResult> {
    let job = store.controller_job_for_key(key)?;
    let status = match job.status {
        JobStatus::Succeeded => FinishedStatus::Succeeded,
        JobStatus::Failed => FinishedStatus::Failed,
        JobStatus::Cancelled => FinishedStatus::Cancelled,
        JobStatus::Queued | JobStatus::Running => return None,
    };
    let step = state
        .steps
        .iter()
        .find(|(_, step)| step.key.as_deref() == Some(key));
    let kind = step.map(|(id, _)| {
        request
            .definition
            .steps
            .iter()
            .find(|definition| &definition.id == id)
            .map(|definition| definition.kind())
            .expect("state steps match the pinned definition")
    });
    let run_id = step.and_then(|(_, step)| step.run_id.clone());
    let output_path = output_path(&request.instance, key).ok();
    let (outcome, output) = match kind {
        Some(crate::workflow::definition::StepKind::Cook(_)) => {
            let outcome = run_id.as_deref().and_then(|run_id| {
                run_outcome_resolver()
                    .read()
                    .expect("workflow run outcome resolver lock")
                    .as_ref()
                    .and_then(|resolve| resolve(run_id))
            });
            (
                outcome,
                output_path.and_then(|path| read_envelope_data(&path)),
            )
        }
        Some(crate::workflow::definition::StepKind::Homeboy(_)) => {
            (None, output_path.and_then(|path| read_envelope_data(&path)))
        }
        Some(_) => (None, output_path.and_then(|path| read_output(&path))),
        None => (None, None),
    };
    Some(StepResult {
        key: key.to_string(),
        status,
        outcome,
        output,
        run_id,
    })
}

fn read_output(path: &Path) -> Option<Value> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_OUTPUT_BYTES {
        return Some(
            json!({ "error": "output exceeds the workflow output limit", "bytes": metadata.len() }),
        );
    }
    let text = std::fs::read_to_string(path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

/// A Homeboy command's output is its JSON envelope's `data`.
fn read_envelope_data(path: &Path) -> Option<Value> {
    let value = read_output(path)?;
    Some(value.get("data").cloned().unwrap_or(value))
}

/// Where one step job writes its output: below the instance's artifacts.
pub fn output_path(instance: &str, key: &str) -> Result<PathBuf> {
    Ok(crate::paths::artifact_root()?
        .join("workflows")
        .join(crate::paths::sanitize_path_segment(instance))
        .join(crate::paths::sanitize_path_segment(&key.replace('/', "__")))
        .join(OUTPUT_FILE))
}

/// Apply one decision: checkpoint, submissions and cancellations commit in
/// one transaction; starting and signalling follow it.
fn commit(
    request: &WorkflowJobRequest,
    job_id: Uuid,
    next: &InstanceState,
    commands: &[Command],
    store: &JobStore,
) -> Result<()> {
    let mut admissions = Vec::new();
    let mut cancellations = Vec::new();
    for command in commands {
        match command {
            Command::SubmitStep(submission) => {
                let step = step_request(request, submission)?;
                let envelope: crate::daemon::ControllerJobRequest =
                    serde_json::from_value(step.submission()?).map_err(|error| {
                        Error::internal_json(error.to_string(), Some("step submission".into()))
                    })?;
                admissions.push((
                    format!("controller.{}", envelope.job_type),
                    envelope.idempotency_key.clone(),
                    crate::daemon::controller_job_state(&envelope)?,
                ));
            }
            Command::CancelStep { key, reason } => {
                cancellations.push((key.clone(), reason.clone()))
            }
            Command::Complete { .. } => {}
        }
    }
    let admitted = store.commit_controller_advance(
        job_id,
        to_value(next)?,
        admissions,
        cancellations.clone(),
    )?;
    for child in admitted {
        if let Ok(job) = store.get(child) {
            let _ = crate::daemon::record_controller_job_generation(&job);
        }
        let _ = crate::daemon::start_controller_job(child, store);
    }
    for (key, reason) in cancellations {
        if let Some(child) = store.controller_job_for_key(&key) {
            if !child.status.is_terminal() {
                let _ = crate::daemon::cancel_controller_job(child.id, &reason, store);
            }
        }
    }
    Ok(())
}

/// Compile one submission into the `step` job that runs it.
pub fn step_request(
    request: &WorkflowJobRequest,
    submission: &StepSubmission,
) -> Result<StepJobRequest> {
    let output = output_path(&request.instance, &submission.key)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let output = output.display().to_string();
    let homeboy_env = || {
        let mut env = request.homeboy_env.clone();
        env.public_env.extend(request.env.public_env.clone());
        env.secret_refs.extend(request.env.secret_refs.clone());
        env.normalize();
        env
    };
    let exec_env = || {
        let mut env = request.env.clone();
        env.public_env
            .insert(STEP_OUTPUT_ENV.to_string(), output.clone());
        env.normalize();
        env
    };
    let program = |name: &str| -> Result<String> {
        if Path::new(name).is_absolute() {
            return Ok(name.to_string());
        }
        request.programs.get(name).cloned().ok_or_else(|| {
            invalid(format!(
                "program `{name}` was not resolved when the workflow was admitted"
            ))
        })
    };
    let mut linked_run = None;
    let (program, args, cwd, env) = match &submission.kind {
        SubmissionKind::Exec {
            program: name,
            args,
            cwd,
        } => (
            program(name)?,
            args.clone(),
            cwd.as_deref()
                .map(|cwd| Path::new(&request.cwd).join(cwd).display().to_string())
                .unwrap_or_else(|| request.cwd.clone()),
            exec_env(),
        ),
        SubmissionKind::Homeboy { args } => {
            let mut argv = vec!["--output".to_string(), output.clone()];
            argv.extend(args.iter().cloned());
            (
                request.homeboy_program.clone(),
                argv,
                request.cwd.clone(),
                homeboy_env(),
            )
        }
        SubmissionKind::Cook { args, cook_id } => {
            let mut argv = vec![
                "--wait".to_string(),
                "--output".to_string(),
                output.clone(),
                "agent-task".to_string(),
                "cook".to_string(),
            ];
            argv.extend(args.iter().cloned());
            argv.push("--cook-id".to_string());
            argv.push(cook_id.clone());
            linked_run = Some(cook_id.clone());
            (
                request.homeboy_program.clone(),
                argv,
                request.cwd.clone(),
                homeboy_env(),
            )
        }
        SubmissionKind::Gate { argv } => {
            let (first, rest) = argv
                .split_first()
                .ok_or_else(|| invalid("a command gate needs a program"))?;
            if first == "homeboy" {
                (
                    request.homeboy_program.clone(),
                    rest.to_vec(),
                    request.cwd.clone(),
                    homeboy_env(),
                )
            } else {
                (
                    program(first)?,
                    rest.to_vec(),
                    request.cwd.clone(),
                    exec_env(),
                )
            }
        }
    };
    let mut step = StepJobRequest::new(submission.key.clone(), program, args, cwd, env);
    if let Some(run_id) = linked_run {
        step = step.with_linked_durable_run_id(run_id);
    }
    if let Some(not_before_ms) = submission.not_before_ms {
        let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(not_before_ms as i64)
            .ok_or_else(|| invalid("retry backoff instant is out of range"))?;
        step = step.with_wait(StepWait {
            not_before: Some(at.to_rfc3339()),
            runner_eligible: None,
        });
    }
    Ok(step)
}

/// A pure projection of the instance for status and progress.
pub fn projection(state: &InstanceState, store: Option<&JobStore>) -> Value {
    let job_of = |key: &str| -> Value {
        store
            .and_then(|store| store.controller_job_for_key(key))
            .map(|job| json!({ "job_id": job.id, "status": job.status }))
            .unwrap_or(Value::Null)
    };
    json!({
        "instance": state.instance,
        "status": state.status.as_str(),
        "iteration": state.iteration,
        "step_runs": state.step_runs,
        "reason": state.reason,
        "steps": state.steps.iter().map(|(id, step)| {
            (id.clone(), json!({
                "phase": step.phase,
                "attempt": step.attempt,
                "outcome": step.outcome,
                "carried": step.carried,
                "key": step.key,
                "job": step.key.as_deref().map(job_of).unwrap_or(Value::Null),
                "run_id": step.run_id,
                "prompt": step.prompt,
                "error": step.error,
                "gates": step.gates.values().map(|gate| json!({
                    "key": gate.key,
                    "passed": gate.passed,
                    "job": job_of(&gate.key),
                })).collect::<Vec<_>>(),
            }))
        }).collect::<serde_json::Map<_, _>>(),
        "until": state.until.values().map(|gate| json!({
            "key": gate.key,
            "passed": gate.passed,
            "job": job_of(&gate.key),
        })).collect::<Vec<_>>(),
        "history": state.history,
    })
}

/// The instance job admitted under `instance`, with its state.
fn instance_job(
    store: &JobStore,
    instance: &str,
) -> Result<(crate::api_jobs::Job, crate::api_jobs::ControllerJobState)> {
    let job = store.controller_job_for_key(instance).ok_or_else(|| {
        Error::validation_invalid_argument(
            "instance",
            format!("no workflow instance `{instance}` in this daemon"),
            Some(instance.to_string()),
            None,
        )
    })?;
    let state = store.controller_job_state(job.id)?;
    if state.job_type != WORKFLOW_JOB_TYPE {
        return Err(invalid(format!("`{instance}` is not a workflow instance")));
    }
    Ok((job, state))
}

fn instance_view(
    job: &crate::api_jobs::Job,
    controller: &crate::api_jobs::ControllerJobState,
    store: &JobStore,
) -> Value {
    let request = WorkflowJobRequest::parse(&controller.request).ok();
    let state = controller
        .checkpoint
        .as_ref()
        .and_then(|checkpoint| serde_json::from_value::<InstanceState>(checkpoint.clone()).ok())
        .or_else(|| request.as_ref().map(WorkflowJobRequest::initial_state));
    let mut view = state
        .as_ref()
        .map(|state| projection(state, Some(store)))
        .unwrap_or_else(|| json!({}));
    // The job row is the terminal authority: a cancelled job is a cancelled
    // instance whatever its last checkpoint said.
    let status = match job.status {
        JobStatus::Cancelled => "cancelled".to_string(),
        JobStatus::Failed
            if state
                .as_ref()
                .is_some_and(|state| !state.status.is_terminal()) =>
        {
            "failed".to_string()
        }
        _ => state
            .as_ref()
            .map(|state| state.status.as_str().to_string())
            .unwrap_or_else(|| "queued".to_string()),
    };
    view["status"] = json!(status);
    view["job"] = json!({
        "job_id": job.id,
        "status": job.status,
        "created_at_ms": job.created_at_ms,
        "finished_at_ms": job.finished_at_ms,
        "cancellation_requested": controller.cancellation_requested,
    });
    if let Some(request) = request {
        view["instance"] = json!(request.instance);
        view["workflow"] = json!(request.definition.id);
        view["definition_digest"] = json!(request.definition_digest);
        view["inputs"] = json!(request.inputs);
    }
    view
}

/// `GET /workflows/<instance>`.
pub(crate) fn workflow_status(store: &JobStore, instance: &str) -> Result<Value> {
    let (job, controller) = instance_job(store, instance)?;
    Ok(instance_view(&job, &controller, store))
}

/// `GET /workflows`.
pub(crate) fn workflow_list(store: &JobStore) -> Value {
    let instances = store
        .controller_jobs_of_type(WORKFLOW_JOB_TYPE)
        .iter()
        .map(|(job, controller)| {
            let view = instance_view(job, controller, store);
            json!({
                "instance": view["instance"],
                "workflow": view["workflow"],
                "status": view["status"],
                "iteration": view["iteration"],
                "step_runs": view["step_runs"],
                "job_id": job.id,
                "created_at_ms": job.created_at_ms,
            })
        })
        .collect::<Vec<_>>();
    json!({ "entities": instances })
}

/// `POST /workflows/<instance>/approvals`: record a durable approval for a
/// step that is awaiting one in the instance's current iteration.
pub(crate) fn workflow_approve(
    store: &JobStore,
    instance: &str,
    body: Option<Value>,
) -> Result<Value> {
    let step = body
        .as_ref()
        .and_then(|body| body.get("step"))
        .and_then(Value::as_str)
        .filter(|step| !step.trim().is_empty())
        .ok_or_else(|| invalid("an approval names its `step`"))?
        .to_string();
    let (job, controller) = instance_job(store, instance)?;
    if job.status.is_terminal() {
        return Err(invalid(
            format!("workflow `{instance}` is already {:?}", job.status).to_lowercase(),
        ));
    }
    let state: InstanceState = controller
        .checkpoint
        .as_ref()
        .and_then(|checkpoint| serde_json::from_value(checkpoint.clone()).ok())
        .ok_or_else(|| invalid(format!("workflow `{instance}` has not started")))?;
    let awaiting = state
        .steps
        .get(&step)
        .is_some_and(|candidate| candidate.phase == StepPhase::AwaitingApproval);
    if !awaiting {
        return Err(Error::validation_invalid_argument(
            "step",
            format!("step `{step}` of `{instance}` is not awaiting approval"),
            Some(step),
            None,
        ));
    }
    let approval = json!({
        "step": step,
        "iteration": state.iteration,
        "approved_at": chrono::Utc::now().to_rfc3339(),
    });
    store.append_event(
        job.id,
        JobEventKind::Progress,
        Some(format!("workflow step `{step}` approved")),
        Some(json!({ "workflow_approval": approval })),
    )?;
    Ok(json!({ "instance": instance, "approval": approval }))
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

fn to_value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize workflow job".to_string()),
        )
    })
}

fn invalid(message: impl Into<String>) -> Error {
    Error::validation_invalid_argument("workflow", message, None, None)
}

#[cfg(all(test, unix))]
#[path = "workflow_job_tests.rs"]
mod tests;

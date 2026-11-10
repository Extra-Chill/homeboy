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
/// The step environment variable that names the instance whose step is
/// running. `workflow run` reads it and records the admitting instance as the
/// admitted instance's `parent`, so a self-admitting chain stays visible.
pub const WORKFLOW_INSTANCE_ENV: &str = "HOMEBOY_WORKFLOW_INSTANCE";
const OUTPUT_FILE: &str = "output.json";
/// Outputs larger than this are not carried into the instance state.
const MAX_OUTPUT_BYTES: u64 = 256 * 1024;
const POLL: Duration = Duration::from_millis(500);

fn step_terminal_consumer() -> &'static RwLock<Option<String>> {
    static CONSUMER: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    CONSUMER.get_or_init(|| RwLock::new(None))
}

/// Domain consumer selected for linked workflow steps, frozen into each request.
pub fn register_workflow_step_terminal_consumer(id: &str) {
    *step_terminal_consumer()
        .write()
        .expect("workflow terminal consumer") = Some(id.to_string());
}

/// The admitted, immutable request for one workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowJobRequest {
    pub schema: String,
    /// `wf-<definition-id>-<hash>`; also the job's idempotency key.
    pub instance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    /// Explicit source of reusable succeeded steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    /// The definition, pinned at admission.
    pub definition: WorkflowDefinition,
    pub definition_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_source: Option<String>,
    pub inputs: BTreeMap<String, Value>,
    /// Absolute path of the Homeboy executable `homeboy` and `cook` steps run.
    pub homeboy_program: String,
    /// Absolute working directory of every step without its own `cwd`.
    pub cwd: String,
    /// The instance whose step admitted this one, when `workflow run` ran
    /// inside a step that carried `HOMEBOY_WORKFLOW_INSTANCE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The workflow's declared environment: public values and secret refs.
    #[serde(default)]
    pub env: EnvMaterializationPlan,
    /// Homeboy's own admitted runtime environment for `homeboy`/`cook` steps.
    #[serde(default)]
    pub homeboy_env: EnvMaterializationPlan,
    /// `exec` and command-gate programs resolved to absolute paths at admission.
    #[serde(default)]
    pub programs: BTreeMap<String, String>,
    /// Resolved at admission, independent of the daemon's environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_route: Option<crate::notification_route::NotificationRoute>,
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
        if let Some(route) = &self.notification_route {
            route.validate()?;
        }
        // Cook flags were checked against the CLI's clap tree at admission.
        self.definition.validate(&|_| true)?;
        if self.definition.digest() != self.definition_digest {
            return Err(invalid(
                "workflow definition digest does not match its body",
            ));
        }
        if self.definition.instance_id_with_resume(
            &self.inputs,
            self.instance_name.as_deref(),
            self.resumed_from.as_deref(),
        ) != self.instance
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
        if self
            .parent
            .as_ref()
            .is_some_and(|parent| parent.trim().is_empty())
        {
            return Err(invalid("workflow `parent` must be a non-empty instance id"));
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

/// What a Cook step's durable run says about its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowRunOutcome {
    /// No durable record exists. A Cook that never recorded its run did not
    /// cook, so a clean step exit settles it as failed; a failed or cancelled
    /// step job keeps its own status.
    Unknown,
    /// The Cook has not settled: its run, or the terminal continuation that
    /// promotes and finalizes it, is still in progress.
    Pending,
    /// The Cook settled with this run state, and the terminal reason its run
    /// recorded when it did not succeed.
    Settled {
        outcome: String,
        reason: Option<String>,
    },
}

/// Resolves a Cook step's durable outcome from its run record. Registered by
/// the agent-task layer.
pub type WorkflowRunOutcomeResolver = Arc<dyn Fn(&str) -> WorkflowRunOutcome + Send + Sync>;

fn run_outcome_resolver() -> &'static RwLock<Option<WorkflowRunOutcomeResolver>> {
    static RESOLVER: OnceLock<RwLock<Option<WorkflowRunOutcomeResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| RwLock::new(None))
}

pub fn register_workflow_run_outcome_resolver(resolver: WorkflowRunOutcomeResolver) {
    *run_outcome_resolver()
        .write()
        .expect("workflow run outcome resolver lock") = Some(resolver);
}

/// Reports a Cook step's live progress (its latest attempt's state and phase)
/// for status projections. Registered by the agent-task layer.
pub type WorkflowRunProgressResolver = Arc<dyn Fn(&str) -> Option<Value> + Send + Sync>;

fn run_progress_resolver() -> &'static RwLock<Option<WorkflowRunProgressResolver>> {
    static RESOLVER: OnceLock<RwLock<Option<WorkflowRunProgressResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| RwLock::new(None))
}

pub fn register_workflow_run_progress_resolver(resolver: WorkflowRunProgressResolver) {
    *run_progress_resolver()
        .write()
        .expect("workflow run progress resolver lock") = Some(resolver);
}

fn run_progress(run_id: Option<&str>) -> Value {
    run_id
        .and_then(|run_id| {
            run_progress_resolver()
                .read()
                .expect("workflow run progress resolver lock")
                .as_ref()
                .and_then(|resolve| resolve(run_id))
        })
        .unwrap_or(Value::Null)
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
            "parent": request.parent,
            "resumed_from": request.resumed_from,
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
                "diagnostic": controller_job_driver::public_io_diagnostic(error),
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

/// Compare the complete templated step, not just its executable arguments.
fn step_digest(
    step: &crate::workflow::definition::StepDefinition,
    state: &InstanceState,
) -> Option<String> {
    fn render(value: &mut Value, state: &InstanceState) -> Option<()> {
        match value {
            Value::String(text) => {
                // Item templates are identical code applied to the identical
                // foreach expansion; resolve the other references normally.
                let mut rendered = String::new();
                let mut rest = text.as_str();
                while let Some(start) = rest.find("{{") {
                    rendered.push_str(&rest[..start]);
                    let end = start + rest[start..].find("}}")? + 2;
                    let reference = &rest[start..end];
                    let parsed = crate::workflow::template::references(reference).ok()?;
                    if matches!(
                        parsed.as_slice(),
                        [crate::workflow::template::Reference::Item(_)]
                    ) {
                        rendered.push_str(reference);
                    } else {
                        rendered
                            .push_str(&crate::workflow::template::render(reference, state).ok()?);
                    }
                    rest = &rest[end..];
                }
                rendered.push_str(rest);
                *text = rendered;
            }
            Value::Array(items) => {
                for item in items {
                    render(item, state)?;
                }
            }
            Value::Object(fields) => {
                for value in fields.values_mut() {
                    render(value, state)?;
                }
            }
            _ => {}
        }
        Some(())
    }
    let mut value = serde_json::to_value(step).ok()?;
    render(&mut value, state)?;
    Some(homeboy_engine_primitives::content_hash::sha256_hex(
        serde_json::to_string(&value).ok()?.as_bytes(),
    ))
}

fn reuse_steps(
    request: &WorkflowJobRequest,
    old: &WorkflowJobRequest,
    source: &InstanceState,
    target: &mut InstanceState,
) -> Result<()> {
    if old.definition.id != request.definition.id {
        return Err(invalid("resume source belongs to a different workflow"));
    }
    // Definitions need not be topologically ordered. Iterate to a fixed point.
    loop {
        let mut changed = false;
        for step in &request.definition.steps {
            if target.steps[&step.id].reused || !step.needs.iter().all(|id| target.steps[id].reused)
            {
                continue;
            }
            let Some(recorded) = source.steps.get(&step.id).filter(|state| {
                state.phase == StepPhase::Done && state.outcome.as_deref() == Some("succeeded")
            }) else {
                continue;
            };
            let Some(previous) = old.definition.steps.iter().find(|old| old.id == step.id) else {
                continue;
            };
            let Some(digest) = step_digest(step, target) else {
                continue;
            };
            if step_digest(previous, source).as_ref() != Some(&digest) {
                continue;
            }
            // Keep output, run ids, item records and original job keys: those
            // keys are the durable references to the source's artifacts.
            let mut reused = recorded.clone();
            reused.reused = true;
            target.steps.insert(step.id.clone(), reused);
            changed = true;
        }
        if !changed {
            return Ok(());
        }
    }
}

fn run_instance(
    request: &WorkflowJobRequest,
    mut state: InstanceState,
    job: &ControllerJobHandle,
    store: &JobStore,
) -> Result<Value> {
    let job_id = job.job().job_id();
    if state.started_at_ms.is_none() {
        if let Some(source) = &request.resumed_from {
            let (source_job, controller) = instance_job(store, source)?;
            if !source_job.status.is_terminal() {
                return Err(invalid("resume source must be terminal"));
            }
            let old_request = WorkflowJobRequest::parse(&controller.request)?;
            let old_state: InstanceState = serde_json::from_value(
                controller
                    .checkpoint
                    .ok_or_else(|| invalid("resume source has no checkpoint"))?,
            )
            .map_err(|error| invalid(format!("invalid resume checkpoint: {error}")))?;
            reuse_steps(request, &old_request, &old_state, &mut state)?;
        }
    }
    // Repair a projection interrupted after the previous checkpoint commit.
    if let Err(error) = project_timing(&state) {
        crate::log_status!("observe", "workflow timing persistence failed: {}", error);
    }
    job.progress(projection(&state, Some(store)))?;
    loop {
        if job.is_cancelled() {
            return Ok(cancelled(request, &state, job_id, store));
        }
        if state.status.is_terminal() {
            return finish(request, &state, job_id, store);
        }
        // Leave the checkpoint and child ledger untouched for driver recovery.
        // Protected children finish independently; advancing here would keep a
        // continuous workflow's old daemon alive indefinitely after an upgrade.
        if super::intake_draining() {
            job.yield_for_recovery(to_value(&state)?)?;
            return Ok(Value::Null);
        }
        super::workflow_notify::awaiting_approvals(request, &state);
        let now_ms = now_ms();
        for event in observe(request, &state, job_id, store) {
            if super::intake_draining() {
                job.yield_for_recovery(to_value(&state)?)?;
                return Ok(Value::Null);
            }
            let (next, commands) = advance(&request.definition, &state, &event, now_ms);
            if next == state && commands.is_empty() {
                continue;
            }
            match commit(request, job_id, &next, &commands, store) {
                Ok(()) => {}
                Err(_) if job.is_cancelled() => {
                    return Ok(cancelled(request, &state, job_id, store));
                }
                Err(error) => return Err(error),
            }
            state = next;
            super::workflow_notify::awaiting_approvals(request, &state);
            job.progress(projection(&state, Some(store)))?;
            if state.status.is_terminal() {
                break;
            }
        }
        if super::intake_draining() {
            job.yield_for_recovery(to_value(&state)?)?;
            return Ok(Value::Null);
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

/// The instance was cancelled: stop every step job it still awaits (the
/// daemon's cascade usually got there first) and report the final state.
fn cancelled(
    request: &WorkflowJobRequest,
    state: &InstanceState,
    job_id: Uuid,
    store: &JobStore,
) -> Value {
    let (next, commands) = advance(
        &request.definition,
        state,
        &Event::Cancel { reason: None },
        now_ms(),
    );
    // Cancellation admits no new work, but its terminal timing still commits.
    let persisted =
        to_value(&next).and_then(|value| store.record_controller_prepared(job_id, value));
    match persisted {
        Ok(()) => {
            let _ = project_timing(&next);
        }
        Err(error) => crate::log_status!(
            "observe",
            "workflow cancellation checkpoint failed: {}",
            error
        ),
    }
    for command in commands {
        if let Command::CancelStep { key, reason } = command {
            if let Some(child) = store.controller_job_for_key(&key) {
                if !child.status.is_terminal() {
                    let _ = crate::daemon::cancel_controller_job(child.id, &reason, store);
                }
            }
        }
    }
    projection(&next, Some(store))
}

fn finish(
    request: &WorkflowJobRequest,
    state: &InstanceState,
    job_id: Uuid,
    store: &JobStore,
) -> Result<Value> {
    let mut state = state.clone();
    if state.status == InstanceStatus::Succeeded {
        let admission = (|| -> Result<()> {
            if let Some(child) = next_pass(request, &state)? {
                let child = match child {
                    NextPass::Admit(child) => child,
                    NextPass::Stop(reason) => {
                        state.reason = Some(format!("chain ended: {reason}"));
                        store.record_controller_prepared(job_id, to_value(&state)?)?;
                        return Ok(());
                    }
                };
                let envelope = serde_json::from_value(child.submission()?)
                    .map_err(|error| invalid(format!("invalid next pass: {error}")))?;
                let admission = super::with_daemon_job_admission(|| {
                    super::admit_controller_job(envelope, store)
                })?;
                super::start_controller_job(admission.job.id, store)?;
            }
            Ok(())
        })();
        if let Err(error) = admission {
            state.status = InstanceStatus::Failed;
            state.reason = Some(format!("next_pass not admitted: {error}"));
            // Persist the failure before returning it: status views and replay
            // must not retain the successful step-completion checkpoint.
            store.record_controller_prepared(job_id, to_value(&state)?)?;
            if let Err(error) = project_timing(&state) {
                crate::log_status!("observe", "workflow timing persistence failed: {}", error);
            }
        }
    }
    let result = projection(&state, Some(store));
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

/// A configured chain either admits its next pass or ends for a recorded
/// reason, so a finished chain is distinguishable from a stalled one.
enum NextPass {
    Admit(Box<WorkflowJobRequest>),
    Stop(String),
}

/// Build a replay-stable child from the admitted (pinned) definition.
fn next_pass(request: &WorkflowJobRequest, state: &InstanceState) -> Result<Option<NextPass>> {
    let Some(next) = &request.definition.next_pass else {
        return Ok(None);
    };
    if let Some(path) = &request.definition_source {
        let text = std::fs::read_to_string(path)
            .map_err(|e| invalid(format!("cannot read next-pass definition {path}: {e}")))?;
        let definition = WorkflowDefinition::parse(&text)?;
        definition.validate(&|_| true)?;
        if definition.digest() != request.definition_digest {
            return Err(invalid(format!("definition changed between passes: {path}; restart the workflow to use the new definition")));
        }
    }
    let iteration = request
        .inputs
        .get("iteration")
        .and_then(Value::as_u64)
        .filter(|n| *n >= 1 && *n <= u64::from(u32::MAX))
        .ok_or_else(|| invalid("next_pass requires a positive integer iteration"))?;
    if iteration >= u64::from(next.max_iterations) {
        return Ok(Some(NextPass::Stop(format!(
            "max_iterations {} reached",
            next.max_iterations
        ))));
    }
    if next.when.as_ref().is_some_and(|gate| !gate.evaluate(state)) {
        return Ok(Some(NextPass::Stop(
            "next_pass.when gate did not pass".into(),
        )));
    }
    let mut streak = 1;
    if let Some(stop) = &next.stop_unchanged {
        let current = state
            .steps
            .get(&stop.step)
            .and_then(|s| s.output.as_ref())
            .and_then(|v| v.pointer(&stop.output))
            .ok_or_else(|| invalid("next_pass.stop_unchanged output is missing"))?;
        let previous = &request.inputs["previous"];
        let old = previous
            .get("outputs")
            .and_then(|v| v.get(&stop.step))
            .and_then(|v| v.pointer(&stop.output));
        if old == Some(current) {
            streak = previous
                .get("unchanged_passes")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                + 1;
        }
        if streak >= u64::from(stop.passes) {
            return Ok(Some(NextPass::Stop(format!(
                "step `{}` output `{}` unchanged for {streak} passes",
                stop.step, stop.output
            ))));
        }
    }
    fn render(value: &Value, state: &InstanceState) -> Result<Value> {
        Ok(match value {
            Value::String(text) => {
                crate::workflow::template::resolve_value(text, state).map_err(invalid)?
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|v| render(v, state))
                    .collect::<Result<_>>()?,
            ),
            Value::Object(items) => Value::Object(
                items
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), render(v, state)?)))
                    .collect::<Result<_>>()?,
            ),
            other => other.clone(),
        })
    }
    let mut child = request.clone();
    for (name, value) in &next.inputs {
        child.inputs.insert(name.clone(), render(value, state)?);
    }
    child
        .inputs
        .insert("iteration".into(), json!(iteration + 1));
    let outputs: BTreeMap<_, _> = state
        .steps
        .iter()
        .filter_map(|(id, step)| step.output.as_ref().map(|v| (id, v)))
        .collect();
    child.inputs.insert(
        "previous".into(),
        json!({"outputs": outputs, "unchanged_passes": streak}),
    );
    child.parent = Some(request.instance.clone());
    child.resumed_from = None;
    child.instance = child
        .definition
        .instance_id(&child.inputs, child.instance_name.as_deref());
    Ok(Some(NextPass::Admit(Box::new(child))))
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
                let rejected = approval["decision"] == "rejected";
                events.push(Event::ApprovalDecided {
                    step: step.to_string(),
                    iteration: state.iteration,
                    rejected,
                    output: json!({
                        "decision": if rejected { "rejected" } else { "approved" },
                        "reason": approval["reason"],
                        "actor": approval["actor"],
                    }),
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
    let owner = state.owner_of(key);
    let kind = owner.map(|(id, _)| {
        request
            .definition
            .steps
            .iter()
            .find(|definition| definition.id == id)
            .map(|definition| definition.kind())
            .expect("state steps match the pinned definition")
    });
    let run_id = owner.and_then(|(id, index)| {
        let step = &state.steps[id];
        match index {
            Some(index) => step.items[index].run_id.clone(),
            None => step.run_id.clone(),
        }
    });
    let output_path = output_path(&request.instance, key).ok();
    // A settled Cook's own terminal reason, which outranks its waiter's exit.
    let mut cook_reason = None;
    let (outcome, output) = match kind {
        Some(crate::workflow::definition::StepKind::Cook(_)) => {
            let resolved = run_id.as_deref().and_then(|run_id| {
                run_outcome_resolver()
                    .read()
                    .expect("workflow run outcome resolver lock")
                    .as_ref()
                    .map(|resolve| resolve(run_id))
            });
            if let (Some(WorkflowRunOutcome::Settled { outcome, reason }), Some(cook_id)) =
                (&resolved, run_id.as_deref())
            {
                cook_reason = (outcome != "succeeded")
                    .then(|| cook_terminal_error(cook_id, outcome, reason.as_deref()));
            }
            let outcome = cook_step_outcome(resolved, status)?;
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
    let error = if outcome.as_deref() == Some("succeeded") {
        None
    } else if cook_reason.is_some() {
        cook_reason
    } else if status == FinishedStatus::Failed {
        self::output_path(&request.instance, key)
            .ok()
            .and_then(|path| read_output(&path))
            .and_then(|envelope| {
                envelope
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| {
                store.events(job.id).ok().and_then(|events| {
                    events
                        .into_iter()
                        .rev()
                        .find(|event| {
                            event.kind == JobEventKind::Error
                                && event
                                    .message
                                    .as_deref()
                                    .is_some_and(|message| !message.trim().is_empty())
                        })
                        .and_then(|event| event.message)
                })
            })
    } else {
        None
    };
    Some(StepResult {
        key: key.to_string(),
        error,
        status,
        outcome,
        output,
        run_id,
    })
}

/// A Cook step's outcome once its step job is terminal: `None` while the Cook
/// has not settled, otherwise the outcome that overrides the job status.
///
/// The Cook process ending is only a wake-up: the Cook settles when its run
/// (and its terminal continuation) does. A step that exited cleanly without
/// ever recording a run did not cook, so it fails rather than inheriting the
/// exit status.
fn cook_step_outcome(
    resolved: Option<WorkflowRunOutcome>,
    status: FinishedStatus,
) -> Option<Option<String>> {
    match resolved {
        Some(WorkflowRunOutcome::Pending) => None,
        Some(WorkflowRunOutcome::Settled { outcome, .. }) => Some(Some(outcome)),
        Some(WorkflowRunOutcome::Unknown) if status == FinishedStatus::Succeeded => {
            Some(Some("failed".to_string()))
        }
        Some(WorkflowRunOutcome::Unknown) | None => Some(None),
    }
}

/// A settled, unsuccessful Cook's item error: the reason its run recorded, or
/// its terminal state, naming the Cook id that `agent-task diagnose` reads.
fn cook_terminal_error(cook_id: &str, outcome: &str, reason: Option<&str>) -> String {
    match reason.map(str::trim).filter(|reason| !reason.is_empty()) {
        Some(reason) => format!("Cook `{cook_id}` ended `{outcome}`: {reason}"),
        None => format!("Cook `{cook_id}` ended `{outcome}`"),
    }
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
    // Projection follows the checkpoint commit; replay uses stable attempt identities.
    if let Err(error) = project_timing(next) {
        crate::log_status!("observe", "workflow timing persistence failed: {}", error);
    }
    for child in admitted {
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

fn project_timing(state: &InstanceState) -> Result<()> {
    crate::observation::ObservationStore::open_initialized()?.record_workflow_timing(state)
}

impl crate::observation::ObservationStore {
    /// Replay completed attempts only after their checkpoint has committed.
    pub fn record_workflow_timing(&self, state: &InstanceState) -> Result<()> {
        use crate::observation::{NewRunRecord, RunStatus};
        let store = self;
        if store.get_run(&state.instance)?.is_none() {
            store.start_run_with_id(
                NewRunRecord::builder("workflow").build(),
                state.instance.clone(),
            )?;
        }
        for (index, span) in state.timing.completed.spans.iter().enumerate() {
            store.record_phase_spans(
                &state.instance,
                &format!("workflow-attempt:{index}"),
                &homeboy_engine_primitives::phase_timing::PhaseTimingReport {
                    spans: vec![span.clone()],
                },
            )?;
        }
        if state.status.is_terminal() {
            store.finish_run(
                &state.instance,
                if state.status == InstanceStatus::Succeeded {
                    RunStatus::Pass
                } else {
                    RunStatus::Fail
                },
                None,
            )?;
        }
        Ok(())
    }
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
    let route_env = || {
        crate::notification_route::child_env(request.notification_route.as_ref())
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
    };
    let homeboy_env = || {
        let mut env = request.homeboy_env.clone();
        env.public_env.extend(request.env.public_env.clone());
        env.secret_refs.extend(request.env.secret_refs.clone());
        env.public_env.extend(route_env());
        env.public_env
            .insert(WORKFLOW_INSTANCE_ENV.to_string(), request.instance.clone());
        env.normalize();
        env
    };
    let exec_env = || {
        let mut env = request.env.clone();
        env.public_env.extend(route_env());
        env.public_env
            .insert(STEP_OUTPUT_ENV.to_string(), output.clone());
        env.public_env
            .insert(WORKFLOW_INSTANCE_ENV.to_string(), request.instance.clone());
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
    };
    let consumer = if linked_run.is_some() {
        step_terminal_consumer()
            .read()
            .expect("workflow terminal consumer")
            .clone()
            .ok_or_else(|| invalid("linked workflow step requires a terminal consumer"))?
    } else {
        super::step_job::STEP_RECEIPT_CONSUMER.to_string()
    };
    let mut step = StepJobRequest::new(submission.key.clone(), program, args, cwd, env, consumer);
    step.root_durable_run_id = Some(request.instance.clone());
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
                "reused": step.reused,
                "launched": !step.reused && step.attempt > 0,
                "attempt": step.attempt,
                "outcome": step.outcome,
                "output": step.output,
                "key": step.key,
                "job": step.key.as_deref().map(job_of).unwrap_or(Value::Null),
                "run_id": step.run_id,
                "run": if step.phase == StepPhase::Running { run_progress(step.run_id.as_deref()) } else { Value::Null },
                "items": step.items.iter().enumerate().map(|(index, item)| json!({
                    "index": index,
                    "item": item.item,
                    "phase": item.phase,
                    "attempt": item.attempt,
                    "outcome": item.outcome,
                    "key": item.key,
                    "job": item.key.as_deref().map(job_of).unwrap_or(Value::Null),
                    "run_id": item.run_id,
                    "run": if item.phase == StepPhase::Running { run_progress(item.run_id.as_deref()) } else { Value::Null },
                    "error": item.error,
                })).collect::<Vec<_>>(),
                "prompt": step.prompt,
                "error": step.error,
            }))
        }).collect::<serde_json::Map<_, _>>(),
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
    if job.status.is_terminal() {
        let expires_at_ms = state.request["expired_at_ms"].as_u64().unwrap_or_else(|| {
            job.finished_at_ms
                .unwrap_or(job.updated_at_ms)
                .saturating_add(
                    crate::defaults::load_config()
                        .retention
                        .workflow_instance_days
                        .saturating_mul(86_400_000),
                )
        });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if now >= expires_at_ms {
            return Err(invalid(format!(
                "workflow instance `{instance}` expired at {expires_at_ms} (Unix milliseconds)"
            )));
        }
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
        view["resumed_from"] = json!(request.resumed_from);
        view["definition_digest"] = json!(request.definition_digest);
        view["inputs"] = json!(request.inputs);
        view["parent"] = json!(request.parent);
        view["children"] = json!(child_instances(store, &request.instance));
    }
    view
}

/// The instances `instance` admitted, oldest first: the child links of a
/// self-admitting chain.
fn child_instances(store: &JobStore, instance: &str) -> Vec<String> {
    let mut children = store
        .controller_jobs_of_type(WORKFLOW_JOB_TYPE)
        .iter()
        .filter_map(|(job, controller)| {
            let request = WorkflowJobRequest::parse(&controller.request).ok()?;
            (request.parent.as_deref() == Some(instance))
                .then(|| (job.created_at_ms, job.id, request.instance))
        })
        .collect::<Vec<_>>();
    children.sort();
    children
        .into_iter()
        .map(|(_, _, instance)| instance)
        .collect()
}

/// `GET /workflows/<instance>`.
pub(crate) fn workflow_status(store: &JobStore, instance: &str) -> Result<Value> {
    let (job, controller) = instance_job(store, instance)?;
    Ok(instance_view(&job, &controller, store))
}

/// The instance a watcher moves to next: the last child of a terminal
/// instance. `workflow run --wait` and `runs watch <instance>` follow these
/// links, so watching a self-admitting chain reports its last instance's
/// terminal status.
pub fn chain_successor(view: &Value) -> Option<String> {
    if !matches!(
        view["status"].as_str(),
        Some("succeeded" | "failed" | "cancelled" | "budget_exhausted")
    ) {
        return None;
    }
    view["children"]
        .as_array()?
        .last()?
        .as_str()
        .map(str::to_string)
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
                "parent": view["parent"],
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
    let decision = body
        .as_ref()
        .and_then(|body| body.get("decision"))
        .and_then(Value::as_str)
        .unwrap_or("approved");
    if !matches!(decision, "approved" | "rejected") {
        return Err(invalid("decision must be approved or rejected"));
    }
    let reason = body.as_ref().and_then(|body| body.get("reason"));
    if reason.is_some_and(|value| !value.is_null() && !value.is_string()) {
        return Err(invalid("reason must be a string"));
    }
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
        "decision": decision,
        "reason": reason,
        "actor": body.as_ref().and_then(|body| body.get("actor"))
            .and_then(Value::as_str).unwrap_or("local-operator"),
    });
    store.append_event(
        job.id,
        JobEventKind::Progress,
        Some(format!("workflow step `{step}` {decision}")),
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

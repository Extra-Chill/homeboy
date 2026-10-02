//! Shared-work supervision for one detached loop-controller coordinator.

use std::time::Duration;

use homeboy_core::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::work_job::{
    register_work_job_handler, work_job_submission, WorkJobHandler, WorkJobInvocation,
    WorkJobPhase, WorkJobStep,
};
use crate::agent_task_controller_service::{ControllerDispatchHook, ControllerDispatchOverrides};
use crate::agent_task_dispatch_service;
use crate::agent_task_loop_controller::{self, AgentTaskLoopControllerState};
use crate::agent_task_provider::{AgentTaskProviderCatalog, ExtensionProviderAgentTaskExecutor};
use crate::agent_task_scheduler::SharedAgentTaskExecutor;

pub const AGENT_TASK_LOOP_JOB_TYPE: &str = "agent-task-loop";
pub const AGENT_TASK_LOOP_JOB_VERSION: u32 = 2;
const AGENT_TASK_LOOP_JOB_SCHEMA: &str = "homeboy/agent-task-loop-job/v2";
const SUPERVISION_POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLoopJobKind {
    DaemonExecution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskLoopJobRequest {
    pub schema: String,
    pub loop_id: String,
    pub kind: AgentTaskLoopJobKind,
    pub generation: String,
    #[serde(default)]
    pub dispatch_defaults: Value,
    /// The caller's admitted provider catalog. The daemon must not rediscover
    /// providers from its own environment while executing this job.
    #[serde(default)]
    pub provider_catalog: AgentTaskProviderCatalog,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskLoopJob {
    pub schema: String,
    pub idempotency_key: String,
    pub request: AgentTaskLoopJobRequest,
    #[serde(default)]
    pub phase: WorkJobPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_state: Option<AgentTaskLoopControllerState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_result: Option<Value>,
}

impl AgentTaskLoopJob {
    fn new(request: AgentTaskLoopJobRequest) -> Result<Self> {
        if request.schema != AGENT_TASK_LOOP_JOB_SCHEMA || request.loop_id.trim().is_empty() {
            return Err(invalid_loop_job(
                "loop jobs require a recognized schema and durable loop id",
            ));
        }
        if request.generation.trim().is_empty() {
            return Err(invalid_loop_job(
                "daemon loop executions require a non-empty generation",
            ));
        }
        Ok(Self {
            schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
            idempotency_key: format!("agent-task-loop:{}:{}", request.loop_id, request.generation),
            request,
            phase: WorkJobPhase::Queued,
            controller_state: None,
            resume_result: None,
        })
    }

    fn parse(value: Value) -> Result<Self> {
        let job: Self = serde_json::from_value(value)
            .map_err(|error| invalid_loop_job(&format!("invalid durable loop job: {error}")))?;
        let expected = Self::new(job.request.clone())?;
        if job.schema != AGENT_TASK_LOOP_JOB_SCHEMA
            || job.idempotency_key != expected.idempotency_key
        {
            return Err(invalid_loop_job(
                "loop job identity does not match its immutable request",
            ));
        }
        if job.phase == WorkJobPhase::Completed
            && !job
                .controller_state
                .is_some_and(controller_state_is_terminal)
        {
            return Err(invalid_loop_job(
                "completed loop jobs require a terminal controller state",
            ));
        }
        Ok(job)
    }

    fn to_checkpoint(&self) -> Result<Value> {
        serde_json::to_value(self).map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("serialize loop work checkpoint".to_string()),
            )
        })
    }

    fn public_projection(&self) -> Value {
        json!({
            "schema": self.schema,
            "idempotency_key": self.idempotency_key,
            "phase": self.phase,
            "loop_id": self.request.loop_id,
            "kind": self.request.kind,
            "controller_state": self.controller_state,
            "generation": self.request.generation,
        })
    }

    fn result(&self) -> Value {
        json!({
            "phase": self.phase,
            "loop_id": self.request.loop_id,
            "controller_state": self.controller_state,
            "resume": self.resume_result,
        })
    }

    fn refresh_controller_state(&mut self) -> bool {
        // The work supervision reads the durable record without side effects.
        // Waiting advancement is the explicit reconcile in `LoopWorkHandler::
        // advance_waiting`, never a hidden refresh inside a status-shaped read.
        let Ok(record) = agent_task_loop_controller::load_controller(&self.request.loop_id) else {
            return false;
        };
        let changed = self.controller_state != Some(record.state);
        self.controller_state = Some(record.state);
        changed
    }
}

struct LoopWorkHandler;

impl WorkJobHandler for LoopWorkHandler {
    fn execution_owner(
        &self,
        state: &Value,
    ) -> Result<homeboy_core::daemon::controller_job_driver::ControllerJobExecutionOwner> {
        use homeboy_core::daemon::controller_job_driver::ControllerJobExecutionOwner as Owner;
        let job = AgentTaskLoopJob::parse(state.clone())?;
        if job.phase == WorkJobPhase::Completed {
            return Ok(Owner::None);
        }
        loop_execution_owner(&job.request.loop_id, &job.request.generation)
    }
    fn linked_durable_run_id(&self, request: &Value) -> Option<String> {
        request["request"]["loop_id"]
            .as_str()
            .or_else(|| request["loop_id"].as_str())
            .map(|id| format!("loop-command:{id}"))
    }
    fn work_type(&self) -> &'static str {
        AGENT_TASK_LOOP_JOB_TYPE
    }

    fn version(&self) -> u32 {
        AGENT_TASK_LOOP_JOB_VERSION
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        Ok(AgentTaskLoopJob::parse(request.clone())?.public_projection())
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        Ok(json!({
            "phase": progress.get("phase").cloned().unwrap_or(Value::Null),
            "loop_id": progress.get("loop_id").cloned().unwrap_or(Value::Null),
            "controller_state": progress.get("controller_state").cloned().unwrap_or(Value::Null),
        }))
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        self.public_progress(result)
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        AgentTaskLoopJob::parse(request.clone()).map(|_| ())
    }

    fn prepare(&self, request: Value) -> Result<Value> {
        let mut job = AgentTaskLoopJob::parse(request)?;
        if job.phase != WorkJobPhase::Queued || job.controller_state.is_some() {
            return Err(invalid_loop_job(
                "new loop jobs must start queued without controller state",
            ));
        }
        job.phase = WorkJobPhase::Supervising;
        job.refresh_controller_state();
        job.to_checkpoint()
    }

    fn initial_progress(&self, checkpoint: &Value) -> Result<Value> {
        Ok(AgentTaskLoopJob::parse(checkpoint.clone())?.result())
    }

    fn terminal_result(&self, checkpoint: &Value) -> Result<Option<Value>> {
        let job = AgentTaskLoopJob::parse(checkpoint.clone())?;
        job.phase
            .eq(&WorkJobPhase::Completed)
            .then(|| Ok(job.result()))
            .transpose()
    }

    fn advance(&self, checkpoint: Value, invocation: WorkJobInvocation) -> Result<WorkJobStep> {
        let mut job = AgentTaskLoopJob::parse(checkpoint)?;
        if job.phase == WorkJobPhase::Completed {
            return Ok(WorkJobStep::Complete(job.result()));
        }
        self.observe(&mut job, invocation)
    }

    fn cancelled(&self, checkpoint: Value) -> Result<Value> {
        let mut job = AgentTaskLoopJob::parse(checkpoint)?;
        terminalize_interrupted(&mut job, AgentTaskLoopControllerState::Abandoned)
    }

    fn cancel(&self, checkpoint: &Value) -> Result<()> {
        let job = AgentTaskLoopJob::parse(checkpoint.clone())?;
        if job.phase != WorkJobPhase::Completed
            && !controller_state_is_terminal(
                agent_task_loop_controller::load_controller(&job.request.loop_id)?.state,
            )
        {
            agent_task_loop_controller::cancel_owned_provider_runs(
                &job.request.loop_id,
                "controller work job cancelled",
            )?;
        }
        Ok(())
    }
}

impl LoopWorkHandler {
    /// Advance a `Waiting` loop from durable evidence, on the supervision
    /// cadence. This is the one automatic driver `Waiting` has: it reuses the
    /// controller service's single wait-reconcile primitive — durable
    /// child/run terminal evidence satisfies a wait, a declared `timeout_at`
    /// expires it, and a controller with nothing left to wait for becomes
    /// runnable again — so child terminalization and deadline expiry advance
    /// the loop without any CLI status, resume, or apply-event side effect.
    fn advance_waiting(&self, job: &mut AgentTaskLoopJob) -> Result<()> {
        if job.controller_state != Some(AgentTaskLoopControllerState::Waiting) {
            return Ok(());
        }
        let mut record = agent_task_loop_controller::load_controller(&job.request.loop_id)?;
        let before_state = record.state;
        let outcome = crate::agent_task_controller_service::reconcile_open_waits(&mut record)?;
        if !outcome.changed() {
            return Ok(());
        }
        record.touch();
        agent_task_loop_controller::write_controller(&record)?;
        crate::agent_task_controller_service::emit_wait_reconcile_notifications(
            &record,
            before_state,
            &outcome,
        );
        job.refresh_controller_state();
        Ok(())
    }

    /// A `Waiting` controller with no open wait and no open action is parked:
    /// nothing durable will ever wake it, so supervision completes instead of
    /// polling forever.
    fn waiting_is_idle(&self, job: &AgentTaskLoopJob) -> bool {
        agent_task_loop_controller::load_controller(&job.request.loop_id)
            .is_ok_and(|record| controller_is_waiting_idle(&record))
    }

    fn observe(
        &self,
        job: &mut AgentTaskLoopJob,
        invocation: WorkJobInvocation,
    ) -> Result<WorkJobStep> {
        job.phase = WorkJobPhase::Supervising;
        job.refresh_controller_state();
        if invocation == WorkJobInvocation::Resume {
            let record = agent_task_loop_controller::load_controller(&job.request.loop_id)?;
            if record.metadata["command_recovery"]["state"] != "reaped"
                && guarded_command_execution_owner(&record.metadata["command_recovery"]).is_some()
            {
                let owner = loop_execution_owner(&job.request.loop_id, &job.request.generation)?;
                if matches!(
                    owner.inspect(),
                    homeboy_core::process::ProcessIdentityState::Live
                        | homeboy_core::process::ProcessIdentityState::Unverifiable
                ) {
                    return Ok(WorkJobStep::Continue {
                        checkpoint: job.to_checkpoint()?,
                        progress: job.result(),
                        wait: SUPERVISION_POLL,
                    });
                }
                if let Some(record) =
                    crate::api_jobs_terminal_recovery::recovered_loop_command_record(
                        &job.request.loop_id,
                    )
                {
                    agent_task_loop_controller::write_controller(&record)?;
                    job.controller_state = Some(record.state);
                    job.resume_result = Some(json!({
                        "recovery": "unknown", "reason": "command owner exited before durable completion; command is not redispatched"
                    }));
                    job.phase = WorkJobPhase::Completed;
                    return Err(homeboy_core::Error::internal_unexpected(
                        "guarded command owner exited before durable completion; outcome is unknown and command is not redispatched",
                    ));
                }
            }
        }
        self.advance_waiting(job)?;
        if self.waiting_is_idle(job) {
            job.phase = WorkJobPhase::Completed;
            return Ok(WorkJobStep::Complete(job.result()));
        }
        if job
            .controller_state
            .is_some_and(controller_state_is_terminal)
        {
            job.phase = WorkJobPhase::Completed;
            return Ok(WorkJobStep::Complete(job.result()));
        }
        // A daemon can die after controller dispatch has admitted provider work
        // but before this WorkJob publishes its next checkpoint. Replaying the
        // controller action here would be an unprovable second dispatch. A
        // durable active-provider identity is the only safe reattach signal;
        // absent one, preserve the revolution and fail closed as unknown.
        if invocation == WorkJobInvocation::Resume && job.resume_result.is_none() {
            if matches!(
                resume_dispatch_is_proven(&job.request.loop_id, &job.request.generation)?,
                ResumeDispatchDecision::Unknown
            ) {
                job.resume_result = Some(json!({
                    "recovery": "unknown",
                    "reason": "daemon stopped after dispatch admission before WorkJob checkpoint"
                }));
                job.phase = WorkJobPhase::Completed;
                job.controller_state = Some(AgentTaskLoopControllerState::Failed);
                let mut record = agent_task_loop_controller::load_controller(&job.request.loop_id)?;
                record.state = AgentTaskLoopControllerState::Failed;
                agent_task_loop_controller::write_controller(&record)?;
                return Ok(WorkJobStep::Complete(job.result()));
            }
        }
        let mut catalog = job.request.provider_catalog.clone();
        apply_admitted_environment(
            &mut catalog,
            &job.request.dispatch_defaults["env_materialization"],
        )?;
        let executor: SharedAgentTaskExecutor = std::sync::Arc::new(
            ExtensionProviderAgentTaskExecutor::from_catalog(catalog.clone()),
        );
        let dispatch = LoopDispatchHook {
            executor: executor.clone(),
            catalog,
            defaults: ControllerDispatchOverrides {
                backend: job.request.dispatch_defaults["backend"]
                    .as_str()
                    .map(str::to_string),
                selector: job.request.dispatch_defaults["selector"]
                    .as_str()
                    .map(str::to_string),
                model: job.request.dispatch_defaults["model"]
                    .as_str()
                    .map(str::to_string),
                provider_config: job.request.dispatch_defaults["provider_config_ref"]
                    .as_str()
                    .map(|reference| format!("@{reference}")),
            },
        };
        write_loop_dispatch_receipt(&job.request.loop_id, &job.request.generation, "dispatching")?;
        let report = match crate::agent_task_controller_service::resume_with_options(
            &job.request.loop_id,
            executor,
            &dispatch,
            crate::agent_task_controller_service::ControllerResumeOptions {
                max_actions: 1,
                stop_on_terminal: true,
            },
        ) {
            Ok(report) => report,
            Err(error) => {
                job.refresh_controller_state();
                if job
                    .controller_state
                    .is_some_and(controller_state_is_terminal)
                {
                    job.phase = WorkJobPhase::Completed;
                    return Ok(WorkJobStep::Complete(job.result()));
                }
                return Err(error);
            }
        };
        write_loop_dispatch_completion_receipt(&job.request.loop_id, &job.request.generation)?;
        let action_failed = report.value.stopped_reason == "action_failed";
        let action_cancelled = report
            .value
            .results
            .iter()
            .any(value_contains_cancelled_state);
        job.resume_result = Some(
            serde_json::to_value(report.value)
                .map_err(|error| homeboy_core::Error::internal_json(error.to_string(), None))?,
        );
        #[cfg(any(test, feature = "test-support"))]
        if test_interrupt_after_loop_dispatch() {
            return Err(homeboy_core::Error::internal_unexpected(
                "test interruption after loop dispatch",
            ));
        }
        job.refresh_controller_state();
        if action_cancelled {
            job.phase = WorkJobPhase::Completed;
            job.controller_state = Some(AgentTaskLoopControllerState::Abandoned);
            return Ok(WorkJobStep::Complete(job.result()));
        }
        if action_failed
            && !job
                .controller_state
                .is_some_and(controller_state_is_terminal)
        {
            job.phase = WorkJobPhase::Completed;
            job.controller_state = Some(AgentTaskLoopControllerState::Failed);
            return Ok(WorkJobStep::Complete(job.result()));
        }
        if self.waiting_is_idle(job) {
            job.phase = WorkJobPhase::Completed;
            return Ok(WorkJobStep::Complete(job.result()));
        }
        if job
            .controller_state
            .is_some_and(controller_state_is_terminal)
        {
            job.phase = WorkJobPhase::Completed;
            return Ok(WorkJobStep::Complete(job.result()));
        }
        Ok(WorkJobStep::Continue {
            checkpoint: job.to_checkpoint()?,
            progress: job.result(),
            wait: SUPERVISION_POLL,
        })
    }
}

fn value_contains_cancelled_state(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| state == "cancelled")
                || object.values().any(value_contains_cancelled_state)
        }
        Value::Array(values) => values.iter().any(value_contains_cancelled_state),
        _ => false,
    }
}

fn write_loop_dispatch_receipt(loop_id: &str, generation: &str, state: &str) -> Result<()> {
    let mut record = agent_task_loop_controller::load_controller(loop_id)?;
    if !record.metadata.is_object() {
        record.metadata = json!({});
    }
    let action_id = record
        .metadata
        .pointer("/loop_dispatch_receipt/action_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            record
                .next_actions
                .iter()
                .find(|action| action.status.is_open())
                .map(|action| action.action_id.clone())
        })
        .or_else(|| {
            record
                .terminal_outcomes
                .last()
                .and_then(|outcome| outcome.action_id.clone())
        });
    record.metadata["loop_dispatch_receipt"] = json!({
        "schema": "homeboy/agent-task-loop-dispatch-receipt/v1",
        "generation": generation,
        "state": state,
        "action_id": action_id,
    });
    agent_task_loop_controller::write_controller(&record)
}

fn write_loop_dispatch_completion_receipt(loop_id: &str, generation: &str) -> Result<()> {
    let record = agent_task_loop_controller::load_controller(loop_id)?;
    let pending = record
        .next_actions
        .iter()
        .any(|action| action.status.is_open());
    write_loop_dispatch_receipt(
        loop_id,
        generation,
        if pending { "ambiguous" } else { "completed" },
    )
}

/// Classify a pre-checkpoint resume from durable controller and lifecycle
/// evidence. The active-run list is only a reference: it is not ownership
/// proof on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeDispatchDecision {
    SafeContinuation,
    Unknown,
}

#[derive(Deserialize)]
struct LoopCommandExecutionOwnership {
    schema: String,
    action_id: String,
    owner_pid: u32,
    owner_start: homeboy_core::process::ProcessStartIdentity,
    root_pid: Option<u32>,
    root_start: Option<homeboy_core::process::ProcessStartIdentity>,
}

/// Shared by supervision and the read-only terminal-evidence provider. Missing
/// root publication is an unresolved spawn handoff, never proof of death.
pub(crate) fn guarded_command_execution_owner(
    value: &Value,
) -> Option<homeboy_core::daemon::controller_job_driver::ControllerJobExecutionOwner> {
    use homeboy_core::daemon::controller_job_driver::{
        ControllerJobExecutionOwner as Owner, ControllerJobExecutionProcess,
    };
    if value.is_null() {
        return None;
    }
    let Ok(receipt) = serde_json::from_value::<LoopCommandExecutionOwnership>(value.clone()) else {
        return Some(Owner::Unavailable);
    };
    if receipt.schema != "homeboy/loop-command-ownership/v1" || receipt.action_id.is_empty() {
        return Some(Owner::Unavailable);
    }
    let Some((root_pid, root_start)) = receipt.root_pid.zip(receipt.root_start) else {
        return Some(Owner::Unavailable);
    };
    Some(Owner::Processes(vec![
        ControllerJobExecutionProcess {
            pid: receipt.owner_pid,
            start_identity: receipt.owner_start,
        },
        ControllerJobExecutionProcess {
            pid: root_pid,
            start_identity: root_start,
        },
    ]))
}

/// Project only generation-bound provider ownership. Reading status must never
/// advance a waiting controller or replay a dispatch receipt.
fn loop_execution_owner(
    loop_id: &str,
    generation: &str,
) -> Result<homeboy_core::daemon::controller_job_driver::ControllerJobExecutionOwner> {
    use homeboy_core::daemon::controller_job_driver::{
        ControllerJobExecutionOwner as Owner, ControllerJobExecutionProcess,
    };
    use homeboy_core::process::ProcessStartIdentity;
    let record = agent_task_loop_controller::load_controller(loop_id)?;
    if record.metadata["command_recovery"]["state"] != "reaped" {
        if let Some(owner) = guarded_command_execution_owner(&record.metadata["command_recovery"]) {
            let dispatch = &record.metadata["loop_dispatch_receipt"];
            if dispatch["schema"] != "homeboy/agent-task-loop-dispatch-receipt/v1"
                || dispatch["generation"].as_str() != Some(generation)
                || dispatch["action_id"] != record.metadata["command_recovery"]["action_id"]
            {
                return Ok(Owner::Unavailable);
            }
            return Ok(owner);
        }
    }
    let Some(runs) = record.metadata["active_provider_runs"].as_array() else {
        return Ok(
            if controller_state_is_terminal(record.state) || controller_is_waiting_idle(&record) {
                Owner::None
            } else {
                Owner::Unavailable
            },
        );
    };
    let store = crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let mut processes = Vec::new();
    for run in runs {
        let Some(run_id) = run.as_str() else {
            return Ok(Owner::Unavailable);
        };
        let lineage = &record.metadata["active_provider_run_lineage"][run_id];
        if lineage["loop_id"].as_str() != Some(loop_id)
            || lineage["generation"].as_str() != Some(generation)
            || !lineage["action_id"].as_str().is_some_and(|id| {
                record
                    .next_actions
                    .iter()
                    .any(|action| action.action_id == id && action.status.is_open())
            })
        {
            return Ok(Owner::Unavailable);
        }
        let run = store.read_record_bounded(run_id)?;
        if run.state.is_terminal() {
            continue;
        }
        if run.state != crate::agent_task_lifecycle::AgentTaskRunState::Running {
            return Ok(Owner::Unavailable);
        }
        let executions = run.metadata["provider_executions"].as_array();
        let active = executions
            .into_iter()
            .flatten()
            .filter(|execution| execution["state"] == "running")
            .collect::<Vec<_>>();
        if active.is_empty() {
            let Some(pid) = run.metadata["runner_pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
            else {
                return Ok(Owner::Unavailable);
            };
            let Ok(start_identity) =
                serde_json::from_value(run.metadata["runner_process_start_identity"].clone())
            else {
                return Ok(Owner::Unavailable);
            };
            processes.push(ControllerJobExecutionProcess {
                pid,
                start_identity,
            });
        } else {
            for execution in active {
                let Some(pid) = execution["owner_pid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
                else {
                    return Ok(Owner::Unavailable);
                };
                let Some(starttime_ticks) = execution["owner_linux_starttime_ticks"].as_u64()
                else {
                    return Ok(Owner::Unavailable);
                };
                processes.push(ControllerJobExecutionProcess {
                    pid,
                    start_identity: ProcessStartIdentity::Linux { starttime_ticks },
                });
            }
        }
    }
    if !processes.is_empty() {
        return Ok(Owner::Processes(processes));
    }
    let receipt = &record.metadata["loop_dispatch_receipt"];
    Ok(
        if controller_state_is_terminal(record.state)
            || controller_is_waiting_idle(&record)
            || (receipt["schema"] == "homeboy/agent-task-loop-dispatch-receipt/v1"
                && receipt["generation"].as_str() == Some(generation)
                && matches!(
                    receipt["state"].as_str(),
                    Some("completed" | "pre_dispatch")
                ))
        {
            Owner::None
        } else {
            Owner::Unavailable
        },
    )
}

fn resume_dispatch_is_proven(loop_id: &str, generation: &str) -> Result<ResumeDispatchDecision> {
    let record = agent_task_loop_controller::load_controller(loop_id)?;
    let has_pending_action = record
        .next_actions
        .iter()
        .any(|action| action.status.is_open());
    let receipt = record.metadata.get("loop_dispatch_receipt");
    let receipt_matches_generation = receipt.is_some_and(|receipt| {
        receipt.get("schema").and_then(Value::as_str)
            == Some("homeboy/agent-task-loop-dispatch-receipt/v1")
            && receipt
                .get("generation")
                .and_then(Value::as_str)
                .is_some_and(|value| value == generation)
    });
    let receipt_state = receipt
        .filter(|_| receipt_matches_generation)
        .and_then(|receipt| receipt.get("state"))
        .and_then(Value::as_str);
    let receipt_action_id = receipt
        .filter(|_| receipt_matches_generation)
        .and_then(|receipt| receipt.get("action_id"))
        .and_then(Value::as_str);
    let receipt_action = receipt_action_id.and_then(|action_id| {
        record
            .next_actions
            .iter()
            .find(|action| action.action_id == action_id)
    });
    if receipt_state == Some("completed")
        && receipt_action.is_some_and(|action| {
            !matches!(
                action.status,
                crate::agent_task_loop_controller::AgentTaskLoopActionStatus::Failed
                    | crate::agent_task_loop_controller::AgentTaskLoopActionStatus::Cancelled
                    | crate::agent_task_loop_controller::AgentTaskLoopActionStatus::BlockedRunnerUnavailable
                    | crate::agent_task_loop_controller::AgentTaskLoopActionStatus::BlockedRemoteMaterialization
                    | crate::agent_task_loop_controller::AgentTaskLoopActionStatus::BlockedLocalFallbackDenied
            )
        })
        && !has_pending_action
    {
        return Ok(ResumeDispatchDecision::SafeContinuation);
    }
    if receipt_state == Some("pre_dispatch")
        && receipt_action.is_some_and(|action| action.status.is_open())
    {
        return Ok(ResumeDispatchDecision::SafeContinuation);
    }
    if receipt_state.is_none() {
        // Missing, stale, malformed, or legacy receipts do not prove that
        // this WorkJob was admitted before the restart boundary.
        return Ok(ResumeDispatchDecision::Unknown);
    }
    let Some(active_runs_value) = record.metadata.get("active_provider_runs") else {
        // A dispatch receipt exists but its owner projection is missing.
        return Ok(ResumeDispatchDecision::Unknown);
    };
    let active_runs = active_runs_value
        .as_array()
        .map(|runs| runs.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    if active_runs.is_empty() {
        // The admission ledger exists but contains no verified owner. The
        // dispatch may have completed before the WorkJob checkpoint, so this
        // is ambiguous and must not be replayed.
        return Ok(ResumeDispatchDecision::Unknown);
    }
    let owned_active_run = loop_execution_owner(loop_id, generation)
        .is_ok_and(|owner| owner.inspect() == homeboy_core::process::ProcessIdentityState::Live);
    Ok(if owned_active_run {
        ResumeDispatchDecision::SafeContinuation
    } else {
        ResumeDispatchDecision::Unknown
    })
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_INTERRUPT_AFTER_LOOP_DISPATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn with_test_loop_checkpoint_interrupt<T>(body: impl FnOnce() -> T) -> T {
    TEST_INTERRUPT_AFTER_LOOP_DISPATCH.with(|flag| flag.set(true));
    let result = body();
    TEST_INTERRUPT_AFTER_LOOP_DISPATCH.with(|flag| flag.set(false));
    result
}

#[cfg(any(test, feature = "test-support"))]
fn test_interrupt_after_loop_dispatch() -> bool {
    if TEST_INTERRUPT_AFTER_LOOP_DISPATCH.with(|flag| flag.replace(false)) {
        return true;
    }
    let Some(marker) = std::env::var_os("HOMEBOY_TEST_LOOP_DISPATCH_ADMITTED") else {
        return false;
    };
    // This hook is compiled only into test-support builds. The marker gives a
    // subprocess test a precise admission boundary; the test owns termination
    // of the daemon, rather than exposing a production CLI fault switch.
    let _ = std::fs::write(marker, format!("{}\n", std::process::id()));
    loop {
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Apply the caller's public environment projection to provider declarations.
/// The command runner then uses its normal launch-context path and resolves
/// secret names separately via `SecretEnvPlan`. Unlisted names are rejected
/// instead of becoming authority through the daemon environment.
fn apply_admitted_environment(catalog: &mut AgentTaskProviderCatalog, value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let mut plan: homeboy_core::env_materialization_plan::EnvMaterializationPlan =
        serde_json::from_value(value.clone()).map_err(|error| {
            invalid_loop_job(&format!(
                "invalid loop environment materialization plan: {error}"
            ))
        })?;
    plan.normalize();
    for (name, value) in &plan.public_env {
        let mut declared = false;
        for provider in &mut catalog.providers {
            if crate::agent_task_provider::provider_secret_env_plan(
                provider,
                &serde_json::from_value(json!({
                    "task_id": "loop-environment-materialization",
                    "executor": { "backend": provider.backend },
                    "instructions": ""
                }))
                .map_err(|error| {
                    invalid_loop_job(&format!("invalid provider secret plan input: {error}"))
                })?,
            )
            .secret_env_names()
            .iter()
            .any(|secret_name| secret_name == name)
            {
                return Err(invalid_loop_job(&format!(
                    "loop environment materialization cannot override secret provider variable `{name}`"
                )));
            }
            for env in &mut provider.invocation.env {
                if env.name == *name {
                    if env.redacted == Some(true) || env.source.as_deref() == Some("secret_env") {
                        return Err(invalid_loop_job(&format!(
                            "loop environment materialization cannot override secret provider variable `{name}`"
                        )));
                    }
                    env.source = Some("value".to_string());
                    env.value = Some(value.clone());
                    env.redacted = Some(false);
                    declared = true;
                }
            }
        }
        if !declared {
            return Err(invalid_loop_job(&format!(
                "loop environment materialization names undeclared provider variable `{name}`"
            )));
        }
    }
    Ok(())
}

#[derive(Clone)]
struct LoopDispatchHook {
    executor: SharedAgentTaskExecutor,
    catalog: AgentTaskProviderCatalog,
    defaults: ControllerDispatchOverrides,
}

impl ControllerDispatchHook for LoopDispatchHook {
    fn dispatch(&self, request: &Value) -> Result<(Value, i32)> {
        let command = crate::agent_task_controller_service::controller_request_dispatch_command(
            request,
            &self.defaults,
        )?;
        agent_task_dispatch_service::run_dispatch_command_with_provider_catalog(
            command,
            self.executor.clone(),
            &self.catalog,
        )
    }
}

fn terminalize_interrupted(
    job: &mut AgentTaskLoopJob,
    interrupted_state: AgentTaskLoopControllerState,
) -> Result<Value> {
    job.refresh_controller_state();
    if !job
        .controller_state
        .is_some_and(controller_state_is_terminal)
    {
        let mut record = agent_task_loop_controller::load_controller(&job.request.loop_id)?;
        record.state = interrupted_state;
        agent_task_loop_controller::write_controller(&record)?;
        job.controller_state = Some(interrupted_state);
    }
    job.phase = WorkJobPhase::Completed;
    Ok(job.result())
}

fn controller_state_is_terminal(state: AgentTaskLoopControllerState) -> bool {
    matches!(
        state,
        AgentTaskLoopControllerState::HumanReady
            | AgentTaskLoopControllerState::Completed
            | AgentTaskLoopControllerState::Abandoned
            | AgentTaskLoopControllerState::Escalated
            | AgentTaskLoopControllerState::Failed
    )
}

fn controller_is_waiting_idle(
    record: &crate::agent_task_loop_controller::AgentTaskLoopControllerRecord,
) -> bool {
    record.state == AgentTaskLoopControllerState::Waiting
        && record.open_wait_count() == 0
        && record
            .next_actions
            .iter()
            .all(|action| !action.status.is_open())
}

pub fn register_loop_work_job_handler() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        register_work_job_handler(std::sync::Arc::new(LoopWorkHandler))
            .expect("register loop work job handler");
    });
}

/// Admit one daemon-owned loop execution without launching a public CLI child.
pub fn loop_work_job_execution_submission(
    loop_id: &str,
    generation: &str,
    dispatch_defaults: Value,
    provider_catalog: AgentTaskProviderCatalog,
) -> Result<Value> {
    if generation.trim().is_empty() {
        return Err(invalid_loop_job(
            "new loop executions require a non-empty generation",
        ));
    }
    let dispatch_defaults = admitted_dispatch_defaults(dispatch_defaults)?;
    let job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
        schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
        loop_id: loop_id.to_string(),
        kind: AgentTaskLoopJobKind::DaemonExecution,
        generation: generation.to_string(),
        dispatch_defaults,
        provider_catalog,
    })?;
    work_job_submission(
        &LoopWorkHandler,
        job.idempotency_key.clone(),
        job.to_checkpoint()?,
    )
}

/// Give daemon-boundary stop tests an actual, long-running provider to cancel.
#[cfg(any(test, feature = "test-support"))]
pub fn active_loop_stop_test_submission(
    record: &mut crate::agent_task_loop_controller::AgentTaskLoopControllerRecord,
    marker: &std::path::Path,
) -> Result<Value> {
    use crate::agent_task_loop_controller::{
        AgentTaskLoopControllerState, AgentTaskLoopPolicyAction,
    };

    record.state = AgentTaskLoopControllerState::Running;
    record.record_action(
        AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key: "loop-stop-provider".to_string(),
            entity_id: None,
            request: json!({
                "mode": "dispatch",
                "dispatch": { "backend": "loop-stop-provider", "prompt": "wait" }
            }),
        },
        "active loop stop fixture",
    );
    crate::agent_task_loop_controller::write_controller(record)?;
    let provider = serde_json::from_value(json!({
        "id": "loop-stop-provider",
        "backend": "loop-stop-provider",
        "command_argv": [
            "sh", "-c", "touch \"$1\"; exec sleep 30", "loop-stop-fixture",
            marker.display().to_string()
        ],
        "capabilities": ["structured_outcome"]
    }))
    .map_err(|error| homeboy_core::Error::internal_json(error.to_string(), None))?;
    loop_work_job_execution_submission(
        &record.loop_id,
        &record.updated_at,
        json!({ "backend": "loop-stop-provider" }),
        AgentTaskProviderCatalog {
            providers: vec![provider],
            ..Default::default()
        },
    )
}

#[cfg(any(test, feature = "test-support"))]
pub fn await_active_loop_stop_test_provider(loop_id: &str, marker: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let admitted = crate::agent_task_loop_controller::load_controller(loop_id)
            .expect("read loop controller")
            .metadata
            .get("active_provider_runs")
            .and_then(Value::as_array)
            .is_some_and(|runs| !runs.is_empty());
        if admitted && marker.exists() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "provider never became active"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Observe the real daemon job after stop without consuming the test listener's
/// finite HTTP request budget. A cancellation request may precede the worker's
/// durable terminal transition.
#[cfg(any(test, feature = "test-support"))]
pub fn await_cancelled_loop_stop_test_job(job_id: &str) {
    use homeboy_core::api_jobs::{JobStatus, JobStore};

    let id = uuid::Uuid::parse_str(job_id).expect("daemon job id");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let store = JobStore::open_without_reconciliation(
            homeboy_core::paths::daemon_jobs_file().expect("daemon jobs path"),
        )
        .expect("read durable daemon jobs");
        let job = store.get(id).expect("read daemon job");
        if job.status == JobStatus::Cancelled {
            return;
        }
        assert!(
            matches!(job.status, JobStatus::Queued | JobStatus::Running),
            "active job finished without cancellation: {:?}",
            job.status
        );
        assert!(
            std::time::Instant::now() < deadline,
            "active daemon job did not become cancelled: {:?}",
            job.status
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn admitted_dispatch_defaults(value: Value) -> Result<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_loop_job("dispatch defaults must be an object"))?;
    let mut admitted = serde_json::Map::new();
    for key in [
        "backend",
        "selector",
        "model",
        "provider_config_ref",
        "provider_account",
        "provider_catalog",
        "env_materialization",
        "secret_env_plan",
    ] {
        if let Some(value) = object.get(key) {
            admitted.insert(key.to_string(), value.clone());
        }
    }
    Ok(Value::Object(admitted))
}

fn invalid_loop_job(message: &str) -> homeboy_core::Error {
    homeboy_core::Error::validation_invalid_argument("loop_job", message, None, None)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use homeboy_core::daemon::controller_job_driver::ControllerJobDriver;
    use homeboy_core::test_support::{with_isolated_home, ControllerJobHarness, EnvVarGuard};

    use super::*;
    use crate::agent_task_service::work_job::{
        WorkJobDriver, WORK_JOB_CHECKPOINT_SCHEMA, WORK_JOB_TYPE, WORK_JOB_VERSION,
    };

    fn submission(loop_id: &str) -> Value {
        register_loop_work_job_handler();
        loop_work_job_execution_submission(
            loop_id,
            "generation-test",
            json!({}),
            AgentTaskProviderCatalog::default(),
        )
        .expect("build loop work submission")
    }

    fn recovery_record(
        loop_id: &str,
        generation: &str,
    ) -> crate::agent_task_loop_controller::AgentTaskLoopControllerRecord {
        let mut record = agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
            .expect("create recovery controller");
        record.updated_at = generation.to_string();
        record.record_action(
            crate::agent_task_loop_controller::AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "provider-action".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "dispatch",
                    "dispatch": { "backend": "fixture", "prompt": "recover" }
                }),
            },
            "recovery fixture",
        );
        record
    }

    #[test]
    fn loop_execution_owner_is_generation_bound_read_only_and_visible_to_generic_recovery() {
        use homeboy_core::api_jobs::DaemonActiveJobRecoveryDisposition as Disposition;
        use homeboy_core::test_support::SupervisedProcessFixture;
        with_isolated_home(|home| {
            super::super::work_job::register_work_job_driver();
            let mut child = SupervisedProcessFixture::spawn();
            let loop_id = "ownership-loop";
            let generation = "generation-test";
            let mut controller = recovery_record(loop_id, generation);
            let plan = crate::agent_task_scheduler::AgentTaskPlan::new(
                "ownership-loop-provider",
                Vec::new(),
            );
            let mut run =
                crate::agent_task_lifecycle::submit_plan(&plan, Some("ownership-loop-run"))
                    .unwrap();
            run.state = crate::agent_task_lifecycle::AgentTaskRunState::Running;
            run.lifecycle =
                homeboy_core::run_lifecycle_record::RunLifecycleRecord::with_execution_state(
                    homeboy_core::run_lifecycle_record::RunExecutionState::Running,
                );
            run.metadata["runner_pid"] = json!(child.pid());
            run.metadata["runner_process_start_identity"] = json!(child.identity);
            let lifecycle_store =
                crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                    .unwrap();
            lifecycle_store.write_record(&run).unwrap();
            controller.metadata["active_provider_runs"] = json!([run.run_id]);
            controller.metadata["active_provider_run_lineage"] = json!({run.run_id.clone(): {
                "loop_id": loop_id, "generation": generation, "action_id": controller.next_actions[0].action_id
            }});
            controller.metadata["loop_dispatch_receipt"] = json!({
                "schema": "homeboy/agent-task-loop-dispatch-receipt/v1",
                "generation": generation, "state": "dispatching", "action_id": controller.next_actions[0].action_id
            });
            agent_task_loop_controller::write_controller(&controller).unwrap();
            let submission = submission(loop_id);
            let path = home.path().join("ownership-loop-jobs.json");
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::durable(
                Arc::clone(&driver),
                submission["request"].clone(),
                &path,
                "loop-lease",
            )
            .unwrap();
            harness
                .handle()
                .checkpoint(driver.prepare(submission["request"].clone()).unwrap())
                .unwrap();
            let restarted = harness.reopen(&path, "replacement-lease").unwrap();
            let before = std::fs::read(&path).unwrap();
            let controller_before =
                serde_json::to_value(agent_task_loop_controller::load_controller(loop_id).unwrap())
                    .unwrap();
            let run_before =
                serde_json::to_value(lifecycle_store.read_record_bounded(&run.run_id).unwrap())
                    .unwrap();
            let evidence = restarted
                .store()
                .active_daemon_job_recovery_evidence(None, |_| false);
            assert_eq!(evidence[0].child_pid, Some(child.pid()));
            assert_eq!(evidence[0].disposition, Disposition::ProtectedLive);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(
                serde_json::to_value(agent_task_loop_controller::load_controller(loop_id).unwrap())
                    .unwrap(),
                controller_before
            );
            assert_eq!(
                serde_json::to_value(lifecycle_store.read_record_bounded(&run.run_id).unwrap())
                    .unwrap(),
                run_before
            );
            assert_eq!(
                resume_dispatch_is_proven(loop_id, generation).unwrap(),
                ResumeDispatchDecision::SafeContinuation
            );

            controller.metadata["active_provider_run_lineage"][&run.run_id]["generation"] =
                json!("foreign-generation");
            agent_task_loop_controller::write_controller(&controller).unwrap();
            assert_eq!(
                restarted
                    .store()
                    .active_daemon_job_recovery_evidence(None, |_| false)[0]
                    .disposition,
                Disposition::BlockingAmbiguous
            );
            controller.metadata["active_provider_run_lineage"][&run.run_id]["generation"] =
                json!(generation);
            agent_task_loop_controller::write_controller(&controller).unwrap();
            child.stop();
            assert_eq!(
                restarted
                    .store()
                    .active_daemon_job_recovery_evidence(None, |_| true)[0]
                    .disposition,
                Disposition::DeadChild
            );
            let diagnostics = restarted
                .store()
                .reconcile_dead_daemon_lease_jobs("loop-lease")
                .unwrap();
            assert_eq!(
                diagnostics.preserved_controller_job_ids,
                vec![harness.job().unwrap().id]
            );
            restarted.recover_after_restart();
            restarted.wait_until_terminal().unwrap();
            assert_eq!(
                agent_task_loop_controller::load_controller(loop_id)
                    .unwrap()
                    .state,
                AgentTaskLoopControllerState::Failed,
                "an interrupted dispatch must not be replayed"
            );
        });
    }

    #[test]
    fn resume_recovery_accepts_only_current_dispatch_receipts() {
        with_isolated_home(|_| {
            let generation = "generation-current";

            let mut predispatch = recovery_record("loop-recovery-predispatch", generation);
            predispatch.metadata["loop_dispatch_receipt"] = json!({
                "schema": "homeboy/agent-task-loop-dispatch-receipt/v1",
                "generation": generation,
                "state": "pre_dispatch",
                "action_id": "action-1",
            });
            agent_task_loop_controller::write_controller(&predispatch).expect("write predispatch");
            assert_eq!(
                resume_dispatch_is_proven("loop-recovery-predispatch", generation)
                    .expect("classify predispatch"),
                ResumeDispatchDecision::SafeContinuation
            );

            let mut completed = recovery_record("loop-recovery-completed", generation);
            completed.next_actions[0].status =
                crate::agent_task_loop_controller::AgentTaskLoopActionStatus::Completed;
            completed.metadata["loop_dispatch_receipt"] = json!({
                "schema": "homeboy/agent-task-loop-dispatch-receipt/v1",
                "generation": generation,
                "state": "completed",
                "action_id": "action-1",
            });
            agent_task_loop_controller::write_controller(&completed).expect("write completed");
            assert_eq!(
                resume_dispatch_is_proven("loop-recovery-completed", generation)
                    .expect("classify completed"),
                ResumeDispatchDecision::SafeContinuation
            );
        });
    }

    #[test]
    fn guarded_loop_command_status_is_read_only_and_dead_work_recovers_only_through_driver() {
        use homeboy_core::api_jobs::{
            DaemonActiveJobRecoveryDisposition as Disposition, JobStatus,
        };
        use homeboy_core::test_support::SupervisedProcessFixture;
        with_isolated_home(|home| {
            crate::api_jobs_terminal_recovery::register();
            super::super::work_job::register_work_job_driver();
            let mut owner = SupervisedProcessFixture::spawn();
            let mut root = SupervisedProcessFixture::spawn();
            let loop_id = "ownership-guarded-command";
            let marker = home.path().join("command-must-not-replay");
            let mut record =
                agent_task_loop_controller::create_controller(loop_id, "repair", "v1").unwrap();
            record.record_action(crate::agent_task_loop_controller::AgentTaskLoopPolicyAction::RunCommand {
                dedupe_key: "guarded-command".to_string(), entity_id: None,
                request: json!({"execution": {"command": "sh", "args": ["-c", "touch \"$1\"", "--", marker]}}),
            }, "owned command fixture");
            record.next_actions[0].status =
                crate::agent_task_loop_controller::AgentTaskLoopActionStatus::Running;
            record.metadata["loop_dispatch_receipt"] = json!({
                "schema": "homeboy/agent-task-loop-dispatch-receipt/v1",
                "generation": "generation-test", "action_id": record.next_actions[0].action_id, "state": "dispatching"
            });
            record.metadata["command_recovery"] = json!({
                "schema": "homeboy/loop-command-ownership/v1", "action_id": record.next_actions[0].action_id,
                "owner_pid": owner.pid(), "owner_start": owner.identity,
                "root_pid": root.pid(), "root_start": root.identity, "state": "running"
            });
            agent_task_loop_controller::write_controller(&record).unwrap();
            let submission = submission(loop_id);
            let path = home.path().join("ownership-guarded-jobs.json");
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::durable(
                Arc::clone(&driver),
                submission["request"].clone(),
                &path,
                "guarded-lease",
            )
            .unwrap();
            harness
                .handle()
                .checkpoint(driver.prepare(submission["request"].clone()).unwrap())
                .unwrap();
            owner.stop();
            let before =
                serde_json::to_value(agent_task_loop_controller::load_controller(loop_id).unwrap())
                    .unwrap();
            let evidence = harness
                .store()
                .active_daemon_job_recovery_evidence(None, |_| false);
            assert_eq!(
                evidence[0].disposition,
                Disposition::ProtectedLive,
                "a surviving root is protected even after its daemon owner died"
            );
            assert_eq!(evidence[0].child_pid, Some(root.pid()));
            root.stop();
            let job_bytes = std::fs::read(&path).unwrap();
            let evidence = harness
                .store()
                .active_daemon_job_recovery_evidence(None, |_| true);
            assert_eq!(evidence[0].disposition, Disposition::DeadChild);
            assert_eq!(
                evidence[0].linked_durable_run_terminal_status,
                Some(JobStatus::Failed)
            );
            assert_eq!(std::fs::read(&path).unwrap(), job_bytes);
            assert_eq!(
                serde_json::to_value(agent_task_loop_controller::load_controller(loop_id).unwrap())
                    .unwrap(),
                before,
                "terminal-evidence reads cannot reconcile the controller"
            );
            assert!(harness
                .store()
                .reconcile_terminal_linked_daemon_jobs()
                .unwrap()
                .is_empty());
            assert_eq!(
                harness
                    .store()
                    .reconcile_dead_daemon_lease_jobs("guarded-lease")
                    .unwrap()
                    .preserved_controller_job_ids,
                vec![harness.job().unwrap().id]
            );
            let restarted = harness.reopen(&path, "replacement-lease").unwrap();
            restarted.recover_after_restart();
            assert_eq!(
                restarted.wait_until_terminal().unwrap().status,
                JobStatus::Failed
            );
            let recovered = agent_task_loop_controller::load_controller(loop_id).unwrap();
            assert_eq!(recovered.state, AgentTaskLoopControllerState::Failed);
            assert_eq!(
                recovered.next_actions[0].status,
                crate::agent_task_loop_controller::AgentTaskLoopActionStatus::Failed
            );
            assert_eq!(
                recovered.metadata["command_recovery"]["state"],
                "owner_lost_unknown_outcome"
            );
            assert!(
                !marker.exists(),
                "driver recovery must never replay the command"
            );
        });
    }

    #[test]
    fn resume_recovery_rejects_legacy_stale_malformed_and_foreign_admission() {
        with_isolated_home(|_| {
            let generation = "generation-current";
            for (loop_id, receipt) in [
                ("loop-recovery-legacy", Value::Null),
                (
                    "loop-recovery-stale",
                    json!({
                        "schema": "homeboy/agent-task-loop-dispatch-receipt/v1",
                        "generation": "generation-old",
                        "state": "pre_dispatch",
                    }),
                ),
                ("loop-recovery-malformed", json!({ "state": "completed" })),
            ] {
                let mut record = recovery_record(loop_id, generation);
                if !receipt.is_null() {
                    record.metadata["loop_dispatch_receipt"] = receipt;
                }
                agent_task_loop_controller::write_controller(&record)
                    .expect("write receipt fixture");
                assert_eq!(
                    resume_dispatch_is_proven(loop_id, generation).expect("classify receipt"),
                    ResumeDispatchDecision::Unknown
                );
            }

            let mut foreign = recovery_record("loop-recovery-foreign", generation);
            foreign.metadata["loop_dispatch_receipt"] = json!({
                "schema": "homeboy/agent-task-loop-dispatch-receipt/v1",
                "generation": generation,
                "state": "dispatching",
            });
            foreign.metadata["active_provider_runs"] = json!(["foreign-live-run"]);
            foreign.metadata["active_provider_run_lineage"] = json!({
                "foreign-live-run": {
                    "loop_id": "other-loop",
                    "action_id": foreign.next_actions[0].action_id,
                    "generation": generation,
                }
            });
            agent_task_loop_controller::write_controller(&foreign).expect("write foreign fixture");
            assert_eq!(
                resume_dispatch_is_proven("loop-recovery-foreign", generation)
                    .expect("classify foreign"),
                ResumeDispatchDecision::Unknown
            );
        });
    }

    #[test]
    fn new_loop_submissions_use_the_shared_work_driver() {
        let submission = submission("loop-shared");

        assert_eq!(submission["type"], WORK_JOB_TYPE);
        assert_eq!(submission["version"], WORK_JOB_VERSION);
        assert_eq!(
            submission["idempotency_key"],
            "agent-task-loop:loop-shared:generation-test"
        );
        assert_eq!(submission["request"]["work_type"], AGENT_TASK_LOOP_JOB_TYPE);
        assert_eq!(
            submission["request"]["request"]["request"]["loop_id"],
            "loop-shared"
        );
        WorkJobDriver
            .validate_secret_references(&submission["request"])
            .expect("reference-only loop request validates");
        let public = WorkJobDriver
            .public_request(&submission["request"])
            .expect("safe public projection");
        assert_eq!(public["loop_id"], "loop-shared");
        assert_eq!(public["kind"], "daemon_execution");
        assert!(!public.to_string().contains("starttime_ticks"));
    }

    #[test]
    fn execution_submissions_admit_catalog_and_never_persist_provider_config() {
        let submission = loop_work_job_execution_submission(
            "loop-execution",
            "generation-1",
            json!({
                "backend": "fixture",
                "provider_config": "credential-value-must-not-cross-boundary"
            }),
            AgentTaskProviderCatalog::default(),
        )
        .expect("build execution submission");

        let encoded = submission.to_string();
        assert!(!encoded.contains("credential-value-must-not-cross-boundary"));
        assert!(submission["request"]["request"]["request"]["provider_catalog"].is_object());
    }

    #[test]
    fn execution_dispatch_keeps_the_admitted_provider_config_reference() {
        let submission = loop_work_job_execution_submission(
            "loop-config-reference-dispatch",
            "generation-1",
            json!({
                "backend": "fixture",
                "provider_config_ref": "provider-configs/caller-a"
            }),
            AgentTaskProviderCatalog::default(),
        )
        .expect("build execution submission");
        let defaults = ControllerDispatchOverrides {
            backend: Some("fixture".to_string()),
            provider_config: submission["request"]["request"]["request"]["dispatch_defaults"]
                ["provider_config_ref"]
                .as_str()
                .map(|reference| format!("@{reference}")),
            ..Default::default()
        };
        let command = crate::agent_task_controller_service::controller_request_dispatch_command(
            &json!({"mode": "dispatch", "prompt": "fixture"}),
            &defaults,
        )
        .expect("dispatch command");
        assert_eq!(
            command.core.provider_config.as_deref(),
            Some("@provider-configs/caller-a")
        );
    }

    #[test]
    fn loop_work_job_provider_receives_caller_environment_and_private_config_reference() {
        with_isolated_home(|_| {
            let loop_id = "loop-caller-environment-a";
            let observed = tempfile::NamedTempFile::new().expect("observed provider output");
            let config = tempfile::NamedTempFile::new().expect("private provider config");
            std::fs::write(config.path(), r#"{"account":"caller-a"}"#)
                .expect("write caller config");
            let _daemon = EnvVarGuard::set("HOMEBOY_FIXTURE_AUTHORITY", "daemon-b");
            let mut record = agent_task_loop_controller::create_controller(
                loop_id,
                "repair",
                "v1",
            )
            .expect("create controller");
            record.record_action(
                crate::agent_task_loop_controller::AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "caller-environment-provider".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "dispatch",
                        "dispatch": { "backend": "caller-fixture", "prompt": "observe" }
                    }),
                },
                "caller environment handoff fixture",
            );
            agent_task_loop_controller::write_controller(&record).expect("write controller");
            let provider: crate::agent_task_provider::AgentTaskExecutorProvider =
                serde_json::from_value(json!({
                    "id": "caller-fixture",
                    "backend": "caller-fixture",
                    "command_argv": [
                        "sh", "-c",
                        format!(
                            "printf '%s\\n%s' $HOMEBOY_FIXTURE_AUTHORITY $HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON > {}; printf '%s' '{{\"schema\":\"homeboy/agent-task-outcome/v1\",\"task_id\":\"fixture\",\"status\":\"succeeded\"}}'",
                            observed.path().display()
                        )
                    ],
                    "invocation": { "env": [{
                        "name": "HOMEBOY_FIXTURE_AUTHORITY",
                        "source": "env",
                        "required": true
                    }] },
                    "capabilities": ["structured_outcome"]
                }))
                .expect("provider fixture");
            let catalog = AgentTaskProviderCatalog {
                providers: vec![provider],
                ..Default::default()
            };
            let mut job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: loop_id.to_string(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                generation: "generation-caller-a".to_string(),
                dispatch_defaults: json!({
                    "backend": "caller-fixture",
                    "provider_config_ref": config.path().display().to_string(),
                    "env_materialization": {
                        "schema": "homeboy/env-materialization-plan/v1",
                        "public_env": { "HOMEBOY_FIXTURE_AUTHORITY": "caller-a" }
                    }
                }),
                provider_catalog: catalog,
            })?;

            let _ = LoopWorkHandler.observe(&mut job, WorkJobInvocation::Execute)?;
            let observed = std::fs::read_to_string(observed.path()).expect("provider ran");
            assert!(observed.starts_with("caller-a\n"), "observed={observed}");
            assert!(observed.contains(r#""account":"caller-a""#));
            assert!(!observed.contains("daemon-b"));
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("caller environment handoff converges");
    }

    #[test]
    fn public_environment_materialization_cannot_override_secret_authority() {
        let mut catalog = AgentTaskProviderCatalog {
            providers: vec![serde_json::from_value(json!({
                "id": "secret-fixture",
                "backend": "secret-fixture",
                "invocation": { "env": [{
                    "name": "FAKE_TOKEN",
                    "source": "secret_env",
                    "redacted": true
                }] }
            }))
            .expect("secret provider")],
            ..Default::default()
        };
        let error = apply_admitted_environment(
            &mut catalog,
            &json!({
                "schema": "homeboy/env-materialization-plan/v1",
                "public_env": { "FAKE_TOKEN": "not-a-secret" }
            }),
        )
        .expect_err("public env must not replace secret authority");
        assert!(
            error.to_string().contains("secret provider variable"),
            "unexpected validation error: {error:?}"
        );
    }

    #[test]
    fn obsolete_process_identity_fields_are_rejected_by_the_strict_schema() {
        let request = json!({
            "schema": AGENT_TASK_LOOP_JOB_SCHEMA,
            "loop_id": "loop-invalid-identity",
            "kind": "daemon_execution",
            "generation": "generation-1",
            "dispatch_defaults": null,
            "provider_catalog": AgentTaskProviderCatalog::default(),
            "child_pid": 4242,
            "child_start_identity": { "linux": { "starttime_ticks": 1 } }
        });
        let error = AgentTaskLoopJob::parse(json!({
            "schema": AGENT_TASK_LOOP_JOB_SCHEMA,
            "idempotency_key": "agent-task-loop:loop-invalid-identity:generation-1",
            "request": request,
            "phase": "queued"
        }))
        .expect_err("obsolete process identity must be rejected");
        assert!(format!("{error:?}").contains("unknown field"));
    }

    #[test]
    fn new_execution_requires_a_generation_and_preserves_provider_config_reference() {
        let error = loop_work_job_execution_submission(
            "loop-missing-generation",
            "",
            json!({
                "backend": "fixture",
                "provider_config_ref": "provider-configs/primary"
            }),
            AgentTaskProviderCatalog::default(),
        )
        .expect_err("execution generation is required");
        assert!(format!("{error:?}").contains("generation"));

        let submission = loop_work_job_execution_submission(
            "loop-config-reference",
            "generation-1",
            json!({
                "backend": "fixture",
                "provider_config_ref": "provider-configs/primary"
            }),
            AgentTaskProviderCatalog::default(),
        )
        .expect("reference-only config is admitted");
        assert_eq!(
            submission["request"]["request"]["request"]["dispatch_defaults"]["provider_config_ref"],
            "provider-configs/primary"
        );
    }

    #[test]
    fn cancelling_new_execution_cancels_owned_run_instead_of_returning_immediately() {
        with_isolated_home(|_| {
            let loop_id = "loop-cancel-execution";
            let mut record = agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
                .expect("create controller");
            record.state = AgentTaskLoopControllerState::Running;
            record.task_lineage.push(
                crate::agent_task_loop_controller::AgentTaskLoopTaskLineage {
                    run_id: "owned-provider-run".to_string(),
                    task_id: None,
                    parent_run_id: None,
                    parent_task_id: None,
                    entity_id: None,
                    dedupe_key: Some("dispatch".to_string()),
                    artifact_refs: Vec::new(),
                    inputs: Value::Null,
                    outputs: Value::Null,
                },
            );
            agent_task_loop_controller::write_controller(&record).expect("write controller");
            let job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: loop_id.to_string(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                generation: "generation-1".to_string(),
                dispatch_defaults: json!({}),
                provider_catalog: AgentTaskProviderCatalog::default(),
            })?;

            LoopWorkHandler.cancel(&job.to_checkpoint()?)?;
            assert_eq!(
                agent_task_loop_controller::load_controller(loop_id)?.state,
                AgentTaskLoopControllerState::Abandoned
            );
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("new execution cancellation converges");
    }

    #[test]
    fn failed_action_blocks_downstream_work_and_terminalizes_supervision() {
        with_isolated_home(|_| {
            let loop_id = "loop-failed-blocker";
            let downstream_marker = tempfile::NamedTempFile::new().expect("marker");
            std::fs::remove_file(downstream_marker.path()).expect("remove marker placeholder");
            let mut record = agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
                .expect("create controller");
            record.record_action(
                agent_task_loop_controller::AgentTaskLoopPolicyAction::RunCommand {
                    dedupe_key: "producer".to_string(),
                    entity_id: None,
                    request: json!({
                        "execution": { "command": "/bin/sh", "args": ["-c", "exit 7"] }
                    }),
                },
                "producer fixture",
            );
            record.record_action(
                agent_task_loop_controller::AgentTaskLoopPolicyAction::RunCommand {
                    dedupe_key: "consumer".to_string(),
                    entity_id: None,
                    request: json!({
                        "execution": {
                            "command": "/bin/sh",
                            "args": ["-c", format!("touch {}", downstream_marker.path().display())]
                        }
                    }),
                },
                "consumer fixture",
            );
            agent_task_loop_controller::write_controller(&record).expect("write controller");

            let mut job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: loop_id.to_string(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                generation: "generation-failed-blocker".to_string(),
                dispatch_defaults: json!({}),
                provider_catalog: AgentTaskProviderCatalog::default(),
            })?;
            let step = LoopWorkHandler.observe(&mut job, WorkJobInvocation::Execute)?;
            assert!(matches!(step, WorkJobStep::Complete(_)));
            assert_eq!(job.phase, WorkJobPhase::Completed);
            let record = agent_task_loop_controller::load_controller(loop_id)?;
            assert_eq!(record.state, AgentTaskLoopControllerState::Running);
            assert_eq!(
                record.next_actions[0].status,
                agent_task_loop_controller::AgentTaskLoopActionStatus::Failed
            );
            assert_eq!(
                record.next_actions[1].status,
                agent_task_loop_controller::AgentTaskLoopActionStatus::Pending
            );
            assert!(!downstream_marker.path().exists());
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("failed producer blocks consumer");
    }

    #[test]
    fn actionless_loop_waits_and_supervising_work_job_completes() {
        with_isolated_home(|_| {
            let loop_id = "loop-actionless-waiting";
            agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
                .expect("create controller");
            let mut job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: loop_id.to_string(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                generation: "generation-actionless".to_string(),
                dispatch_defaults: json!({}),
                provider_catalog: AgentTaskProviderCatalog::default(),
            })?;
            let step = LoopWorkHandler.observe(&mut job, WorkJobInvocation::Execute)?;
            assert!(matches!(step, WorkJobStep::Complete(_)));
            assert_eq!(job.phase, WorkJobPhase::Completed);
            assert_eq!(
                agent_task_loop_controller::load_controller(loop_id)?.state,
                AgentTaskLoopControllerState::Waiting
            );
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("actionless loop becomes waiting");
    }

    #[test]
    fn active_provider_execution_is_cancelled_through_the_work_job_harness() {
        with_isolated_home(|_| {
            register_loop_work_job_handler();
            let loop_id = "loop-active-provider-cancel";
            let mut record = agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
                .expect("create controller");
            record.state = AgentTaskLoopControllerState::Running;
            record.record_action(
                crate::agent_task_loop_controller::AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "long-provider".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "dispatch",
                        "dispatch": { "backend": "long-provider", "prompt": "wait" }
                    }),
                },
                "active provider cancellation fixture",
            );
            agent_task_loop_controller::write_controller(&record).expect("write controller");
            let provider_marker = tempfile::tempdir().expect("provider marker directory");
            let marker_path = provider_marker.path().join("started");
            let provider: crate::agent_task_provider::AgentTaskExecutorProvider =
                serde_json::from_value(json!({
                    "id": "long-provider",
                    "backend": "long-provider",
                    "command_argv": [
                        "sh",
                        "-c",
                        format!("touch {}; exec sleep 30", marker_path.display())
                    ],
                    "capabilities": ["structured_outcome"]
                }))
                .expect("long provider fixture");
            let catalog = AgentTaskProviderCatalog {
                providers: vec![provider],
                ..Default::default()
            };
            let job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: loop_id.to_string(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                generation: "generation-1".to_string(),
                dispatch_defaults: json!({ "backend": "long-provider" }),
                provider_catalog: catalog,
            })?;
            let submission = work_job_submission(
                &LoopWorkHandler,
                job.idempotency_key.clone(),
                job.to_checkpoint()?,
            )?;
            let work_request = submission["request"].clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = Arc::new(
                ControllerJobHarness::new(Arc::clone(&driver), work_request.clone())
                    .expect("construct provider harness"),
            );
            let prepared = driver.prepare(work_request).expect("prepare provider job");
            let thread_driver = Arc::clone(&driver);
            let thread_harness = Arc::clone(&harness);
            let execution = std::thread::spawn(move || {
                thread_driver
                    .execute(prepared, thread_harness.handle())
                    .expect("provider execution returns after cancellation")
            });

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let provider_run_id = loop {
                let active = agent_task_loop_controller::load_controller(loop_id)
                    .expect("controller exists")
                    .metadata
                    .get("active_provider_runs")
                    .and_then(Value::as_array)
                    .and_then(|runs| runs.first())
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(run_id) = active
                    .filter(|run_id| !homeboy_agents_run_is_not_running(run_id))
                    .filter(|_| marker_path.exists())
                {
                    break run_id;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "provider admission missing: controller={:?}; marker={}; checkpoint={:?}; events={:?}",
                    agent_task_loop_controller::load_controller(loop_id),
                    marker_path.exists(),
                    harness.checkpoint(),
                    harness.events()
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            assert_eq!(
                agent_task_loop_controller::load_controller(loop_id)?
                    .metadata["active_provider_run_lineage"][&provider_run_id]["generation"],
                "generation-1",
                "provider lineage must retain the admitted WorkJob generation"
            );
            let cancellation_started = std::time::Instant::now();
            harness
                .request_cancellation("cancel active provider")
                .expect("request cancellation");
            driver
                .cancel(
                    &harness
                        .checkpoint()
                        .expect("read checkpoint")
                        .expect("checkpoint exists"),
                )
                .expect("cancel active provider");
            let result = execution.join().expect("join provider execution");
            assert!(
                cancellation_started.elapsed() < Duration::from_secs(5),
                "provider cancellation did not interrupt the active execution"
            );
            assert_eq!(result["result"]["controller_state"], "abandoned");
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("active provider cancellation converges");
    }

    fn homeboy_agents_run_is_not_running(run_id: &str) -> bool {
        let Ok(store) =
            crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
        else {
            return true;
        };
        !store.read_record(run_id).is_ok_and(|record| {
            record.state == crate::agent_task_lifecycle::AgentTaskRunState::Running
        })
    }

    #[test]
    fn actionless_waiting_execution_completes_without_a_pending_action() {
        with_isolated_home(|_| {
            let mut record = agent_task_loop_controller::create_controller(
                "loop-waiting-execution",
                "repair",
                "v1",
            )
            .expect("create controller");
            record.state = AgentTaskLoopControllerState::Waiting;
            agent_task_loop_controller::write_controller(&record).expect("write waiting state");
            let mut job = AgentTaskLoopJob::new(AgentTaskLoopJobRequest {
                schema: AGENT_TASK_LOOP_JOB_SCHEMA.to_string(),
                loop_id: record.loop_id.clone(),
                kind: AgentTaskLoopJobKind::DaemonExecution,
                generation: "generation-1".to_string(),
                dispatch_defaults: json!({}),
                provider_catalog: AgentTaskProviderCatalog::default(),
            })?;

            let step = LoopWorkHandler.observe(&mut job, WorkJobInvocation::Execute)?;
            assert!(matches!(step, WorkJobStep::Complete(_)));
            assert_eq!(job.phase, WorkJobPhase::Completed);
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("actionless waiting execution completes");
    }

    #[test]
    fn failed_daemon_execution_terminalizes_through_the_shared_harness() {
        with_isolated_home(|_| {
            let mut record =
                agent_task_loop_controller::create_controller("loop-daemon-failed", "repair", "v1")
                    .expect("create controller");
            record.state = AgentTaskLoopControllerState::Failed;
            agent_task_loop_controller::write_controller(&record).expect("fail controller");
            let request = submission("loop-daemon-failed")["request"].clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let prepared = driver.prepare(request).expect("prepare loop work");

            let result = driver
                .execute(prepared, harness.handle())
                .expect("observe dead coordinator");

            assert_eq!(result["result"]["phase"], "completed");
            assert_eq!(result["result"]["controller_state"], "failed");
            assert_eq!(
                agent_task_loop_controller::load_controller("loop-daemon-failed")
                    .expect("read terminal controller")
                    .state,
                AgentTaskLoopControllerState::Failed
            );
            let checkpoint = harness
                .checkpoint()
                .expect("read checkpoint")
                .expect("loop supervision checkpoint");
            assert_eq!(checkpoint["schema"], WORK_JOB_CHECKPOINT_SCHEMA);
            assert_eq!(checkpoint["work_type"], AGENT_TASK_LOOP_JOB_TYPE);
        });
    }

    #[test]
    fn completed_loop_checkpoint_replays_without_reexecution() {
        with_isolated_home(|_| {
            let mut record =
                agent_task_loop_controller::create_controller("loop-complete", "repair", "v1")
                    .expect("create controller");
            record.state = AgentTaskLoopControllerState::Completed;
            agent_task_loop_controller::write_controller(&record).expect("complete controller");
            let request = submission("loop-complete")["request"].clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let mut checkpoint = driver.prepare(request).expect("prepare loop work");
            checkpoint["checkpoint"]["phase"] = json!("completed");
            harness
                .request_cancellation("cancel racing terminal replay")
                .expect("request cancellation");

            let first = driver
                .resume(checkpoint.clone(), harness.handle())
                .expect("first replay");
            let second = driver
                .resume(checkpoint, harness.handle())
                .expect("second replay");

            assert_eq!(first, second);
            assert_eq!(first["result"]["controller_state"], "completed");
            assert_eq!(
                agent_task_loop_controller::load_controller("loop-complete")
                    .expect("read terminal controller")
                    .state,
                AgentTaskLoopControllerState::Completed
            );
        });
    }

    #[test]
    fn interrupted_loop_child_admission_recovers_without_redispatch() {
        with_isolated_home(|_| {
            register_loop_work_job_handler();
            let marker = tempfile::NamedTempFile::new().expect("provider marker");
            let mut record = agent_task_loop_controller::create_controller(
                "loop-interrupted-admission",
                "repair",
                "v1",
            )
            .expect("create controller");
            record.record_action(
                agent_task_loop_controller::AgentTaskLoopPolicyAction::SpawnTask {
                    dedupe_key: "interrupted-child-admission".to_string(),
                    entity_id: None,
                    request: json!({
                        "mode": "dispatch",
                        "dispatch": { "backend": "interrupted-fixture", "prompt": "run" }
                    }),
                },
                "interrupted child admission",
            );
            agent_task_loop_controller::stamp_loop_runtime_metadata(
                &mut record.metadata,
                true,
                None,
                true,
            )?;
            agent_task_loop_controller::write_controller(&record).expect("write controller");
            let provider: crate::agent_task_provider::AgentTaskExecutorProvider =
                serde_json::from_value(json!({
                "id": "interrupted-fixture",
                "backend": "interrupted-fixture",
                "command_argv": [
                        "sh", "-c",
                        format!(
                        "printf x >> {}; task_id=$(cat | sed -n 's/.*\"task_id\":\"\\([^\"]*\\)\".*/\\1/p'); printf '{{\"schema\":\"homeboy/agent-task-outcome/v1\",\"task_id\":\"%s\",\"status\":\"succeeded\",\"artifacts\":[{{\"id\":\"patch\",\"kind\":\"patch\",\"name\":\"patch\",\"path\":\"{}\"}}]}}' \"$task_id\"",
                        marker.path().display(),
                        marker.path().display()
                    )
                ],
                "capabilities": ["structured_outcome"]
                }))
                .expect("provider fixture");
            let request = loop_work_job_execution_submission(
                "loop-interrupted-admission",
                "generation-1",
                json!({ "backend": "interrupted-fixture" }),
                AgentTaskProviderCatalog {
                    providers: vec![provider],
                    ..Default::default()
                },
            )?["request"]
                .clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let prepared = driver.prepare(request).expect("prepare work job");
            let interrupted = with_test_loop_checkpoint_interrupt(|| {
                driver.execute(prepared, harness.handle())
            });
            assert!(interrupted.is_err(), "fault hook must interrupt before checkpoint");
            assert_eq!(
                std::fs::read(marker.path()).expect("provider marker").len(),
                1
            );
            let checkpoint = harness
                .checkpoint()
                .expect("checkpoint")
                .expect("prepared checkpoint");
            assert!(checkpoint["checkpoint"]["resume"].is_null());

            let recovered = agent_task_loop_controller::load_controller(
                "loop-interrupted-admission",
            )?;
            assert_eq!(recovered.state, AgentTaskLoopControllerState::Running);
            let recovered_job = checkpoint["checkpoint"].clone();
            let step = LoopWorkHandler.advance(recovered_job, WorkJobInvocation::Resume)?;
            assert!(matches!(
                step,
                WorkJobStep::Continue { .. } | WorkJobStep::Complete(_)
            ));
            let recovered = agent_task_loop_controller::load_controller(
                "loop-interrupted-admission",
            )?;
            assert_eq!(
                recovered.next_actions[0].status,
                agent_task_loop_controller::AgentTaskLoopActionStatus::Completed,
                "recovered controller: {recovered:#?}"
            );
            assert!(recovered
                .dedupe_keys
                .contains_key("interrupted-child-admission"));
            assert_eq!(
                recovered.metadata["runtime"]["revolutions"],
                json!(1)
            );
            assert_eq!(
                std::fs::read(marker.path()).expect("provider marker after recovery").len(),
                1
            );
            Ok::<(), homeboy_core::Error>(())
        })
        .expect("interrupted admission recovery converges");
    }

    #[test]
    fn cancellation_preserves_a_controller_that_terminalized_while_child_lives() {
        with_isolated_home(|_| {
            register_loop_work_job_handler();
            let loop_id = "loop-cancel-racing-terminal";
            let mut record = agent_task_loop_controller::create_controller(loop_id, "repair", "v1")
                .expect("create controller");
            let request = loop_work_job_execution_submission(
                loop_id,
                "generation-cancel-racing",
                json!({}),
                AgentTaskProviderCatalog::default(),
            )
            .expect("build submission")["request"]
                .clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let prepared = driver.prepare(request).expect("prepare loop work");

            record.state = AgentTaskLoopControllerState::Completed;
            agent_task_loop_controller::write_controller(&record).expect("complete controller");
            harness
                .request_cancellation("cancel racing terminal controller")
                .expect("request cancellation");

            driver
                .cancel(&prepared)
                .expect("authoritative cancel recheck");
            let result = driver
                .resume(prepared, harness.handle())
                .expect("preserve terminal controller outcome");

            assert_eq!(result["result"]["phase"], "completed");
            assert_eq!(result["result"]["controller_state"], "completed");
        });
    }

    #[test]
    fn idle_ticks_do_not_repeat_checkpoint_or_progress_writes() {
        with_isolated_home(|_| {
            register_loop_work_job_handler();
            agent_task_loop_controller::create_controller("loop-idle", "repair", "v1")
                .expect("create controller");
            let request = loop_work_job_execution_submission(
                "loop-idle",
                "generation-idle",
                json!({}),
                AgentTaskProviderCatalog::default(),
            )
            .expect("build submission")["request"]
                .clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = Arc::new(
                ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                    .expect("construct work harness"),
            );
            let prepared = driver.prepare(request).expect("prepare loop work");
            let thread_harness = Arc::clone(&harness);
            let thread_driver = Arc::clone(&driver);
            let execution = std::thread::spawn(move || {
                thread_driver
                    .execute(prepared, thread_harness.handle())
                    .expect("execute idle loop");
            });

            // Reaching "idle" takes more than the two unconditional writes at
            // job start (queued status, then the first supervising progress):
            // the loop's first real tick still has to observe the controller
            // and discover there is no pending action, which is itself a
            // legitimate one-time `resume_result: None -> Some("idle")`
            // progress write before the loop is actually settled. Wait for the
            // event count to stop growing (rather than a fixed count) so this
            // baseline is whatever settling genuinely takes, and the assertion
            // below is only about ticks *after* that point.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let mut initial_events = harness.events().expect("read events").len();
            loop {
                assert!(
                    std::time::Instant::now() < deadline,
                    "initial events did not settle"
                );
                std::thread::sleep(Duration::from_millis(50));
                let count = harness.events().expect("read events").len();
                if count == initial_events && count >= 2 {
                    break;
                }
                initial_events = count;
            }
            std::thread::sleep(Duration::from_millis(650));
            assert_eq!(
                harness.events().expect("read idle events").len(),
                initial_events
            );

            harness
                .request_cancellation("stop idle loop")
                .expect("request cancellation");
            driver
                .cancel(
                    &harness
                        .checkpoint()
                        .expect("read checkpoint")
                        .expect("checkpoint"),
                )
                .expect("cancel coordinator");
            execution.join().expect("join loop execution");
        });
    }

    #[test]
    fn cancellation_through_the_shared_harness_stops_the_coordinator() {
        with_isolated_home(|_| {
            register_loop_work_job_handler();
            agent_task_loop_controller::create_controller("loop-cancel", "repair", "v1")
                .expect("create controller");
            let request = loop_work_job_execution_submission(
                "loop-cancel",
                "generation-cancel",
                json!({}),
                AgentTaskProviderCatalog::default(),
            )
            .expect("build submission")["request"]
                .clone();
            let driver: Arc<dyn ControllerJobDriver> = Arc::new(WorkJobDriver);
            let harness = ControllerJobHarness::new(Arc::clone(&driver), request.clone())
                .expect("construct work harness");
            let prepared = driver.prepare(request).expect("prepare loop work");
            harness
                .request_cancellation("test cancellation")
                .expect("request cancellation");

            driver
                .cancel(&prepared)
                .expect("cancel coordinator process tree");
            let result = driver
                .execute(prepared, harness.handle())
                .expect("terminalize cancelled loop work");

            assert_eq!(result["result"]["controller_state"], "abandoned");
        });
    }
}

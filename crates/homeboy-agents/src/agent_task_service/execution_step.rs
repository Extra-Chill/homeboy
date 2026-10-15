//! Agents-owned execution outbox. Submission inputs are frozen by the lifecycle
//! writer, never reconstructed by the daemon draining the outbox.
use std::path::Path;
use std::sync::{Arc, OnceLock};

use homeboy_core::daemon::step_job::{self, StepJobRequest, StepWait};
use homeboy_core::observation::WorkIntent;
use homeboy_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState};
use crate::agent_task_scheduler::AgentTaskPlan;

pub const EXECUTION_STEP_KIND: &str = "agent-task-execution-step";
pub const EXECUTION_STEP_CONSUMER: &str = "agent-task.execution-step";
const SCHEMA: &str = "homeboy/agent-task-execution-step/v1";
pub(crate) const PENDING_EXECUTION_INTENT: &str = "pending_execution_intent";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPreparation {
    pub cook_id: String,
    pub source_run_id: String,
    pub attempt: u32,
    pub replaces_source_attempt: bool,
    /// Digest of the source's immutable recipe attempt, not a mutable status row.
    pub source_recipe_revision: String,
    pub plan: AgentTaskPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionStepMode {
    Admission {
        admission_request: Value,
    },
    QueuedRun {
        retry_preparation: Option<RetryPreparation>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionStepIntent {
    pub schema: String,
    pub intent_id: String,
    pub run_id: String,
    pub subject_id: String,
    pub epoch: u64,
    pub recovery_attempt: u32,
    pub superseded_intent_id: Option<String>,
    pub not_before: Option<String>,
    pub runner: Option<String>,
    pub mode: ExecutionStepMode,
    pub step: StepJobRequest,
}

/// CLI supplies normal parsing and Lab transport; all intent/lifecycle decisions
/// remain in this service. A transport validates workspace input before claim.
pub trait ExecutionStepTransport {
    fn admission(&self, argv: Vec<String>, runner: Option<String>) -> Result<i32>;
    fn queued_preflight(
        &self,
        _record: &AgentTaskRunRecord,
        _plan: &mut AgentTaskPlan,
        _intent: &ExecutionStepIntent,
    ) -> Result<()> {
        Ok(())
    }
    fn dispatch_queued(
        &self,
        _record: &AgentTaskRunRecord,
        _plan: &AgentTaskPlan,
        _intent: &ExecutionStepIntent,
    ) -> Result<Option<i32>> {
        Ok(None)
    }
}

impl ExecutionStepIntent {
    pub fn parse(payload: &Value) -> Result<Self> {
        let intent: Self = serde_json::from_value(payload.clone())
            .map_err(|error| invalid(format!("invalid execution intent: {error}")))?;
        if intent.schema != SCHEMA
            || crate::agent_task_lifecycle::sanitize_run_id(&intent.subject_id) != intent.subject_id
            || intent.epoch == 0
            || intent.run_id != intent.subject_id
            || intent.intent_id
                != format!(
                    "agent-task-exec:{}:{}:{}",
                    intent.subject_id, intent.epoch, intent.recovery_attempt
                )
            || intent.step.key != intent.intent_id
            || intent.step.terminal_consumer != EXECUTION_STEP_CONSUMER
            || intent.step.linked_durable_run_id.as_deref() != Some(intent.subject_id.as_str())
            || intent.step.wait.not_before != intent.not_before
            || intent.step.wait.runner_eligible != intent.runner
            || intent.step.args
                != [
                    "agent-task",
                    "execution-step",
                    "--intent-id",
                    &intent.intent_id,
                ]
        {
            return Err(invalid("execution intent envelope does not match its step"));
        }
        intent.step.submission()?;
        Ok(intent)
    }

    pub fn authorizes(&self, record: &AgentTaskRunRecord) -> bool {
        record.run_id == self.subject_id
            && record.state == AgentTaskRunState::Queued
            && record.metadata["execution_scheduling"]["epoch"].as_u64() == Some(self.epoch)
            && record.metadata["execution_scheduling"]["current_intent_id"].as_str()
                == Some(self.intent_id.as_str())
            && record.metadata.get("queue_quarantine").is_none()
            && record.metadata["detached_cook_handoff"]["cancellation_fence"]["state"]
                != "cancelled"
            && record.metadata["detached_cook_handoff"]["cancellation_fence"]["state"]
                != "cancel_requested"
    }

    pub fn work_intent(&self) -> Result<WorkIntent> {
        let payload = serde_json::to_value(self)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        Self::parse(&payload)?;
        Ok(WorkIntent {
            id: self.intent_id.clone(),
            run_id: self.run_id.clone(),
            kind: EXECUTION_STEP_KIND.into(),
            version: 1,
            payload,
        })
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::validation_invalid_argument("execution_intent", message, None, None)
}

fn deterministic_store_error(error: &Error) -> bool {
    matches!(
        error.code,
        homeboy_core::ErrorCode::ValidationInvalidArgument
            | homeboy_core::ErrorCode::InternalJsonError
    )
}

type RunnerWaitTarget = dyn Fn() -> Result<Option<String>> + Send + Sync;
static RUNNER_WAIT_TARGET: OnceLock<Arc<RunnerWaitTarget>> = OnceLock::new();

/// Transport inventory adapter. Selection of a configured target is captured at
/// admission; the outbox scheduler never queries mutable runner configuration.
pub fn register_execution_step_runner_target(target: Arc<RunnerWaitTarget>) {
    let _ = RUNNER_WAIT_TARGET.set(target);
}

/// Prepare explicit work for the next lifecycle transaction. The store consumes
/// the private draft before serializing the record, leaving only the fence.
pub(crate) fn prepare_execution_intent(
    store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    not_before: Option<String>,
) -> Result<()> {
    let recovery_failures = record.metadata["execution_scheduling"]["recovery_failures"]
        .as_u64()
        .unwrap_or(0);
    let epoch = record.metadata["execution_scheduling"]["epoch"]
        .as_u64()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid("execution scheduling epoch exhausted"))?;
    let id = format!("agent-task-exec:{}:{epoch}:0", record.run_id);
    let previous = record.metadata["execution_scheduling"]["current_intent_id"]
        .as_str()
        .map(|id| load_execution_intent(store, id))
        .transpose()?;
    let admission = &record.metadata["unmaterialized_cook_admission"];
    let mode = if admission.is_object() {
        ExecutionStepMode::Admission {
            admission_request: admission["binding"].clone(),
        }
    } else {
        ExecutionStepMode::QueuedRun {
            retry_preparation: record
                .metadata
                .get("execution_retry_preparation")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| invalid(format!("invalid retry preparation: {error}")))?
                .or_else(|| {
                    previous.as_ref().and_then(|intent| match &intent.mode {
                        ExecutionStepMode::QueuedRun { retry_preparation } => {
                            retry_preparation.clone()
                        }
                        _ => None,
                    })
                }),
        }
    };
    let local = admission["binding"]["placement"]["requested"] == "local";
    let runner = if local {
        None
    } else {
        admission["binding"]["placement"]["runner_ref"]
            .as_str()
            .or_else(|| {
                record.metadata["execution_placement_decision"]["runner"]["runner_id"].as_str()
            })
            .filter(|runner| *runner != "local")
            .map(str::to_string)
    };
    let runner = if runner.is_none() && admission.is_object() && !local {
        RUNNER_WAIT_TARGET
            .get()
            .map(|target| target())
            .transpose()?
            .flatten()
    } else {
        runner
    };
    let not_before = not_before.or_else(|| {
        (admission.is_object() && !local && runner.is_none())
            .then(|| (chrono::Utc::now() + chrono::Duration::seconds(15)).to_rfc3339())
    });
    let (program, cwd, env) = if let Some(previous) = &previous {
        (
            previous.step.program.clone(),
            previous.step.cwd.clone(),
            previous.step.env.clone(),
        )
    } else {
        let program =
            std::env::current_exe().map_err(|error| Error::internal_io(error.to_string(), None))?;
        let cwd =
            std::env::current_dir().map_err(|error| Error::internal_io(error.to_string(), None))?;
        (
            program.display().to_string(),
            cwd.display().to_string(),
            super::admitted_cook_environment([
                (
                    "HOMEBOY_CONFIG_ROOT".into(),
                    store.roots().config().display().to_string(),
                ),
                (
                    "HOMEBOY_DATA_DIR".into(),
                    store.roots().data().display().to_string(),
                ),
                (
                    "HOMEBOY_ARTIFACT_ROOT".into(),
                    store.roots().artifacts().display().to_string(),
                ),
            ])?,
        )
    };
    let mut env = env;
    env.public_env.retain(|name, _| {
        ![
            "HOMEBOY_LOCAL_COOK_SUPERVISOR_JOB_ID",
            "HOMEBOY_COOK_STARTUP_LAUNCHER_ID",
            "HOMEBOY_COOK_PINNED_CONTROLLER_RUNTIME",
            "HOMEBOY_AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_JSON",
        ]
        .contains(&name.as_str())
    });
    let step = StepJobRequest::new(
        &id,
        program,
        vec![
            "agent-task".into(),
            "execution-step".into(),
            "--intent-id".into(),
            id.clone(),
        ],
        cwd,
        env,
        EXECUTION_STEP_CONSUMER,
    )
    .with_linked_durable_run_id(&record.run_id)
    .with_wait(StepWait {
        not_before: not_before.clone(),
        runner_eligible: runner.clone(),
    });
    let intent = ExecutionStepIntent {
        schema: SCHEMA.into(),
        intent_id: id.clone(),
        run_id: record.run_id.clone(),
        subject_id: record.run_id.clone(),
        epoch,
        recovery_attempt: 0,
        superseded_intent_id: previous.as_ref().map(|intent| intent.intent_id.clone()),
        not_before,
        runner,
        mode,
        step,
    };
    record
        .metadata
        .as_object_mut()
        .map(|metadata| metadata.remove("execution_retry_preparation"));
    record.metadata[PENDING_EXECUTION_INTENT] = serde_json::to_value(intent.work_intent()?)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    record.metadata["execution_scheduling"] =
        json!({"epoch": epoch, "current_intent_id": id, "recovery_failures": recovery_failures});
    Ok(())
}

pub fn register_execution_step_hooks() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        step_job::register_step_job_driver();
        step_job::register_step_terminal_consumer(
            EXECUTION_STEP_CONSUMER,
            Arc::new(consume_execution_terminal),
        );
        homeboy_core::daemon::orchestration::register_work_intent_scheduler(
            EXECUTION_STEP_KIND,
            1,
            Arc::new(|payload| {
                let client =
                    homeboy_core::daemon::LocalControllerJobClient::connect_current_build()?;
                schedule_execution_intent_with_cleanup(
                    payload,
                    |submission| Ok(client.submit(submission)?.id.to_string()),
                    // A predecessor that already finished is inert; only a
                    // live one needs cancelling.
                    |id| {
                        if client.status(id)?.status.is_terminal() {
                            return Ok(());
                        }
                        client.cancel(id, "execution intent superseded").map(|_| ())
                    },
                )
            }),
        )
        .expect("register agents execution intent scheduler");
    });
}

/// The delivery key, not the child exit code, is the lifecycle authority. Every
/// decision is re-evaluated under the lifecycle lock and the successor is part
/// of the same SQLite write as the recovery fence.
fn consume_execution_terminal(delivery: &step_job::StepTerminalDelivery) -> Result<Value> {
    let env = &delivery.request.env.public_env;
    let roots = [
        "HOMEBOY_CONFIG_ROOT",
        "HOMEBOY_DATA_DIR",
        "HOMEBOY_ARTIFACT_ROOT",
    ]
    .map(|name| env.get(name).map(std::path::PathBuf::from));
    let [Some(config), Some(data), Some(artifacts)] = roots else {
        return Ok(
            json!({"acknowledged": true, "diagnostic": "execution step has no rooted environment"}),
        );
    };
    let store =
        AgentTaskLifecycleStore::new(homeboy_core::paths::PathRoots::new(config, data, artifacts));
    consume_execution_terminal_in_store(&store, delivery)
}

pub(super) fn consume_execution_terminal_in_store(
    store: &AgentTaskLifecycleStore,
    delivery: &step_job::StepTerminalDelivery,
) -> Result<Value> {
    use crate::agent_task_lifecycle::{
        AgentTaskAdvance, AgentTaskTerminalOutcome, AgentTaskTransition,
    };
    use step_job::StepTerminalOutcome;
    let Some(subject) = delivery.linked_durable_run_id.as_deref() else {
        return Ok(json!({"acknowledged": true, "diagnostic": "missing execution subject"}));
    };
    if !store.record_exists(subject)? {
        return Ok(json!({"acknowledged": true, "disposition": "missing_subject"}));
    }
    if store.read_record(subject)?.metadata["execution_scheduling"]["current_intent_id"].as_str()
        != Some(&delivery.key)
    {
        return Ok(json!({"acknowledged": true, "disposition": "stale"}));
    }
    let Some(_subject_lock) = try_execution_subject_lock_in_store(store, subject)? else {
        // A terminal job is not authority to supersede another live exact
        // child. Its crash-released lock still owns the side-effect boundary.
        return Ok(json!({"acknowledged": true, "disposition": "active_owner"}));
    };
    // A diagnostic emitted by the parsing adapter is deterministic unless it
    // explicitly reports a retryable pre-execution failure. Missing output is
    // never proof of zero execution; that proof comes from the durable records.
    let command = super::cook_step::last_failed_command_result(&delivery.log_path);
    let diagnostic = command
        .as_ref()
        .and_then(|value| {
            value
                .get("diagnostics")
                .or_else(|| value.get("error"))
                .or_else(|| value.pointer("/data/failure_context/diagnostic"))
        })
        .cloned()
        .or_else(|| delivery.error.clone());
    let reason = diagnostic
        .as_ref()
        .and_then(|value| value.get("message").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "execution step {} ended before publication ({:?}, exit {:?})",
                delivery.key, delivery.outcome, delivery.exit_code
            )
        });
    let runner_ineligible = diagnostic.as_ref().is_some_and(|value| {
        value["details"]["execution_step_disposition"] == "runner_ineligible"
            || value["data"]["execution_step_disposition"] == "runner_ineligible"
    });
    let recoverable = runner_ineligible
        || delivery.outcome == StepTerminalOutcome::SpawnInterrupted
        || (delivery.outcome == StepTerminalOutcome::Exited && command.is_none())
        || (delivery.child.is_some() && command.is_none())
        || diagnostic.as_ref().is_none_or(|value| {
            value["retryable"] == true
                || value["data"]["retryable"] == true
                || value["details"]["classification"] == "retryable"
        });
    let mut failure = None;
    let mut disposition = "stale";
    let mut reconcile = None;
    let mut cancel_target = None;
    let committed = store.transition_when(subject, |stored| {
        if stored.metadata["execution_scheduling"]["current_intent_id"].as_str()
            != Some(&delivery.key)
            || (stored.state.is_terminal()
                && stored.metadata["detached_cook_handoff"]["state"] != "redirected")
        {
            return None;
        }
        if !delivery.cancelled && stored.metadata.get("queue_quarantine").is_some() {
            disposition = "quarantined";
            return None;
        }
        // Published admissions have transferred ownership to the reserved
        // attempt. Inspect it before permitting any pre-publication replay.
        let mut execution_record = stored.clone();
        if let Some(child) = stored.metadata["detached_cook_handoff"]
            ["materializing_attempt_run_id"]
            .as_str()
            .or_else(|| stored.metadata["detached_cook_handoff"]["attempt_run_id"].as_str())
        {
            match store.record_exists(child).and_then(|exists| {
                if exists {
                    store.read_record(child).map(Some)
                } else {
                    Ok(None)
                }
            }) {
                Ok(Some(record)) => execution_record = record,
                Ok(None) => {}
                Err(error) => {
                    failure = Some(error);
                    return None;
                }
            }
        }
        if delivery.cancelled
            && stored.state.is_terminal()
            && execution_record.run_id != stored.run_id
        {
            disposition = "cancelled";
            cancel_target = Some(execution_record.run_id.clone());
            return None;
        }
        if !delivery.cancelled
            && execution_record.run_id != stored.run_id
            && execution_record.metadata["execution_scheduling"]["current_intent_id"].is_string()
        {
            disposition = "published";
            return None;
        }
        let zero = super::cook_recipe::has_unambiguous_zero_execution(&execution_record);
        if !delivery.cancelled && !zero {
            disposition = "execution_owned";
            if execution_record.runner_job_id().is_none()
                && execution_record.lab_handoff.as_ref().is_none_or(|handoff| {
                    handoff.state != crate::agent_task_lifecycle::AgentTaskLabHandoffState::Accepted
                })
            {
                reconcile = Some(execution_record.run_id.clone());
            }
            return None;
        }
        if !delivery.cancelled && stored.metadata["detached_cook_handoff"]["state"] == "redirected"
        {
            // A published attempt already owns a deferral or its durable
            // terminal result. Never manufacture another initial admission.
            disposition = "published";
            if !execution_record.state.is_terminal()
                && !(execution_record.state == AgentTaskRunState::Queued
                    && execution_record.metadata["execution_scheduling"]["current_intent_id"]
                        .is_string())
            {
                reconcile = Some(execution_record.run_id.clone());
            }
            return None;
        }
        let failures = stored.metadata["execution_scheduling"]["recovery_failures"]
            .as_u64()
            .unwrap_or(0);
        if delivery.cancelled || !recoverable || (!runner_ineligible && failures + 1 >= 20) {
            disposition = if delivery.cancelled {
                "cancelled"
            } else if recoverable {
                "exhausted"
            } else {
                "failed"
            };
            let mut scheduling = stored.metadata["execution_scheduling"].clone();
            if recoverable && !delivery.cancelled {
                scheduling["recovery_failures"] = json!(failures + 1);
            }
            return Some(AgentTaskTransition::Terminate {
                outcome: if delivery.cancelled {
                    AgentTaskTerminalOutcome::Cancelled
                } else {
                    AgentTaskTerminalOutcome::Failed
                },
                metadata: serde_json::Map::from_iter([
                    ("execution_scheduling".into(), scheduling),
                    (
                        "execution_step_failure".into(),
                        homeboy_core::redaction::redact_json(&json!({
                            "message": reason, "diagnostic": diagnostic, "job_id": delivery.job_id,
                            "step_key": delivery.key, "classification": disposition,
                        })),
                    ),
                ]),
            });
        }
        let intent = match load_execution_intent(store, &delivery.key) {
            Ok(intent) => intent,
            Err(error) => {
                failure = Some(error);
                return None;
            }
        };
        let mut next = intent.clone();
        if runner_ineligible {
            let Some(epoch) = next.epoch.checked_add(1) else {
                failure = Some(invalid("execution scheduling epoch exhausted"));
                return None;
            };
            next.epoch = epoch;
            next.recovery_attempt = 0;
        } else {
            next.recovery_attempt += 1;
        }
        next.intent_id = format!(
            "agent-task-exec:{}:{}:{}",
            next.subject_id, next.epoch, next.recovery_attempt
        );
        next.not_before = Some(
            (chrono::Utc::now()
                + chrono::Duration::seconds(if runner_ineligible {
                    2
                } else {
                    15 * (1i64 << failures.min(5))
                }))
            .to_rfc3339(),
        );
        next.step.key = next.intent_id.clone();
        next.step.args[3] = next.intent_id.clone();
        next.step.wait.not_before = next.not_before.clone();
        let work = match next.work_intent().and_then(|work| {
            serde_json::to_value(work)
                .map_err(|error| Error::internal_json(error.to_string(), None))
        }) {
            Ok(work) => work,
            Err(error) => {
                failure = Some(error);
                return None;
            }
        };
        let mut prepared = stored.clone();
        prepared.updated_at = Some(chrono::Utc::now().to_rfc3339());
        crate::agent_task_lifecycle::set_run_state(&mut prepared, AgentTaskRunState::Queued);
        prepared.lifecycle.execution.started_at = None;
        prepared.lifecycle.execution.finished_at = None;
        for task in &mut prepared.tasks {
            task.state = crate::agent_tasks::AgentTaskState::Queued;
        }
        for key in [
            "execution_step_owner",
            "runner_pid",
            "runner_process_start_identity",
            "runner_started_at",
        ] {
            prepared
                .metadata
                .as_object_mut()
                .map(|metadata| metadata.remove(key));
        }
        prepared.metadata[PENDING_EXECUTION_INTENT] = work;
        prepared.metadata["execution_scheduling"]["current_intent_id"] = json!(next.intent_id);
        prepared.metadata["execution_scheduling"]["epoch"] = json!(next.epoch);
        prepared.metadata["execution_scheduling"]["recovery_failures"] =
            json!(failures + u64::from(!runner_ineligible));
        prepared.metadata["execution_step_failure"] = homeboy_core::redaction::redact_json(
            &json!({"message": reason, "diagnostic": diagnostic, "step_key": delivery.key}),
        );
        disposition = if runner_ineligible {
            "runner_wait"
        } else {
            "recovered"
        };
        Some(AgentTaskTransition::Advance {
            kind: AgentTaskAdvance::Requeue,
            prepared: Box::new(prepared),
        })
    });
    if let Err(error) = committed {
        if !deterministic_store_error(&error) {
            return Err(error);
        }
        failure = Some(error);
    }
    if let Some(error) = failure {
        if !deterministic_store_error(&error) {
            return Err(error);
        }
        store.transition_when(subject, |record| {
            (record.metadata["execution_scheduling"]["current_intent_id"].as_str() == Some(&delivery.key)
                && !record.state.is_terminal()).then(|| AgentTaskTransition::Terminate {
                outcome: AgentTaskTerminalOutcome::Failed,
                metadata: serde_json::Map::from_iter([("execution_step_failure".into(), homeboy_core::redaction::redact_json(&json!({
                    "message": error.message, "code": error.code.as_str(), "details": error.details,
                    "job_id": delivery.job_id, "step_key": delivery.key, "classification": "deterministic",
                })))]),
            })
        })?;
        disposition = "failed";
    }
    if let Some(run_id) = reconcile {
        if let Err(error) =
            crate::agent_task_lifecycle::record_interrupted_local_owner_in_store(store, &run_id)
        {
            if !deterministic_store_error(&error) {
                return Err(error);
            }
            // Broken immutable provenance is a terminal diagnostic, not a
            // poison delivery to retry forever. A new step owner still wins.
            store.transition_when(&run_id, |record| {
                let key = record.metadata["execution_scheduling"]["current_intent_id"].as_str();
                (!record.state.is_terminal() && key.is_none_or(|key| key == delivery.key))
                    .then(|| AgentTaskTransition::Terminate {
                        outcome: AgentTaskTerminalOutcome::Failed,
                        metadata: serde_json::Map::from_iter([("execution_step_failure".into(), homeboy_core::redaction::redact_json(&json!({
                            "message": error.message, "code": error.code.as_str(), "details": error.details,
                            "job_id": delivery.job_id, "step_key": delivery.key, "classification": "deterministic",
                        })))]),
                    })
            })?;
        }
    }
    if disposition == "cancelled" {
        crate::agent_task_lifecycle::cancel_reserved_detached_cook_handoff_attempt_if_cancelled_in_store(store, subject)?;
    }
    if let Some(run_id) = cancel_target {
        crate::agent_task_lifecycle::cancel_run_in_store(
            store,
            &run_id,
            Some("execution step cancelled"),
        )?;
    }
    Ok(json!({"acknowledged": true, "job_id": delivery.job_id, "disposition": disposition}))
}

pub(crate) fn schedule_execution_intent_with(
    payload: &Value,
    submit: impl FnOnce(Value) -> Result<String>,
) -> Result<Value> {
    let intent = ExecutionStepIntent::parse(payload)?;
    let job_id = submit(intent.step.submission()?)?;
    Ok(json!({"scheduled": true, "job_id": job_id}))
}

/// Replacement admission and predecessor cancellation share outbox custody:
/// if cancellation loses its response, the source stays pending and redelivery
/// reuses both immutable submission keys. Epoch authorization already makes a
/// racing predecessor inert before this cross-store cleanup can run.
pub(super) fn schedule_execution_intent_with_cleanup(
    payload: &Value,
    mut submit: impl FnMut(Value) -> Result<String>,
    mut cancel: impl FnMut(&str) -> Result<()>,
) -> Result<Value> {
    let intent = ExecutionStepIntent::parse(payload)?;
    let receipt = schedule_execution_intent_with(payload, &mut submit)?;
    if let Some(previous) = &intent.superseded_intent_id {
        let env = &intent.step.env.public_env;
        let root = |name: &str| {
            env.get(name)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| invalid("replacement execution intent has no rooted environment"))
        };
        let store = AgentTaskLifecycleStore::new(homeboy_core::paths::PathRoots::new(
            root("HOMEBOY_CONFIG_ROOT")?,
            root("HOMEBOY_DATA_DIR")?,
            root("HOMEBOY_ARTIFACT_ROOT")?,
        ));
        let previous = load_execution_intent(&store, previous)?;
        if previous.subject_id != intent.subject_id || previous.epoch >= intent.epoch {
            return Err(invalid(
                "replacement does not supersede an earlier subject epoch",
            ));
        }
        let job = submit(previous.step.submission()?)?;
        cancel(&job)?;
    }
    Ok(receipt)
}

/// Exact lookup is rooted in the lifecycle store and works after source ACK.
pub fn load_execution_intent(
    store: &AgentTaskLifecycleStore,
    id: &str,
) -> Result<ExecutionStepIntent> {
    let work = store
        .open_observation_readonly()?
        .work_intent_by_id(id)?
        .ok_or_else(|| invalid("execution intent is unavailable"))?;
    if work.kind != EXECUTION_STEP_KIND || work.version != 1 {
        return Err(invalid("unsupported execution intent kind/version"));
    }
    let intent = ExecutionStepIntent::parse(&work.payload)?;
    if work.id != intent.intent_id || work.run_id != intent.run_id {
        return Err(invalid("execution intent identity mismatch"));
    }
    Ok(intent)
}

pub fn execute_execution_step(
    id: &str,
    executor: crate::agent_task_scheduler::SharedAgentTaskExecutor,
    dispatcher: impl Fn(&Value) -> Result<Option<Arc<dyn super::AgentTaskCookAttemptDispatcher>>>,
    transport: &impl ExecutionStepTransport,
) -> Result<i32> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    let intent = load_execution_intent(&store, id)?;
    if !intent.authorizes(&store.read_record(&intent.subject_id)?) {
        return Ok(0);
    }
    let Some(_subject_lock) = try_execution_subject_lock_in_store(&store, &intent.subject_id)?
    else {
        return Ok(0);
    };
    if !intent.authorizes(&store.read_record(&intent.subject_id)?) {
        return Ok(0);
    }
    #[cfg(feature = "test-support")]
    homeboy_core::test_support::wait_at_file_gate("HOMEBOY_TEST_EXECUTION_STEP_GATE")?;
    let Some(_generation) = pin_execution_generation_or_defer(&store, &intent.subject_id)? else {
        return Ok(75);
    };
    match &intent.mode {
        ExecutionStepMode::Admission { admission_request } => {
            let record = crate::agent_task_lifecycle::recover_unmaterialized_cook_input_publication_in_store(&store, &intent.subject_id)?;
            if !intent.authorizes(&record) {
                return Ok(0);
            }
            let argv = prepare_admission_arguments(admission_request, &intent.subject_id)?;
            let claimed = store.claim_execution_admission(&intent)?;
            if !claimed {
                return Ok(0);
            }
            transport.admission(argv, intent.runner.clone())
        }
        ExecutionStepMode::QueuedRun { retry_preparation } => {
            if let Some(preparation) = retry_preparation {
                super::execution::prepare_reserved_cook_retry(
                    &store,
                    &intent.subject_id,
                    preparation,
                )?;
            }
            super::execution::execute_exact_queued_intent(
                &store, &intent, executor, dispatcher, transport,
            )
        }
    }
}

/// Pin before either adapter can provision transport/workspace/provider input.
/// Holding this guard through execution makes the lock authoritative rather
/// than a readiness probe followed by an unprotected provisioning window.
pub(super) fn pin_execution_generation_or_defer(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<homeboy_core::runtime_promotion::RuntimeGenerationPinGuard>> {
    let record = store.read_record(run_id)?;
    match homeboy_core::runtime_promotion::try_pin_cook_generation(
        run_id,
        &record.metadata["controller_runtime"],
    ) {
        Ok(pin) => Ok(Some(pin)),
        Err(error)
            if error.code == homeboy_core::ErrorCode::RuntimePromotionWaitTimeout
                && error.details["wait_stage"] == "os_lock" =>
        {
            let owner = json!({
                "pid": error.details["holder_pid"], "operation": error.details["holder_operation"],
                "operation_id": error.details["holder_operation_id"], "status_command": error.details["holder_status_command"],
                "target": error.details["target"], "generation": error.details["holder_generation"],
            });
            if crate::agent_task_lifecycle::defer_cook_runtime_admission_in_store(
                store, run_id, owner,
            )? {
                Ok(None)
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

/// Crash-released subject ownership, shared with explicit rearm/placement.
/// A live child is never superseded by a timeout or a blocking writer.
pub(crate) fn try_execution_subject_lock_in_store(
    store: &AgentTaskLifecycleStore,
    subject: &str,
) -> Result<Option<std::fs::File>> {
    use fs4::fs_std::FileExt;
    let root = store.data_root().join("agent-task-execution-locks");
    std::fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    let path = root.join(format!(
        "{}.lock",
        crate::agent_task_lifecycle::sanitize_run_id(subject)
    ));
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    match lock.try_lock_exclusive() {
        Ok(true) => Ok(Some(lock)),
        Ok(false) => Ok(None),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}

/// Validate immutable input hashes/manifest and expand recorded content tokens
/// before the parsing/transport adapter sees a Cook request.
pub fn prepare_admission_arguments(binding: &Value, cook_id: &str) -> Result<Vec<String>> {
    use sha2::{Digest, Sha256};
    #[derive(Deserialize, Serialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct InputRef {
        kind: String,
        path: String,
        sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argv_token: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Inputs {
        schema: String,
        cook_id: String,
        argv: Vec<String>,
        input_refs: Vec<InputRef>,
        input_manifest: InputRef,
    }
    let inputs: Inputs = serde_json::from_value(binding["admission_inputs"].clone())
        .map_err(|error| invalid(format!("invalid admission inputs: {error}")))?;
    if inputs.schema != "homeboy/cook-admission-inputs/v1" || inputs.cook_id != cook_id {
        return Err(invalid("admission inputs do not match their subject"));
    }
    let mut verified = Vec::new();
    for reference in inputs
        .input_refs
        .iter()
        .chain(std::iter::once(&inputs.input_manifest))
    {
        if !Path::new(&reference.path).is_absolute() {
            return Err(invalid("admission input path must be absolute"));
        }
        let bytes = std::fs::read(&reference.path)
            .map_err(|error| invalid(format!("admission input unavailable: {error}")))?;
        let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
        if digest != reference.sha256 {
            return Err(invalid("admission input changed after acceptance"));
        }
        verified.push(bytes);
    }
    let manifest: Vec<InputRef> =
        serde_json::from_slice(&verified.pop().expect("verified manifest"))
            .map_err(|error| invalid(error.to_string()))?;
    if manifest != inputs.input_refs {
        return Err(invalid("admission manifest does not match frozen inputs"));
    }
    let mut argv = inputs.argv;
    for (reference, bytes) in inputs.input_refs.into_iter().zip(verified) {
        if let Some(token) = reference.argv_token {
            let content = String::from_utf8(bytes).map_err(|error| invalid(error.to_string()))?;
            for argument in &mut argv {
                if argument == &token {
                    *argument = content.clone();
                } else if let Some(flag) = argument
                    .strip_suffix(&token)
                    .filter(|flag| flag.ends_with('='))
                {
                    *argument = format!("{flag}{content}");
                }
            }
        }
    }
    let run_ids = argv
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| {
            argument.strip_prefix("--run-id=").or_else(|| {
                (argument == "--run-id")
                    .then(|| argv.get(index + 1).map(String::as_str))
                    .flatten()
            })
        })
        .collect::<Vec<_>>();
    if run_ids.as_slice() != [cook_id] {
        return Err(invalid(
            "admission argv must select its exact Cook subject once",
        ));
    }
    Ok(argv)
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn inputs(root: &Path) -> Value {
        let prompt = root.join("prompt");
        std::fs::write(&prompt, "frozen mission").unwrap();
        let refs = json!([{"kind": "prompt", "path": prompt, "sha256": format!("sha256:{:x}", Sha256::digest(b"frozen mission")), "argv_token": "homeboy-admission-ref:0"}]);
        let bytes = serde_json::to_vec(&refs).unwrap();
        let manifest = root.join("manifest");
        std::fs::write(&manifest, &bytes).unwrap();
        json!({"admission_inputs": {
            "schema": "homeboy/cook-admission-inputs/v1", "cook_id": "subject",
            "argv": ["homeboy", "agent-task", "cook", "--run-id", "subject", "--prompt=homeboy-admission-ref:0"],
            "input_refs": refs,
            "input_manifest": {"kind": "manifest", "path": manifest, "sha256": format!("sha256:{:x}", Sha256::digest(&bytes))}
        }})
    }

    #[test]
    fn execution_step_inputs_expand_only_verified_frozen_content() {
        let root = tempfile::tempdir().unwrap();
        let binding = inputs(root.path());
        let argv = prepare_admission_arguments(&binding, "subject").unwrap();
        assert_eq!(argv.last().unwrap(), "--prompt=frozen mission");
        std::fs::write(root.path().join("prompt"), "changed mission").unwrap();
        assert!(prepare_admission_arguments(&binding, "subject")
            .unwrap_err()
            .message
            .contains("changed after acceptance"));
    }

    #[test]
    fn execution_step_inputs_reject_manifest_and_subject_substitution() {
        let root = tempfile::tempdir().unwrap();
        let mut binding = inputs(root.path());
        assert!(prepare_admission_arguments(&binding, "another-subject").is_err());
        binding["admission_inputs"]["argv"][4] = json!("another-subject");
        assert!(prepare_admission_arguments(&binding, "subject")
            .unwrap_err()
            .message
            .contains("exact Cook subject"));
        binding["admission_inputs"]["argv"][4] = json!("subject");
        binding["admission_inputs"]["input_refs"][0]["argv_token"] = json!("substituted-token");
        assert!(prepare_admission_arguments(&binding, "subject")
            .unwrap_err()
            .message
            .contains("manifest does not match"));
    }
}

//! Shared controller-job lifecycle for agent-task orchestration work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use homeboy_core::daemon::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobHandle, ControllerJobPublicError,
};
use homeboy_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const WORK_JOB_TYPE: &str = "work";
pub const WORK_JOB_VERSION: u32 = 1;

/// Supervision phase of one detached orchestration child.
///
/// Cook, fanout, and loop all supervise a detached child through the same three
/// states, so they share this projection rather than restating it. The wire
/// form is `queued` / `supervising` / `completed`, unchanged from the
/// per-domain enums this replaces.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkJobPhase {
    /// Admitted, not yet supervising.
    #[default]
    Queued,
    /// The daemon is watching a live detached child.
    Supervising,
    /// The child ended and its durable outcome was observed.
    Completed,
}

/// Whether a supervised detached child is still the exact process the job
/// admitted. A recycled PID is not the same child, so start identity is part of
/// the question rather than an optional refinement.
pub(crate) fn supervised_child_is_live(
    child_pid: u32,
    child_start_identity: &homeboy_core::process::ProcessStartIdentity,
) -> bool {
    matches!(
        homeboy_core::process::process_identity_state_with_start_identity(
            child_pid,
            None,
            Some(child_start_identity),
        ),
        homeboy_core::process::ProcessIdentityState::Live
    )
}
pub(crate) const WORK_JOB_REQUEST_SCHEMA: &str = "homeboy/work-job-request/v1";
pub(crate) const WORK_JOB_CHECKPOINT_SCHEMA: &str = "homeboy/work-job-checkpoint/v1";
pub(crate) const WORK_JOB_PROGRESS_SCHEMA: &str = "homeboy/work-job-progress/v1";
pub(crate) const WORK_JOB_RESULT_SCHEMA: &str = "homeboy/work-job-result/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkJobRequest {
    pub schema: String,
    pub work_type: String,
    pub work_version: u32,
    pub request: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkJobCheckpoint {
    pub schema: String,
    pub work_type: String,
    pub work_version: u32,
    pub checkpoint: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WorkJobProgress {
    schema: String,
    work_type: String,
    work_version: u32,
    progress: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WorkJobResult {
    schema: String,
    work_type: String,
    work_version: u32,
    result: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkJobInvocation {
    Execute,
    Resume,
}

pub(crate) enum WorkJobStep {
    Continue {
        checkpoint: Value,
        progress: Value,
        wait: Duration,
    },
    Complete(Value),
}

/// Built-in orchestration adapter behind the generic lifecycle driver.
pub(crate) trait WorkJobHandler: Send + Sync {
    fn work_type(&self) -> &'static str;
    fn version(&self) -> u32;
    fn linked_durable_run_id(&self, _request: &Value) -> Option<String> {
        None
    }
    fn public_request(&self, request: &Value) -> Result<Value>;
    fn public_progress(&self, progress: &Value) -> Result<Value>;
    fn public_result(&self, result: &Value) -> Result<Value>;
    fn validate_secret_references(&self, request: &Value) -> Result<()>;
    fn prepare(&self, request: Value) -> Result<Value>;
    fn initial_progress(&self, checkpoint: &Value) -> Result<Value>;
    fn terminal_result(&self, checkpoint: &Value) -> Result<Option<Value>>;
    fn advance(&self, checkpoint: Value, invocation: WorkJobInvocation) -> Result<WorkJobStep>;
    fn cancelled(&self, checkpoint: Value) -> Result<Value>;
    fn cancel(&self, checkpoint: &Value) -> Result<()>;
}

/// Internal projection surface used by the shared driver.
#[derive(Clone)]
pub(crate) struct WorkJobHandle {
    inner: ControllerJobHandle,
    work_type: &'static str,
    work_version: u32,
}

impl WorkJobHandle {
    fn versioned(inner: ControllerJobHandle, handler: &dyn WorkJobHandler) -> Self {
        Self {
            inner,
            work_type: handler.work_type(),
            work_version: handler.version(),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn progress(&self, progress: Value) -> Result<()> {
        self.inner.progress(to_value(WorkJobProgress {
            schema: WORK_JOB_PROGRESS_SCHEMA.to_string(),
            work_type: self.work_type.to_string(),
            work_version: self.work_version,
            progress,
        })?)
    }

    fn checkpoint(&self, checkpoint: Value) -> Result<()> {
        self.inner.checkpoint(to_value(WorkJobCheckpoint {
            schema: WORK_JOB_CHECKPOINT_SCHEMA.to_string(),
            work_type: self.work_type.to_string(),
            work_version: self.work_version,
            checkpoint,
        })?)
    }
}

type WorkJobHandlerKey = (String, u32);
type WorkJobHandlers = Mutex<HashMap<WorkJobHandlerKey, Arc<dyn WorkJobHandler>>>;

fn handlers() -> &'static WorkJobHandlers {
    static HANDLERS: std::sync::OnceLock<WorkJobHandlers> = std::sync::OnceLock::new();
    HANDLERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn register_work_job_handler(handler: Arc<dyn WorkJobHandler>) -> Result<()> {
    let key = (handler.work_type().to_string(), handler.version());
    let mut registry = handlers().lock().expect("work job handler lock");
    if registry.contains_key(&key) {
        return Err(Error::validation_invalid_argument(
            "work_job_handler",
            format!(
                "work job handler `{}` version {} is already registered",
                key.0, key.1
            ),
            Some(key.0),
            None,
        ));
    }
    registry.insert(key, handler);
    Ok(())
}

fn handler(work_type: &str, version: u32) -> Result<Arc<dyn WorkJobHandler>> {
    handlers()
        .lock()
        .expect("work job handler lock")
        .get(&(work_type.to_string(), version))
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "work_type",
                format!("no work job handler is registered for `{work_type}` version {version}"),
                Some(work_type.to_string()),
                None,
            )
        })
}

pub struct WorkJobDriver;

impl ControllerJobDriver for WorkJobDriver {
    fn job_type(&self) -> &'static str {
        WORK_JOB_TYPE
    }

    fn version(&self) -> u32 {
        WORK_JOB_VERSION
    }

    fn linked_durable_run_id(&self, request: &Value) -> Option<String> {
        let request = parse_request(request.clone()).ok()?;
        handler(&request.work_type, request.work_version)
            .ok()?
            .linked_durable_run_id(&request.request)
            .filter(|run_id| !run_id.trim().is_empty())
    }

    fn public_request(&self, request: &Value) -> Result<Value> {
        let request = parse_request(request.clone())?;
        handler(&request.work_type, request.work_version)?.public_request(&request.request)
    }

    fn public_progress(&self, progress: &Value) -> Result<Value> {
        let progress: WorkJobProgress = parse_value(progress.clone(), "work progress")?;
        validate_schema(&progress.schema, WORK_JOB_PROGRESS_SCHEMA, "work progress")?;
        handler(&progress.work_type, progress.work_version)?.public_progress(&progress.progress)
    }

    fn public_result(&self, result: &Value) -> Result<Value> {
        let result: WorkJobResult = parse_value(result.clone(), "work result")?;
        validate_schema(&result.schema, WORK_JOB_RESULT_SCHEMA, "work result")?;
        handler(&result.work_type, result.work_version)?.public_result(&result.result)
    }

    fn public_error(&self, error: &Error) -> ControllerJobPublicError {
        ControllerJobPublicError {
            message: "controller-owned work failed".to_string(),
            data: json!({ "code": format!("{:?}", error.code) }),
        }
    }

    fn validate_secret_references(&self, request: &Value) -> Result<()> {
        let request = parse_request(request.clone())?;
        handler(&request.work_type, request.work_version)?
            .validate_secret_references(&request.request)
    }

    fn prepare(&self, request: Value) -> Result<Value> {
        let request = parse_request(request)?;
        let prepared =
            handler(&request.work_type, request.work_version)?.prepare(request.request)?;
        to_value(WorkJobCheckpoint {
            schema: WORK_JOB_CHECKPOINT_SCHEMA.to_string(),
            work_type: request.work_type,
            work_version: request.work_version,
            checkpoint: prepared,
        })
    }

    fn execute(&self, prepared: Value, handle: ControllerJobHandle) -> Result<Value> {
        self.supervise(prepared, handle, WorkJobInvocation::Execute)
    }

    fn resume(&self, checkpoint: Value, handle: ControllerJobHandle) -> Result<Value> {
        self.supervise(checkpoint, handle, WorkJobInvocation::Resume)
    }

    fn cancel(&self, prepared: &Value) -> Result<()> {
        let checkpoint = parse_checkpoint(prepared.clone())?;
        handler(&checkpoint.work_type, checkpoint.work_version)?.cancel(&checkpoint.checkpoint)
    }
}

impl WorkJobDriver {
    fn supervise(
        &self,
        prepared: Value,
        handle: ControllerJobHandle,
        invocation: WorkJobInvocation,
    ) -> Result<Value> {
        let checkpoint = parse_checkpoint(prepared)?;
        let handler = handler(&checkpoint.work_type, checkpoint.work_version)?;
        if let Some(result) = handler.terminal_result(&checkpoint.checkpoint)? {
            return to_value(WorkJobResult {
                schema: WORK_JOB_RESULT_SCHEMA.to_string(),
                work_type: handler.work_type().to_string(),
                work_version: handler.version(),
                result,
            });
        }
        let work_handle = WorkJobHandle::versioned(handle, handler.as_ref());
        let mut checkpoint = checkpoint.checkpoint;
        let mut progress = handler.initial_progress(&checkpoint)?;
        work_handle.checkpoint(checkpoint.clone())?;
        work_handle.progress(progress.clone())?;
        loop {
            if work_handle.is_cancelled() {
                handler.cancel(&checkpoint)?;
                let result = handler.cancelled(checkpoint)?;
                return to_value(WorkJobResult {
                    schema: WORK_JOB_RESULT_SCHEMA.to_string(),
                    work_type: handler.work_type().to_string(),
                    work_version: handler.version(),
                    result,
                });
            }
            match handler.advance(checkpoint.clone(), invocation)? {
                WorkJobStep::Complete(result) => {
                    return to_value(WorkJobResult {
                        schema: WORK_JOB_RESULT_SCHEMA.to_string(),
                        work_type: handler.work_type().to_string(),
                        work_version: handler.version(),
                        result,
                    });
                }
                WorkJobStep::Continue {
                    checkpoint: next,
                    progress: next_progress,
                    wait,
                } => {
                    if next != checkpoint {
                        work_handle.checkpoint(next.clone())?;
                    }
                    if next_progress != progress {
                        work_handle.progress(next_progress.clone())?;
                    }
                    checkpoint = next;
                    progress = next_progress;
                    std::thread::sleep(wait);
                }
            }
        }
    }
}

pub fn register_work_job_driver() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        controller_job_driver::register_controller_job_driver(Arc::new(WorkJobDriver))
            .expect("register work controller job driver");
    });
}

pub(crate) fn work_job_submission(
    handler: &dyn WorkJobHandler,
    idempotency_key: String,
    request: Value,
) -> Result<Value> {
    handler.validate_secret_references(&request)?;
    let request = WorkJobRequest {
        schema: WORK_JOB_REQUEST_SCHEMA.to_string(),
        work_type: handler.work_type().to_string(),
        work_version: handler.version(),
        request,
    };
    Ok(json!({
        "type": WORK_JOB_TYPE,
        "version": WORK_JOB_VERSION,
        "idempotency_key": idempotency_key,
        "request": to_value(request)?,
    }))
}

fn parse_request(value: Value) -> Result<WorkJobRequest> {
    let request: WorkJobRequest = parse_value(value, "work request")?;
    validate_schema(&request.schema, WORK_JOB_REQUEST_SCHEMA, "work request")?;
    if request.work_type.trim().is_empty() {
        return Err(invalid_work_job("work requests require a work type"));
    }
    Ok(request)
}

fn parse_checkpoint(value: Value) -> Result<WorkJobCheckpoint> {
    let checkpoint: WorkJobCheckpoint = parse_value(value, "work checkpoint")?;
    validate_schema(
        &checkpoint.schema,
        WORK_JOB_CHECKPOINT_SCHEMA,
        "work checkpoint",
    )?;
    Ok(checkpoint)
}

fn validate_schema(actual: &str, expected: &str, context: &str) -> Result<()> {
    if actual != expected {
        return Err(invalid_work_job(&format!(
            "{context} requires recognized schema `{expected}`"
        )));
    }
    Ok(())
}

fn parse_value<T: for<'de> Deserialize<'de>>(value: Value, context: &str) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|error| invalid_work_job(&format!("invalid durable {context}: {error}")))
}

fn to_value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize work job state".to_string()),
        )
    })
}

fn invalid_work_job(message: &str) -> Error {
    Error::validation_invalid_argument("work_job", message, None, None)
}

#[cfg(test)]
mod work_job_phase_tests {
    use super::WorkJobPhase;

    /// Cook, fanout, and loop job records are durable. Collapsing their three
    /// identical phase enums into one is only safe while the wire form stays
    /// exactly what those records already contain.
    #[test]
    fn phase_wire_form_matches_the_persisted_per_domain_encoding() {
        for (phase, encoded) in [
            (WorkJobPhase::Queued, "\"queued\""),
            (WorkJobPhase::Supervising, "\"supervising\""),
            (WorkJobPhase::Completed, "\"completed\""),
        ] {
            assert_eq!(serde_json::to_string(&phase).expect("encode"), encoded);
            assert_eq!(
                serde_json::from_str::<WorkJobPhase>(encoded).expect("decode"),
                phase
            );
        }
    }

    /// A record written before the phase field existed still loads as queued.
    #[test]
    fn absent_phase_defaults_to_queued() {
        assert_eq!(WorkJobPhase::default(), WorkJobPhase::Queued);
    }
}

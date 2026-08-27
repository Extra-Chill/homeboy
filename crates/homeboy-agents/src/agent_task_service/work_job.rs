//! Shared controller-job lifecycle for agent-task orchestration work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use homeboy_core::daemon::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobHandle, ControllerJobPublicError,
};
use homeboy_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const WORK_JOB_TYPE: &str = "work";
pub const WORK_JOB_VERSION: u32 = 1;
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
pub enum WorkJobInvocation {
    Execute,
    Resume,
}

/// Domain behavior behind the one generic orchestration lifecycle driver.
///
/// Implementations receive only [`WorkJobHandle`], so they can publish domain
/// checkpoints and progress but cannot mutate generic controller-job state.
pub trait WorkJobHandler: Send + Sync {
    fn work_type(&self) -> &'static str;
    fn version(&self) -> u32;
    fn public_request(&self, request: &Value) -> Result<Value>;
    fn public_progress(&self, progress: &Value) -> Result<Value>;
    fn public_result(&self, result: &Value) -> Result<Value>;
    fn public_error(&self, error: &Error) -> ControllerJobPublicError;
    fn validate_secret_references(&self, request: &Value) -> Result<()>;
    fn prepare(&self, request: Value) -> Result<Value>;
    fn advance(
        &self,
        checkpoint: Value,
        handle: WorkJobHandle,
        invocation: WorkJobInvocation,
    ) -> Result<Value>;
    fn cancel(&self, checkpoint: &Value) -> Result<()>;
}

#[derive(Clone, Copy)]
enum WorkJobFraming {
    Versioned,
    Legacy,
}

/// The bounded event surface available to domain handlers.
#[derive(Clone)]
pub struct WorkJobHandle {
    inner: ControllerJobHandle,
    work_type: &'static str,
    work_version: u32,
    framing: WorkJobFraming,
}

impl WorkJobHandle {
    fn versioned(inner: ControllerJobHandle, handler: &dyn WorkJobHandler) -> Self {
        Self {
            inner,
            work_type: handler.work_type(),
            work_version: handler.version(),
            framing: WorkJobFraming::Versioned,
        }
    }

    pub(crate) fn legacy(inner: ControllerJobHandle, handler: &dyn WorkJobHandler) -> Self {
        Self {
            inner,
            work_type: handler.work_type(),
            work_version: handler.version(),
            framing: WorkJobFraming::Legacy,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    pub fn job_id(&self) -> String {
        self.inner.job_id()
    }

    pub fn progress(&self, progress: Value) -> Result<()> {
        match self.framing {
            WorkJobFraming::Versioned => self.inner.progress(to_value(WorkJobProgress {
                schema: WORK_JOB_PROGRESS_SCHEMA.to_string(),
                work_type: self.work_type.to_string(),
                work_version: self.work_version,
                progress,
            })?),
            WorkJobFraming::Legacy => self.inner.progress(progress),
        }
    }

    pub fn checkpoint(&self, checkpoint: Value) -> Result<()> {
        match self.framing {
            WorkJobFraming::Versioned => self.inner.checkpoint(to_value(WorkJobCheckpoint {
                schema: WORK_JOB_CHECKPOINT_SCHEMA.to_string(),
                work_type: self.work_type.to_string(),
                work_version: self.work_version,
                checkpoint,
            })?),
            WorkJobFraming::Legacy => self.inner.checkpoint(checkpoint),
        }
    }
}

type WorkJobHandlerKey = (String, u32);
type WorkJobHandlers = Mutex<HashMap<WorkJobHandlerKey, Arc<dyn WorkJobHandler>>>;

fn handlers() -> &'static WorkJobHandlers {
    static HANDLERS: std::sync::OnceLock<WorkJobHandlers> = std::sync::OnceLock::new();
    HANDLERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_work_job_handler(handler: Arc<dyn WorkJobHandler>) -> Result<()> {
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
        self.advance(prepared, handle, WorkJobInvocation::Execute)
    }

    fn resume(&self, checkpoint: Value, handle: ControllerJobHandle) -> Result<Value> {
        self.advance(checkpoint, handle, WorkJobInvocation::Resume)
    }

    fn cancel(&self, prepared: &Value) -> Result<()> {
        let checkpoint = parse_checkpoint(prepared.clone())?;
        handler(&checkpoint.work_type, checkpoint.work_version)?.cancel(&checkpoint.checkpoint)
    }
}

impl WorkJobDriver {
    fn advance(
        &self,
        prepared: Value,
        handle: ControllerJobHandle,
        invocation: WorkJobInvocation,
    ) -> Result<Value> {
        let checkpoint = parse_checkpoint(prepared)?;
        let handler = handler(&checkpoint.work_type, checkpoint.work_version)?;
        let result = handler.advance(
            checkpoint.checkpoint,
            WorkJobHandle::versioned(handle, handler.as_ref()),
            invocation,
        )?;
        to_value(WorkJobResult {
            schema: WORK_JOB_RESULT_SCHEMA.to_string(),
            work_type: checkpoint.work_type,
            work_version: checkpoint.work_version,
            result,
        })
    }
}

pub fn register_work_job_driver() {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    REGISTERED.get_or_init(|| {
        controller_job_driver::register_controller_job_driver(Arc::new(WorkJobDriver))
            .expect("register work controller job driver");
    });
}

pub fn work_job_submission(
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

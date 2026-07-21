//! Typed controller-local work registered by domain crates.
//!
//! The daemon owns the durable job lifecycle. Drivers only interpret their
//! versioned request and report progress through the supplied job handle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::api_jobs::JobHandle;
use crate::error::{Error, Result};

/// A driver-owned error projection safe for durable public job state.
#[derive(Debug, Clone)]
pub struct ControllerJobPublicError {
    pub message: String,
    pub data: Value,
}

pub trait ControllerJobDriver: Send + Sync {
    fn job_type(&self) -> &'static str;
    fn version(&self) -> u32;

    /// Return the safe request projection exposed through public job events.
    /// The original request remains in Homeboy's private durable store.
    fn public_request(&self, request: &Value) -> Result<Value>;
    fn public_progress(&self, progress: &Value) -> Result<Value>;
    fn public_result(&self, result: &Value) -> Result<Value>;
    fn public_error(&self, error: &Error) -> ControllerJobPublicError;

    /// Validate that sensitive inputs are durable references rather than inline
    /// values. Domain drivers define their own reference vocabulary.
    fn validate_secret_references(&self, request: &Value) -> Result<()>;

    /// Prepare the persisted request inside the daemon worker. Drivers may
    /// override this to resolve controller-local inputs after durable admission.
    fn prepare(&self, request: Value) -> Result<Value> {
        Ok(request)
    }

    fn execute(&self, prepared: Value, job: ControllerJobHandle) -> Result<Value>;

    /// Resume one daemon-recovered job from the authoritative checkpoint written
    /// after `prepare`. Implementations must treat this as a new process-local
    /// invocation of the same idempotent durable operation.
    fn resume(&self, checkpoint: Value, job: ControllerJobHandle) -> Result<Value> {
        self.execute(checkpoint, job)
    }

    /// Called by the daemon when the durable job is cancelled. Implementations
    /// must stop their owned work before returning.
    fn cancel(&self, prepared: &Value) -> Result<()>;
}

/// The only event surface exposed to controller drivers. Every event payload is
/// projected by the driver before it reaches the durable public job log.
#[derive(Clone)]
pub struct ControllerJobHandle {
    job: JobHandle,
    driver: Arc<dyn ControllerJobDriver>,
}

impl ControllerJobHandle {
    pub(crate) fn new(job: JobHandle, driver: Arc<dyn ControllerJobDriver>) -> Self {
        Self { job, driver }
    }

    pub fn is_cancelled(&self) -> bool {
        self.job.is_cancelled()
    }

    pub fn progress(&self, private_progress: Value) -> Result<()> {
        self.job
            .progress(self.driver.public_progress(&private_progress)?)
            .map(|_| ())
    }
}

fn drivers() -> &'static Mutex<HashMap<(String, u32), Arc<dyn ControllerJobDriver>>> {
    static DRIVERS: std::sync::OnceLock<
        Mutex<HashMap<(String, u32), Arc<dyn ControllerJobDriver>>>,
    > = std::sync::OnceLock::new();
    DRIVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_controller_job_driver(driver: Arc<dyn ControllerJobDriver>) -> Result<()> {
    let key = (driver.job_type().to_string(), driver.version());
    let mut registry = drivers().lock().expect("controller job driver lock");
    if registry.contains_key(&key) {
        return Err(Error::validation_invalid_argument(
            "controller_job_driver",
            format!(
                "controller job driver `{}` version {} is already registered",
                key.0, key.1
            ),
            Some(key.0),
            None,
        ));
    }
    registry.insert(key, driver);
    Ok(())
}

pub(crate) fn driver(job_type: &str, version: u32) -> Result<Arc<dyn ControllerJobDriver>> {
    drivers()
        .lock()
        .expect("controller job driver lock")
        .get(&(job_type.to_string(), version))
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "type",
                format!(
                    "no controller job driver is registered for `{job_type}` version {version}"
                ),
                Some(job_type.to_string()),
                None,
            )
        })
}

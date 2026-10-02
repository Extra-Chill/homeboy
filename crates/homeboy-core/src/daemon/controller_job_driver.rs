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

/// Read-only projection of externally supervised work. This is not a daemon
/// spawn reservation: the domain retains responsibility for cancellation and
/// recovery, including collecting its durable outcome after process death.
#[derive(Debug, Clone)]
pub enum ControllerJobExecutionOwner {
    /// No external work remains. The driver may still need to publish its
    /// retained checkpoint outcome or advance controller-local work.
    None,
    /// Exact, kernel-discriminated identities of the supervised work.
    Processes(Vec<ControllerJobExecutionProcess>),
    /// The domain cannot establish ownership from available durable evidence.
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct ControllerJobExecutionProcess {
    pub pid: u32,
    pub start_identity: crate::process::ProcessStartIdentity,
}

impl ControllerJobExecutionOwner {
    pub fn supervised(pid: u32, start_identity: crate::process::ProcessStartIdentity) -> Self {
        Self::Processes(vec![ControllerJobExecutionProcess {
            pid,
            start_identity,
        }])
    }

    /// An exact live owner wins; incomplete inspection never proves absence.
    /// PID reuse remains distinct from an actually dead process.
    pub fn inspect(&self) -> crate::process::ProcessIdentityState {
        self.inspect_with_pid().1
    }

    /// Keep the reported PID and cohort classification from the same probe.
    pub(crate) fn inspect_with_pid(&self) -> (Option<u32>, crate::process::ProcessIdentityState) {
        use crate::process::ProcessIdentityState as State;
        match self {
            Self::None => (None, State::Dead),
            Self::Unavailable => (None, State::Unverifiable),
            Self::Processes(processes) if processes.is_empty() => (None, State::Unverifiable),
            Self::Processes(processes) => {
                let states = processes
                    .iter()
                    .map(|process| {
                        (
                            Some(process.pid),
                            crate::process::process_identity_state_with_start_identity(
                                process.pid,
                                None,
                                Some(&process.start_identity),
                            ),
                        )
                    })
                    .collect::<Vec<_>>();
                [
                    State::Live,
                    State::Unverifiable,
                    State::IdentityMismatch,
                    State::Dead,
                ]
                .into_iter()
                .find_map(|state| {
                    states
                        .iter()
                        .find(|(_, observed)| *observed == state)
                        .copied()
                })
                .expect("nonempty process cohort")
            }
        }
    }
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

    /// The controller-minted durable run this job executes for, extracted from
    /// the driver's typed request. The daemon persists the linkage at
    /// admission — before any driver work can escape the daemon lifecycle — so
    /// recovery can reconcile this job from its linked run's terminal state
    /// without parsing the opaque request.
    fn linked_durable_run_id(&self, _request: &Value) -> Option<String> {
        None
    }

    /// Interpret private typed state without preparing, advancing, or mutating
    /// work. Before checkpoint publication the admitted request is authoritative.
    /// `None` retains the existing generic local-child/linked-run contract;
    /// `Some(Unavailable)` protects driver-owned work whose evidence is missing.
    fn execution_owner(
        &self,
        _request: &Value,
        _checkpoint: Option<&Value>,
    ) -> Result<Option<ControllerJobExecutionOwner>> {
        Ok(None)
    }

    /// Recover a checkpoint from admitted state without preparing or launching
    /// work. Only drivers whose request already identifies external work may
    /// support this pre-checkpoint handoff boundary.
    fn recovery_checkpoint(&self, _request: &Value) -> Result<Option<Value>> {
        Ok(None)
    }

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

    /// Durable identity for domain projections which need to link their parent
    /// record to this generic controller job.
    pub fn job_id(&self) -> String {
        self.job.job_id().to_string()
    }

    pub fn progress(&self, private_progress: Value) -> Result<()> {
        self.job
            .progress(self.driver.public_progress(&private_progress)?)
            .map(|_| ())
    }

    /// Replace the durable recovery checkpoint after an idempotent phase has
    /// completed. The daemon resumes from this value after process recovery.
    pub fn checkpoint(&self, private_checkpoint: Value) -> Result<()> {
        self.job.record_controller_prepared(private_checkpoint)
    }
}

type ControllerJobDriverKey = (String, u32);
type ControllerJobDrivers = Mutex<HashMap<ControllerJobDriverKey, Arc<dyn ControllerJobDriver>>>;

fn drivers() -> &'static ControllerJobDrivers {
    static DRIVERS: std::sync::OnceLock<ControllerJobDrivers> = std::sync::OnceLock::new();
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

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::process::ProcessIdentityState as State;
    use crate::test_support::SupervisedProcessFixture;

    #[test]
    fn execution_owner_inspection_distinguishes_live_reused_dead_and_unavailable() {
        let mut child = SupervisedProcessFixture::spawn();
        let owner = ControllerJobExecutionOwner::supervised(child.pid(), child.identity.clone());
        assert_eq!(owner.inspect_with_pid(), (Some(child.pid()), State::Live));
        let reused =
            ControllerJobExecutionOwner::supervised(child.pid(), child.mismatched_identity());
        assert_eq!(reused.inspect(), State::IdentityMismatch);
        assert_eq!(
            ControllerJobExecutionOwner::Unavailable.inspect(),
            State::Unverifiable
        );
        assert_eq!(
            ControllerJobExecutionOwner::Processes(Vec::new()).inspect(),
            State::Unverifiable
        );
        assert_eq!(
            ControllerJobExecutionOwner::supervised(0, child.identity.clone()).inspect(),
            State::Unverifiable
        );
        let cohort = ControllerJobExecutionOwner::Processes(vec![
            ControllerJobExecutionProcess {
                pid: 0,
                start_identity: child.identity.clone(),
            },
            ControllerJobExecutionProcess {
                pid: child.pid(),
                start_identity: child.identity.clone(),
            },
        ]);
        assert_eq!(
            cohort.inspect_with_pid(),
            (Some(child.pid()), State::Live),
            "the reported PID must actually support the live classification"
        );
        child.stop();
        assert_eq!(owner.inspect(), State::Dead);
        assert_eq!(
            cohort.inspect(),
            State::Unverifiable,
            "one dead member does not resolve unavailable evidence"
        );
    }
}

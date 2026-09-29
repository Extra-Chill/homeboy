//! Daemon-driven orchestration hook.
//!
//! Stale-run reconciliation is implemented in `homeboy-agents` and was only
//! ever advanced by a human typing a command.
//! The daemon is the only long-lived process that can drive them, and it lives
//! in `homeboy-core`, which must not depend on the agent-task subsystem.
//!
//! This is that seam. `homeboy-agents` registers a driver at startup; with no
//! driver registered the daemon's orchestration tick is inert rather than
//! broken, which is the correct behaviour for a build that does not link the
//! agent-task subsystem at all.
//!
//! Every method returns rather than panics, and each is invoked separately by
//! the tick so one failing mechanism cannot stop the others.

use crate::observation::{ObservationStore, WorkIntent};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::Result;

type WorkIntentSchedulers =
    Mutex<HashMap<(String, u32), Arc<dyn Fn(&Value) -> Result<Value> + Send + Sync>>>;

fn work_intent_schedulers() -> &'static WorkIntentSchedulers {
    static SCHEDULERS: OnceLock<WorkIntentSchedulers> = OnceLock::new();
    SCHEDULERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Domains register their typed scheduling action; the outbox owns replay.
pub fn register_work_intent_scheduler(
    kind: &str,
    version: u32,
    schedule: Arc<dyn Fn(&Value) -> Result<Value> + Send + Sync>,
) -> Result<()> {
    if kind.trim().is_empty() || version == 0 {
        return Err(crate::Error::validation_invalid_argument(
            "work_intent",
            "require a work type and version",
            None,
            None,
        ));
    }
    let mut registry = work_intent_schedulers()
        .lock()
        .expect("work intent schedulers lock");
    let key = (kind.to_string(), version);
    if registry.contains_key(&key) {
        return Err(crate::Error::validation_invalid_argument(
            "work_intent",
            "work scheduler already registered",
            Some(kind.to_string()),
            None,
        ));
    }
    registry.insert(key, schedule);
    Ok(())
}

/// Agent-task orchestration the daemon drives on a timer.
pub trait OrchestrationDriver: Send + Sync {
    /// Recover orphaned `running` agent-task records whose owner died.
    ///
    /// Implementations apply the same safe cancel path the manual
    /// `agent-task active --reconcile --apply` command uses; the daemon only
    /// supplies the cadence.
    fn reconcile_stale_active_runs(&self) -> Result<Value>;

    /// Advance durable Cooks that were admitted before a Lab destination was
    /// eligible. Implementations must not materialize work while blocked.
    fn reconcile_unmaterialized_cook_admissions(&self) -> Result<Value>;

    /// Resume one durable queued retry whose reservation survived its launcher.
    fn reconcile_queued_retries(&self) -> Result<Value>;

    /// Reconcile durable controller waits from locally observable evidence.
    /// External-event waits must remain open until their event or declared
    /// deadline is present.
    fn reconcile_waiting_controllers(&self) -> Result<Value>;
}

/// CLI-owned execution seam for an already-fenced Cook admission replay.
/// Core and agents carry only typed JSON and never depend on CLI or Lab types.
pub trait CookAdmissionReplayDriver: Send + Sync {
    /// Select one currently eligible runner using the CLI/Lab policy that owns
    /// configured preferences and capability admission.
    fn select_runner(&self, request: &Value) -> Result<Value>;

    /// Start a replay worker. The worker must consume the supplied token at the
    /// lifecycle mutation boundary before it performs any route side effect.
    fn replay(&self, request: &Value) -> Result<Value>;

    /// Admit terminal Cook continuation work as a durable controller job.
    fn schedule_terminal_continuation(&self, _request: &Value) -> Result<Value> {
        Ok(Value::Null)
    }
}

/// CLI-owned execution seam for a durable retry reservation. The lifecycle
/// consumer must claim the queued record before dispatching it.
pub trait QueuedRetryReplayDriver: Send + Sync {
    fn replay(&self, run_id: &str) -> Result<Value>;
}

struct NoopCookAdmissionReplayDriver;

impl CookAdmissionReplayDriver for NoopCookAdmissionReplayDriver {
    fn select_runner(&self, _request: &Value) -> Result<Value> {
        Ok(serde_json::json!({
            "state": "blocked_runner_unavailable",
            "reason": "Cook admission replay driver is not registered",
        }))
    }

    fn replay(&self, _request: &Value) -> Result<Value> {
        Err(crate::Error::internal_unexpected(
            "Cook admission replay driver is not registered",
        ))
    }
}

struct NoopQueuedRetryReplayDriver;

impl QueuedRetryReplayDriver for NoopQueuedRetryReplayDriver {
    fn replay(&self, _run_id: &str) -> Result<Value> {
        Ok(Value::Null)
    }
}

/// Inert driver used when the agent-task subsystem is not linked or not wired.
struct NoopOrchestrationDriver;

impl OrchestrationDriver for NoopOrchestrationDriver {
    fn reconcile_stale_active_runs(&self) -> Result<Value> {
        Ok(Value::Null)
    }

    fn reconcile_unmaterialized_cook_admissions(&self) -> Result<Value> {
        Ok(Value::Null)
    }

    fn reconcile_queued_retries(&self) -> Result<Value> {
        Ok(Value::Null)
    }

    fn reconcile_waiting_controllers(&self) -> Result<Value> {
        Ok(Value::Null)
    }
}

homeboy_engine_primitives::provider_registry_arc! {
    provider: dyn OrchestrationDriver,
    noop: NoopOrchestrationDriver,
    /// Register the agent-task orchestration driver. Called once at startup.
    register: pub fn register_orchestration_driver,
    /// Resolve the active driver, cloning the `Arc` so the registry lock is not
    /// held while a reconcile pass runs.
    active: fn active_driver,
}

mod cook_replay_registry {
    use super::{CookAdmissionReplayDriver, NoopCookAdmissionReplayDriver};

    homeboy_engine_primitives::provider_registry_arc! {
        provider: dyn CookAdmissionReplayDriver,
        noop: NoopCookAdmissionReplayDriver,
        register: pub(super) fn register,
        active: pub(super) fn active,
    }
}

mod queued_retry_replay_registry {
    use super::{NoopQueuedRetryReplayDriver, QueuedRetryReplayDriver};

    homeboy_engine_primitives::provider_registry_arc! {
        provider: dyn QueuedRetryReplayDriver,
        noop: NoopQueuedRetryReplayDriver,
        register: pub(super) fn register,
        active: pub(super) fn active,
    }
}

/// Register the CLI replay implementation at startup.
pub fn register_cook_admission_replay_driver(
    driver: std::sync::Arc<dyn CookAdmissionReplayDriver>,
) {
    cook_replay_registry::register(driver);
}

/// Register the CLI queue consumer at startup.
pub fn register_queued_retry_replay_driver(driver: std::sync::Arc<dyn QueuedRetryReplayDriver>) {
    queued_retry_replay_registry::register(driver);
}

/// Drive one stale-active-run reconcile pass.
///
/// Public so the owning layer can exercise a single pass without standing up a
/// daemon, and so an operator-facing command could drive the same pass the
/// tick drives.
pub fn reconcile_stale_active_runs() -> Result<Value> {
    active_driver().reconcile_stale_active_runs()
}

/// Drive one unmaterialized Cook admission pass.
pub fn reconcile_unmaterialized_cook_admissions() -> Result<Value> {
    active_driver().reconcile_unmaterialized_cook_admissions()
}

/// Drive one queued-retry recovery pass.
pub fn reconcile_queued_retries() -> Result<Value> {
    active_driver().reconcile_queued_retries()
}

/// Drain one indexed intent. The source lifecycle write and intent share a
/// SQLite transaction; the daemon job submission has its own idempotent key.
/// A crash after submission but before ACK replays the same job on restart.
pub fn drain_work_intents() -> Result<Value> {
    drain_work_intents_with(|intent| {
        let schedule = work_intent_schedulers()
            .lock()
            .expect("work intent schedulers lock")
            .get(&(intent.kind.clone(), intent.version))
            .cloned()
            .ok_or_else(|| {
                crate::Error::validation_invalid_argument(
                    "work_intent.kind",
                    "no scheduler registered for work intent",
                    Some(intent.kind.clone()),
                    None,
                )
            })?;
        schedule(&intent.payload)
    })
}

pub fn drain_work_intents_with(
    schedule: impl FnOnce(&WorkIntent) -> Result<Value>,
) -> Result<Value> {
    let store = ObservationStore::open_initialized()?;
    let Some(intent) = store.next_pending_work_intent()? else {
        return Ok(serde_json::json!({"scheduled": false}));
    };
    let receipt = schedule(&intent)?;
    if receipt["scheduled"] != true || receipt["job_id"].as_str().is_none_or(str::is_empty) {
        return Err(crate::Error::internal_unexpected(
            "work intent scheduling returned no durable job receipt",
        ));
    }
    store.acknowledge_work_intent(&intent.id, &receipt)?;
    Ok(receipt)
}

pub fn schedule_terminal_cook_continuation(request: &Value) -> Result<Value> {
    cook_replay_registry::active().schedule_terminal_continuation(request)
}

/// Drive one durable controller-wait reconciliation pass.
pub fn reconcile_waiting_controllers() -> Result<Value> {
    active_driver().reconcile_waiting_controllers()
}

/// Invoke the registered replay worker after agents has durably claimed it.
pub fn replay_unmaterialized_cook_admission(request: &Value) -> Result<Value> {
    cook_replay_registry::active().replay(request)
}

/// Invoke the registered queue consumer. It must atomically claim `run_id`
/// before any dispatch so replay and concurrent ticks converge.
pub fn replay_queued_retry(run_id: &str) -> Result<Value> {
    queued_retry_replay_registry::active().replay(run_id)
}

/// Resolve current Lab eligibility through the registered CLI-owned policy.
pub fn select_unmaterialized_cook_runner(request: &Value) -> Result<Value> {
    cook_replay_registry::active().select_runner(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct RecordingReplayDriver(Arc<AtomicUsize>);

    impl CookAdmissionReplayDriver for RecordingReplayDriver {
        fn select_runner(&self, _request: &Value) -> Result<Value> {
            Ok(serde_json::json!({ "state": "eligible", "runner_id": "lab" }))
        }

        fn replay(&self, request: &Value) -> Result<Value> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({ "fence": request["fence"] }))
        }
    }

    #[test]
    fn registered_replay_driver_receives_the_exact_fenced_request_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        register_cook_admission_replay_driver(Arc::new(RecordingReplayDriver(Arc::clone(&calls))));
        let request = serde_json::json!({ "cook_id": "cook-1", "fence": 4, "token": "t" });
        let receipt = replay_unmaterialized_cook_admission(&request).expect("replayed");
        assert_eq!(receipt["fence"], 4);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        register_cook_admission_replay_driver(Arc::new(NoopCookAdmissionReplayDriver));
    }
}

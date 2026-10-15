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
}

/// Inert driver used when the agent-task subsystem is not linked or not wired.
struct NoopOrchestrationDriver;

impl OrchestrationDriver for NoopOrchestrationDriver {
    fn reconcile_stale_active_runs(&self) -> Result<Value> {
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

/// Drive one stale-active-run reconcile pass.
///
/// Public so the owning layer can exercise a single pass without standing up a
/// daemon, and so an operator-facing command could drive the same pass the
/// tick drives.
pub fn reconcile_stale_active_runs() -> Result<Value> {
    active_driver().reconcile_stale_active_runs()
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

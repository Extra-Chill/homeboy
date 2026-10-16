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

/// Drain every work intent due at the start of this pass, oldest first. The
/// source lifecycle write and intent share a SQLite transaction; the daemon
/// job submission has its own idempotent key, so a crash after submission but
/// before ACK replays the same job on restart.
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

/// A failed delivery keeps custody: the intent is deferred with its own
/// durable backoff and the pass continues with the next due intent, so one
/// failing intent never blocks newer ones. Deferred intents are not due again
/// before the pass's start time, which bounds the pass.
///
/// Returns `{"scheduled": [receipt...], "deferred": [WorkIntentDeferral...]}`.
pub fn drain_work_intents_with(
    mut schedule: impl FnMut(&WorkIntent) -> Result<Value>,
) -> Result<Value> {
    let store = ObservationStore::open_initialized()?;
    let now_ms = crate::api_jobs::timestamp_ms();
    let mut scheduled = Vec::new();
    let mut deferred = Vec::new();
    while let Some(intent) = store.next_pending_work_intent(now_ms)? {
        match schedule(&intent).and_then(durable_job_receipt) {
            Ok(receipt) => {
                store.acknowledge_work_intent(&intent.id, &receipt)?;
                scheduled.push(receipt);
            }
            Err(error) => {
                deferred.extend(store.defer_work_intent(&intent.id, now_ms, &error.message)?)
            }
        }
    }
    Ok(serde_json::json!({ "scheduled": scheduled, "deferred": deferred }))
}

fn durable_job_receipt(receipt: Value) -> Result<Value> {
    if receipt["scheduled"] != true || receipt["job_id"].as_str().is_none_or(str::is_empty) {
        return Err(crate::Error::internal_unexpected(
            "work intent scheduling returned no durable job receipt",
        ));
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::RunRecord;
    use crate::test_support::with_isolated_home;

    fn commit_intents(ids: &[&str]) {
        let run = RunRecord {
            id: "drain-run".to_string(),
            kind: "agent-task".to_string(),
            component_id: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            status: "running".to_string(),
            command: None,
            cwd: None,
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({}),
        };
        let intents: Vec<_> = ids
            .iter()
            .map(|id| WorkIntent {
                id: id.to_string(),
                run_id: run.id.clone(),
                kind: "test".to_string(),
                version: 1,
                payload: serde_json::json!({ "id": id }),
            })
            .collect();
        ObservationStore::open_initialized()
            .unwrap()
            .upsert_imported_run_with_events_and_intents(&run, true, None, None, &[], &intents)
            .unwrap();
    }

    #[test]
    fn a_failing_head_intent_defers_without_blocking_newer_intents() {
        with_isolated_home(|_| {
            commit_intents(&["intent-a-head", "intent-b-newer"]);
            let schedule = |fail_head: bool| {
                move |intent: &WorkIntent| {
                    if fail_head && intent.id == "intent-a-head" {
                        return Err(crate::Error::internal_unexpected(
                            "controller idempotency key is already bound",
                        ));
                    }
                    Ok(
                        serde_json::json!({ "scheduled": true, "job_id": format!("job:{}", intent.id) }),
                    )
                }
            };

            let pass = drain_work_intents_with(schedule(true)).unwrap();
            assert_eq!(
                pass["scheduled"],
                serde_json::json!([{ "scheduled": true, "job_id": "job:intent-b-newer" }])
            );
            assert_eq!(pass["deferred"][0]["id"], "intent-a-head");
            assert_eq!(pass["deferred"][0]["attempts"], 1);
            assert_eq!(
                pass["deferred"][0]["last_error"],
                "controller idempotency key is already bound"
            );

            let backing_off = drain_work_intents_with(|_| -> Result<Value> {
                panic!("an intent in backoff is not retried early")
            })
            .unwrap();
            assert_eq!(
                backing_off,
                serde_json::json!({ "scheduled": [], "deferred": [] })
            );

            let deadline = pass["deferred"][0]["next_attempt_ms"].as_u64().unwrap();
            while crate::api_jobs::timestamp_ms() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let retried = drain_work_intents_with(schedule(false)).unwrap();
            assert_eq!(
                retried["scheduled"],
                serde_json::json!([{ "scheduled": true, "job_id": "job:intent-a-head" }])
            );
            assert_eq!(retried["deferred"], serde_json::json!([]));
        });
    }
}

//! Daemon-driven orchestration hook.
//!
//! Stale-run reconciliation and controller wait resolution are implemented in
//! `homeboy-agents` and were only ever advanced by a human typing a command.
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

use serde_json::Value;

use crate::error::Result;

/// Agent-task orchestration the daemon drives on a timer.
pub trait OrchestrationDriver: Send + Sync {
    /// Recover orphaned `running` agent-task records whose owner died.
    ///
    /// Implementations apply the same safe cancel path the manual
    /// `agent-task active --reconcile --apply` command uses; the daemon only
    /// supplies the cadence.
    fn reconcile_stale_active_runs(&self) -> Result<Value>;

    /// Resolve controller waits that durable state already satisfies.
    ///
    /// A controller parked in `Waiting` has no pending action, so `resume`
    /// reports `idle` and exits. Nothing polls and nothing times out, which
    /// makes `Waiting` a state with no automatic exit. Implementations resolve
    /// only on unambiguous durable evidence.
    fn reconcile_controller_waits(&self) -> Result<Value>;

    /// Advance durable Cooks that were admitted before a Lab destination was
    /// eligible. Implementations must not materialize work while blocked.
    fn reconcile_unmaterialized_cook_admissions(&self) -> Result<Value>;
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

/// Inert driver used when the agent-task subsystem is not linked or not wired.
struct NoopOrchestrationDriver;

impl OrchestrationDriver for NoopOrchestrationDriver {
    fn reconcile_stale_active_runs(&self) -> Result<Value> {
        Ok(Value::Null)
    }

    fn reconcile_controller_waits(&self) -> Result<Value> {
        Ok(Value::Null)
    }

    fn reconcile_unmaterialized_cook_admissions(&self) -> Result<Value> {
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

/// Register the CLI replay implementation at startup.
pub fn register_cook_admission_replay_driver(
    driver: std::sync::Arc<dyn CookAdmissionReplayDriver>,
) {
    cook_replay_registry::register(driver);
}

/// Drive one stale-active-run reconcile pass.
///
/// Public so the owning layer can exercise a single pass without standing up a
/// daemon, and so an operator-facing command could drive the same pass the
/// tick drives.
pub fn reconcile_stale_active_runs() -> Result<Value> {
    active_driver().reconcile_stale_active_runs()
}

/// Drive one controller-wait reconcile pass. Public for the same reason as
/// [`reconcile_stale_active_runs`].
pub fn reconcile_controller_waits() -> Result<Value> {
    active_driver().reconcile_controller_waits()
}

/// Drive one unmaterialized Cook admission pass.
pub fn reconcile_unmaterialized_cook_admissions() -> Result<Value> {
    active_driver().reconcile_unmaterialized_cook_admissions()
}

/// Invoke the registered replay worker after agents has durably claimed it.
pub fn replay_unmaterialized_cook_admission(request: &Value) -> Result<Value> {
    cook_replay_registry::active().replay(request)
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

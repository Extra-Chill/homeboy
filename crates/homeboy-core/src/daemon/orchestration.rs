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

/// Drive one controller-wait reconcile pass. Public for the same reason as
/// [`reconcile_stale_active_runs`].
pub fn reconcile_controller_waits() -> Result<Value> {
    active_driver().reconcile_controller_waits()
}

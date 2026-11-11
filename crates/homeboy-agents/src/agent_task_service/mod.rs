//! Agent-task run service: discovery/liveness reporting, stale-run
//! reconciliation, plan execution lifecycle, and the deterministic cook
//! orchestration cycle. Split out of a former single-file god-module into
//! concern submodules; this `mod.rs` only wires the submodules together and
//! re-exports their public surface so existing call sites keep resolving
//! `crate::agent_task_service::*` unchanged.

mod cook;
mod cook_ci;
pub use cook_ci::{cook_ci_status, repoll_cook_ci};
mod cook_activity;
mod cook_adoption;
/// Fresh provider attempts start from a clean Cook base tree.
mod cook_attempt_workspace;
pub use cook_attempt_workspace::AttemptWorkspaceNotAtBase;
mod cook_baseline;
pub(crate) use cook_baseline::compare_gate_failures_to_verified_base;
mod cook_budget;
mod cook_delivery;
/// The named readers of the artifacts the `cook_delivery` record holds.
mod cook_delivery_artifacts;
mod cook_lineage;
pub(crate) mod cook_pre_execution;
mod cook_prior_candidate;
mod cook_promotion;
mod cook_recipe;
/// Repro gates that must fail on the Cook base and pass on the candidate.
mod cook_repro;
pub use cook_repro::{CookReproBaseEvidence, CookReproBaseResult};
/// The one Cook recovery decision and its `agent-task recover` executor.
mod cook_recovery;
/// Detached Cook execution as a daemon `step` with an admitted environment.
mod cook_step;
/// Resource supervision of a running Cook against its declared budgets.
mod cook_supervision;
mod discovery;
mod execution;
mod execution_step;
/// Read-only process-tree activity sampling for a running Cook.
///
/// Lived in `homeboy-core` as a top-level module even though `cook_activity`
/// here was its only consumer anywhere in the workspace (#11143). Kept `pub`
/// so the sampling primitives stay addressable evidence rather than becoming
/// unreachable internals.
pub mod process_activity;
mod promotion_service;
mod reconcile;
mod status_support;
/// Shared daemon lifecycle for newly submitted orchestration work.
mod work_job;

/// The promotion request is defined once in `agent_task_promotion`; the service
/// layer re-exports it so existing importers keep one canonical type.
pub use crate::agent_task_promotion::AgentTaskPromotionRequest;
pub(crate) use cook::review_form_from_aggregate;
pub use cook::*;
pub use cook_activity::{worktree_files_changed, CookActivityProbe, CookProviderActivity};
pub use cook_adoption::*;
pub use cook_baseline::*;
pub use cook_budget::*;
pub use cook_delivery::publication_receipt;
#[cfg(any(test, feature = "test-support"))]
pub use cook_delivery::test_support::*;
pub(crate) use cook_delivery::COOK_DELIVERY_KEY;
pub use cook_delivery::{
    CookDelivery, CookDeliveryArtifacts, CookDeliveryBlocker, CookDeliveryBlockerCode,
    CookDeliveryCandidate, CookDeliveryDestination, CookDeliveryGate, CookDeliveryLifecycle,
    CookDeliveryPublication, CookDeliveryState, CookManualPublication, COOK_DELIVERY_SCHEMA,
};
pub use cook_delivery_artifacts::*;
pub use cook_lineage::{CookAttemptLineage, CookLineageKind};
#[cfg(test)]
pub(crate) use cook_pre_execution::*;
pub use cook_pre_execution::{recover_recipe_attempt, retryable_pre_execution_failure};
pub use cook_promotion::*;
pub use cook_recipe::*;
pub use cook_recovery::{
    cook_recovery_delegation, is_store_contention, plan_cook_recovery, recover_cook,
    with_store_contention_backoff, CookPublishRequest, CookRecoveryOverrides,
    CookRecoveryPerformer, CookRecoveryPlan, CookRecoveryReport, CookRecoveryRequest,
    CookRecoveryStep, CookReplaceGatesRequest, CookReplacementProof, COOK_RECOVERY_SCHEMA,
    STORE_BUSY_CODE,
};
pub(crate) use cook_recovery::{cook_recovery_status, incomplete_review_recovery_command};

/// Observe the local controller daemon with the one freshness report
/// `homeboy daemon status` renders (#16181). Queued work is scheduled by that
/// daemon's step jobs, so this observation — never a second liveness probe —
/// decides whether a waiting admission can still be scheduled. `None` when the
/// report itself is unreadable; a missing observation never claims a live
/// daemon, it only declines to name the blocker.
pub fn scheduling_daemon_evidence(
) -> Option<homeboy_control_plane_contract::ControlPlaneBlockerDaemon> {
    let status = homeboy_core::daemon::read_status().ok()?;
    Some(homeboy_control_plane_contract::ControlPlaneBlockerDaemon {
        admits_work: status.admits_work(),
        summary: status.render_summary(),
    })
}

/// Start the local controller daemon that schedules queued work, as the
/// submitter of daemon-scheduled work must under the on-demand model
/// (#17506). `ensure_running` defers to a service manager and keeps routing to
/// a busy older build. Reports replacement waiting without rejecting the
/// durable submission, or the start error when unavailable. Status is read-only.
pub fn ensure_scheduling_daemon() -> Option<serde_json::Value> {
    let result = start_scheduling_daemon();
    if let Ok(status) = homeboy_core::daemon::read_status() {
        let report = status.admission_report();
        if report["waiting_for"] == "daemon_replacement" {
            return Some(report);
        }
    }
    let error = result.err()?;
    Some(serde_json::json!({
        "daemon": scheduling_daemon_evidence(),
        "error": error.attributed_message(),
        "next_action": "homeboy daemon recover",
    }))
}

fn start_scheduling_daemon() -> homeboy_core::Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(start) = SCHEDULING_DAEMON_START.with(std::cell::Cell::get) {
        return start();
    }
    homeboy_core::daemon::ensure_running(homeboy_core::daemon::DEFAULT_ADDR).map(drop)
}

// This crate's own unit tests never start a daemon from their test binary.
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SCHEDULING_DAEMON_START: std::cell::Cell<Option<fn() -> homeboy_core::Result<()>>> =
        const { std::cell::Cell::new(if cfg!(test) { Some(scheduling_daemon_running) } else { None }) };
}

#[cfg(any(test, feature = "test-support"))]
fn scheduling_daemon_running() -> homeboy_core::Result<()> {
    Ok(())
}

/// Run `body` with `start` standing in for starting the scheduling daemon on
/// this thread, so in-process tests never spawn their own binary as a daemon.
#[cfg(any(test, feature = "test-support"))]
pub fn with_scheduling_daemon_start<R>(
    start: fn() -> homeboy_core::Result<()>,
    body: impl FnOnce() -> R,
) -> R {
    let previous = SCHEDULING_DAEMON_START.with(|slot| slot.replace(Some(start)));
    let result = body();
    SCHEDULING_DAEMON_START.with(|slot| slot.set(previous));
    result
}
#[cfg(test)]
pub(crate) use cook_step::finalize_detached_cook_attempt;
pub use cook_step::*;
pub use cook_supervision::{resolve_supervision_policy, CookSupervisionTick, CookSupervisor};
pub use discovery::*;
pub use execution::*;
pub use execution_step::*;
pub use promotion_service::*;
pub use reconcile::*;
pub use status_support::*;
pub use work_job::*;

#[cfg(test)]
mod tests;

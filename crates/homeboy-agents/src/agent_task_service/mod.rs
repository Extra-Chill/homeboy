//! Agent-task run service: discovery/liveness reporting, stale-run
//! reconciliation, plan execution lifecycle, and the deterministic cook
//! orchestration cycle. Split out of a former single-file god-module into
//! concern submodules; this `mod.rs` only wires the submodules together and
//! re-exports their public surface so existing call sites keep resolving
//! `crate::agent_task_service::*` unchanged.

mod cook;
mod cook_activity;
mod cook_adoption;
mod cook_baseline;
mod cook_budget;
mod cook_delivery;
/// The named raw readers of per-stage delivery artifacts (#16049).
mod cook_delivery_artifacts;
mod cook_lineage;
pub(crate) mod cook_pre_execution;
mod cook_prior_candidate;
mod cook_promotion;
mod cook_recipe;
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
pub use cook::*;
pub use cook_activity::{worktree_files_changed, CookActivityProbe, CookProviderActivity};
pub use cook_adoption::*;
pub use cook_baseline::*;
pub use cook_budget::*;
pub use cook_delivery::publication_receipt;
pub(crate) use cook_delivery::COOK_DELIVERY_KEY;
pub use cook_delivery::{
    CookDelivery, CookDeliveryBlocker, CookDeliveryBlockerCode, CookDeliveryCandidate,
    CookDeliveryDestination, CookDeliveryGate, CookDeliveryLifecycle, CookDeliveryPublication,
    CookDeliveryState, COOK_DELIVERY_SCHEMA,
};
pub use cook_delivery_artifacts::*;
pub use cook_lineage::{CookAttemptLineage, CookLineageKind};
#[cfg(test)]
pub(crate) use cook_pre_execution::*;
pub use cook_pre_execution::{
    rebind_queued_cook_runtime_in_store, recover_recipe_attempt, retryable_pre_execution_failure,
};
pub use cook_promotion::*;
pub use cook_recipe::*;
pub use cook_recovery::{
    plan_cook_recovery, recover_cook, CookRecoveryPerformer, CookRecoveryPlan, CookRecoveryReport,
    CookRecoveryStep, COOK_RECOVERY_SCHEMA,
};
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

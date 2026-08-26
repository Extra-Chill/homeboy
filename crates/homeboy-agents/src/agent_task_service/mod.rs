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
/// Daemon-owned durable lifecycle for a locally-placed detached cook batch.
mod cook_batch_job;
mod cook_budget;
/// Daemon-owned durable lifecycle for a locally-placed detached Cook.
mod cook_job;
pub(crate) mod cook_pre_execution;
mod cook_promotion;
mod cook_recipe;
/// Resource supervision of a running Cook against its declared budgets.
mod cook_supervision;
mod discovery;
mod execution;
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

pub use cook::*;
pub use cook_activity::{CookActivityProbe, CookProviderActivity};
pub use cook_adoption::*;
pub use cook_baseline::*;
pub use cook_batch_job::*;
pub use cook_budget::*;
pub use cook_job::*;
#[cfg(not(test))]
pub use cook_pre_execution::recover_recipe_attempt;
#[cfg(test)]
pub(crate) use cook_pre_execution::*;
pub use cook_promotion::*;
pub use cook_recipe::*;
pub use cook_supervision::{resolve_supervision_policy, CookSupervisionTick, CookSupervisor};
pub use discovery::*;
pub use execution::*;
pub use promotion_service::*;
pub use reconcile::*;
pub use status_support::*;
pub use work_job::*;

#[cfg(test)]
mod tests;

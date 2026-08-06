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
mod cook_pre_execution;
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

pub use cook::*;
pub use cook_activity::{CookActivityProbe, CookProviderActivity};
pub use cook_adoption::*;
pub use cook_baseline::*;
pub use cook_budget::*;
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

#[cfg(test)]
mod tests;

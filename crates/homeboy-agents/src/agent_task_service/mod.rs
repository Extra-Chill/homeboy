//! Agent-task run service: discovery/liveness reporting, stale-run
//! reconciliation, plan execution lifecycle, and the deterministic cook
//! orchestration cycle. Split out of a former single-file god-module into
//! concern submodules; this `mod.rs` only wires the submodules together and
//! re-exports their public surface so existing call sites keep resolving
//! `crate::agent_task_service::*` unchanged.

mod cook;
mod cook_adoption;
mod cook_baseline;
mod cook_budget;
mod cook_pre_execution;
mod cook_promotion;
mod cook_recipe;
mod discovery;
mod execution;
mod reconcile;
mod status_support;

pub use cook::*;
pub use cook_adoption::*;
pub use cook_baseline::*;
pub use cook_budget::*;
pub use cook_pre_execution::*;
pub use cook_promotion::*;
pub use cook_recipe::*;
pub use discovery::*;
pub use execution::*;
pub use reconcile::*;
pub use status_support::*;

#[cfg(test)]
mod tests;

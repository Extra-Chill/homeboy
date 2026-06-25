//! Agent-task run service: discovery/liveness reporting, stale-run
//! reconciliation, plan execution lifecycle, and the deterministic cook
//! orchestration cycle. Split out of a former single-file god-module into
//! concern submodules; this `mod.rs` only wires the submodules together and
//! re-exports their public surface so existing call sites keep resolving
//! `crate::core::agent_task_service::*` unchanged.

mod cook;
mod discovery;
mod execution;
mod reconcile;

pub use cook::*;
pub use discovery::*;
pub use execution::*;
pub use reconcile::*;

#[cfg(test)]
mod tests;

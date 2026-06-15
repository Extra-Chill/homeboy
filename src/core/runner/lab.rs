//! Lab offload service boundary.
//!
//! The orchestration is split across focused submodules so each concern stays
//! close to its tests:
//!
//! - `offload` — request execution + runner-selection orchestration
//!   (`execute_lab_offload`, `LabOffloadRequest`, fallback policy, workspace
//!   sync mode decisions).
//! - `trace_fetch_refs` — trace compare target materialization refs for git
//!   workspace sync.
//! - `agent_task_bridge` — inline AgentTask plan remapping and run-plan
//!   lifecycle mirroring.
//! - `secrets` — command-specific secret env hydration (agent-task vs trace).
//! - `evidence` — terminal Lab run evidence discovery for the idempotency
//!   guard.
//! - `args_util` — minimal argv inspection helpers shared by the other
//!   submodules.
//!
//! `core::runners::execute_lab_offload` remains the public facade entry point;
//! everything else is internal to the runner module.

mod agent_task_bridge;
mod args_util;
mod evidence;
mod offload;
pub(super) mod secrets;
mod trace_fetch_refs;

pub use super::lab_selection::LabRunnerSelectionSource;
pub use offload::{
    execute_lab_offload, LabOffloadCommand, LabOffloadOutcome, LabOffloadRequest,
    LabOffloadWorkspaceModePolicy,
};

#[cfg(test)]
#[path = "lab_arg_tests.rs"]
mod lab_arg_tests;

#[cfg(test)]
mod preparation_tests;

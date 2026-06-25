//! Agent task loop controller.
//!
//! Split from a single god-file into concern-focused submodules. This parent
//! module is a minimal aggregation layer: it declares the submodules and
//! re-exports their public items so existing call sites referencing
//! `crate::core::agent_task_loop_controller::*` keep working unchanged.

pub use crate::core::agent_task_loop_runner_policy::{
    AgentTaskLoopLocalFallbackPolicy, AgentTaskLoopRunnerAvailability,
    AgentTaskLoopRunnerExecutionTarget, AgentTaskLoopRunnerPolicy,
    AgentTaskLoopRunnerPolicyDecision,
};

mod defaults;
mod diagnostics;
mod gate;
mod helpers;
mod policy;
mod record_impl;
mod service;
mod types;

pub use defaults::*;
pub use diagnostics::*;
pub use gate::*;
pub(crate) use helpers::*;
pub use policy::*;
pub use service::*;
pub use types::*;

#[cfg(test)]
mod tests;

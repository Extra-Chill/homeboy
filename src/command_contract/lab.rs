//! Lab portability contract public surface.

mod handoff;
mod placement;
mod support;
#[cfg(test)]
mod tests;
mod types;
mod workload;

pub use handoff::*;
pub use support::*;
pub use types::*;
pub use workload::*;

pub(crate) use placement::{
    agent_task_controller_materializes_worktree, agent_task_lab_extension_ids,
    agent_task_provider_requires_cwd_git_checkout,
    agent_task_provider_requires_cwd_git_checkout_with, apply_lab_contract_to_descriptor,
    review_lab_extension_ids, AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON,
    AGENT_TASK_FANOUT_COORDINATOR_CONTROLLER_REASON,
};

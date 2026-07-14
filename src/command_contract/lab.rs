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

pub(crate) use placement::apply_lab_contract_to_descriptor;
#[cfg(test)]
pub(crate) use placement::{
    AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON, AGENT_TASK_FANOUT_COORDINATOR_CONTROLLER_REASON,
};

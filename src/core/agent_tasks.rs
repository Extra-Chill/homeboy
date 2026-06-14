//! Stable facade for agent task orchestration APIs.
//!
//! New command/core code should import agent task contracts from this module
//! instead of depending on the underlying implementation file layout.

#![allow(ambiguous_glob_reexports)]

pub use super::agent_task::*;
pub use super::agent_task_aggregate::*;
pub use super::agent_task_cook_loop::*;
pub use super::agent_task_fanout::*;
pub use super::agent_task_finalization::*;
pub use super::agent_task_gate::*;
pub use super::agent_task_lifecycle::*;
pub use super::agent_task_loop_controller::*;
pub use super::agent_task_promotion::*;
pub use super::agent_task_provider::*;
pub use super::agent_task_schedule::*;
pub use super::agent_task_scheduler::*;
pub use super::agent_task_secrets::*;
pub use super::agent_task_service::*;

pub mod cook_loop {
    pub use super::super::agent_task_cook_loop::*;
}

pub mod finalization {
    pub use super::super::agent_task_finalization::*;
}

pub mod gate {
    pub use super::super::agent_task_gate::*;
}

pub mod lifecycle {
    pub use super::super::agent_task_lifecycle::*;
}

pub mod loop_controller {
    pub use super::super::agent_task_loop_controller::*;
}

pub mod promotion {
    pub use super::super::agent_task_promotion::*;
}

pub mod provider {
    pub use super::super::agent_task_provider::*;
}

pub mod scheduler {
    pub use super::super::agent_task_scheduler::*;
}

pub mod secrets {
    pub use super::super::agent_task_secrets::*;
}

pub mod service {
    pub use super::super::agent_task_service::*;
}

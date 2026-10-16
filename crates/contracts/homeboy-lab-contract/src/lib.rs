//! Shared Lab/runner contract types for homeboy.
//!
//! This crate holds the contract surface that both the core engine and the
//! CLI/command-contract layer depend on: Lab workload/handoff and agent-task
//! policy types. Generic runner materialization contracts are compatibility
//! re-exports from `homeboy-runner-contract`.
//!
//! Lab runner routing, placement, and capability-handshake contracts live in
//! the private `runner` module and are re-exported at the crate root.

pub mod agent_task_config;
pub mod agent_task_outcome;
pub mod env_materialization_plan;
pub mod materialization_currency;
pub mod notification_payload;
pub mod notification_route;
pub mod path_materialization;
pub mod secret_env_plan;

mod runner;
pub use runner::*;

pub mod lab {
    pub mod execution_envelope;
    pub mod handoff;
    pub mod labels;
    pub mod transport_failure;
    pub mod types;
    pub mod workload;
}

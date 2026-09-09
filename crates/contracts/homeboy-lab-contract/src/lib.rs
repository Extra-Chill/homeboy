//! Shared Lab/runner contract types for homeboy.
//!
//! This crate holds the contract surface that both the core engine and the
//! CLI/command-contract layer depend on: Lab workload/handoff and agent-task
//! policy types. Generic runner materialization contracts are compatibility
//! re-exports from `homeboy-runner-contract`.

pub mod agent_task_config;
pub mod agent_task_outcome;
pub mod env_materialization_plan;
pub mod materialization_currency;
pub mod notification_payload;
pub mod notification_route;
pub mod path_materialization;
pub mod secret_env_plan;

pub mod lab {
    pub mod execution_envelope;
    pub mod handoff;
    pub mod labels;
    pub mod transport_failure;
    pub mod types;
    pub mod workload;
}

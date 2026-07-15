//! Shared Lab/runner contract types for homeboy.
//!
//! This crate holds the contract surface that both the core engine and the
//! CLI/command-contract layer depend on: the Lab workload/handoff types plus the
//! env/secret/path materialization plans and agent-task config/policy data they
//! reference. Extracting it below both `core` and `command_contract` breaks the
//! former `core <-> command_contract` dependency cycle.

pub mod agent_task_config;
pub mod agent_task_outcome;
pub mod env_materialization_plan;
pub mod notification_route;
pub mod path_materialization;
pub mod secret_env_plan;

pub mod lab {
    pub mod handoff;
    pub mod labels;
    pub mod types;
    pub mod workload;
}

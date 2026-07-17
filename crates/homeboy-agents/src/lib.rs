//! Agent-task subsystem for homeboy.
//!
//! Durable agent-task orchestration, controller loops, dispatch, promotion,
//! finalization, and review. This crate depends on `homeboy-core`; core does
//! not depend on it. Agent tasks can use runners (Lab offload) but do not
//! depend on the runner crate — that orthogonality is enforced by the crate
//! boundary.

pub mod agent_task;
pub mod agent_task_aggregate;
pub mod agent_task_artifacts;
pub mod agent_task_batch;
pub mod agent_task_candidate_baseline;
pub mod agent_task_config_materialization;
pub mod agent_task_contract;
pub mod agent_task_controller_service;
pub mod agent_task_cook_loop;
pub mod agent_task_deterministic_loop;
pub mod agent_task_dispatch_plan;
pub mod agent_task_dispatch_service;
pub mod agent_task_executor_evidence;
pub mod agent_task_fanout;
pub mod agent_task_finalization;
pub mod agent_task_gate;
pub mod agent_task_gate_executor;
pub mod agent_task_lifecycle;
pub mod agent_task_loop_controller;
pub mod agent_task_loop_definition;
pub mod agent_task_loop_runner_policy;
pub mod agent_task_promotion;
pub mod agent_task_prompts;
pub mod agent_task_provider;
pub mod agent_task_repo_loop_compile;
pub mod agent_task_review_dossier;
pub mod agent_task_runtime_dependency_graph;
pub mod agent_task_schedule;
pub mod agent_task_scheduler;
pub mod agent_task_secrets;
pub mod agent_task_service;
pub mod agent_task_timeout;
pub mod agent_task_timeout_artifacts;
pub mod agent_tasks;
pub mod agent_tool_control_plane;
pub mod controller_runtime;
pub mod controller_scratch;

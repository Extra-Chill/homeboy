// Stable domain facades for new command/core integrations.
pub mod agent_tasks;
pub mod artifacts;
pub mod runners;

// Public extensions (config first — exports entity_crud! macro used by entity extensions)
#[macro_use]
pub mod config;

// Compatibility exports for existing `homeboy::core::<module>` consumers. Prefer the
// facade modules above for new code so implementation files can move without
// becoming accidental public API.
pub mod agent_runtime_manifest;
pub mod agent_task;
pub mod agent_task_aggregate;
pub(crate) mod agent_task_config_materialization;
pub mod agent_task_controller_service;
pub mod agent_task_cook_loop;
pub mod agent_task_dispatch_service;
pub mod agent_task_fanout;
pub mod agent_task_finalization;
pub mod agent_task_gate;
pub mod agent_task_lifecycle;
pub mod agent_task_loop_controller;
mod agent_task_pr_body;
pub mod agent_task_promotion;
pub mod agent_task_provider;
pub mod agent_task_schedule;
pub mod agent_task_scheduler;
pub mod agent_task_secrets;
pub mod agent_task_service;
pub(crate) mod agent_task_timeout;
pub(crate) mod agent_task_timeout_artifacts;
pub mod api_jobs;
pub mod artifact_address;
pub mod artifact_contract;
pub mod artifact_inputs;
pub mod artifact_links;
pub mod artifact_manifest;
pub(crate) mod artifact_metadata;
pub mod artifact_origin;
pub mod artifact_ref;
pub mod browser_evidence;
pub mod build_identity;
pub mod change_artifact;
pub mod ci_profile;
pub mod cleanup;
pub mod code_audit;
pub mod component;
pub mod context;
pub mod daemon;
pub mod db;
pub mod deploy;
pub mod deps;
pub mod engine;
pub mod error;
pub mod evidence_manifest;
pub mod execution;
pub mod execution_contract;
pub(crate) mod expand;
pub mod extension;
pub mod finding;
pub mod fleet;
pub mod gate;
pub mod git;
pub mod http_api;
pub(crate) mod http_probe;
pub mod http_request;
pub mod hygiene;
pub(crate) mod io;
pub mod issues;
pub mod keychain;
pub mod lab_routing;
pub mod observation;
pub mod output;
pub(crate) mod ownership;
pub mod plan;
pub mod preview_client;
pub mod preview_ingress;
#[cfg(test)]
mod preview_ingress_tests;
pub mod process;
pub mod product_identity;
pub mod project;
pub mod proof;
pub mod publication_artifacts;
pub mod quality;
pub mod redaction;
pub mod refactor;
pub mod release;
pub mod review;
pub mod rig;
pub mod run_lifecycle_record;
pub mod runner;
pub mod scope;
pub mod secret_env_plan;
pub mod self_status;
pub mod server;
pub mod source_snapshot;
pub mod stack;
pub mod structured_sidecar;
pub mod top_n;
pub mod trace_secrets;
pub mod triage;
pub mod tunnel;
#[cfg(test)]
mod tunnel_tests;
pub mod update_check_cache;
pub mod upgrade;
pub mod validation_progress;
pub mod worktree;

// Internal path resolution helpers.
pub(crate) mod paths;

// Public extensions for CLI access
pub mod defaults;

pub use extension::build;

// Re-export relocated modules so existing `homeboy::api`, `homeboy::auth`, etc. paths keep working.
// Consumers within the crate have been updated to canonical paths; these re-exports
// preserve the public API for external users of the library.
pub use code_audit::codebase_map;
pub use engine::cli_tool;
pub use engine::hooks;
pub use server::api;
pub use server::auth;

// Re-export common types for convenience
pub use error::{Error, ErrorCode, Result};
pub use output::{
    BatchResult, BatchResultItem, BulkResult, BulkResultBuilder, BulkSummary, CreateOutput,
    CreateResult, EntityCrudOutput, ItemOutcome, MergeOutput, MergeResult, NoExtra,
    ObservationOutputDetails, ObservationOutputMetadata, OutcomeTotals, RemoveResult,
};

/// Set a process-local artifact root override for the current CLI invocation.
pub fn set_artifact_root_override(path: Option<std::path::PathBuf>) {
    paths::set_artifact_root_override(path);
}

/// Resolve the artifact root used for copied/downloaded run artifacts.
pub fn artifact_root() -> Result<std::path::PathBuf> {
    paths::artifact_root()
}

/// Resolve a remote path against an optional project base path.
pub fn join_remote_path(base_path: Option<&str>, path: &str) -> Result<String> {
    paths::join_remote_path(base_path, path)
}

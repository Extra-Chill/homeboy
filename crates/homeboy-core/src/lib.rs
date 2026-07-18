/// Macro for prefixed status logging to stderr (only when stderr is a terminal).
///
/// Usage:
/// ```ignore
/// log_status!("deploy", "Uploading {} to {}", artifact, server);
/// log_status!("release", "Version bumped to {}", version);
/// ```
#[macro_export]
macro_rules! log_status {
    ($prefix:expr, $($arg:tt)*) => {
        if ::std::io::IsTerminal::is_terminal(&::std::io::stderr()) {
            eprintln!(concat!("[", $prefix, "] {}"), format_args!($($arg)*));
        }
    };
}

/// Helper for `#[serde(skip_serializing_if = "is_zero")]` on `usize` fields.
pub fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Helper for `#[serde(skip_serializing_if = "is_zero_u32")]` on `u32` fields.
pub fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

// Included legacy tests retain their pre-extraction crate paths without exposing
// a compatibility surface from the production homeboy-core package.
#[cfg(test)]
extern crate self as homeboy;
#[cfg(test)]
pub use crate as core;
#[cfg(test)]
pub use lab_contract as command_contract;

// Stable domain facades for new command/core integrations.
pub mod artifacts;

// Public extensions (config first — exports entity_crud! macro used by entity extensions)
#[macro_use]
pub mod config;

// Compatibility exports for existing `crate::<module>` consumers. Prefer the
// facade modules above for new code so implementation files can move without
// becoming accidental public API.
pub mod activity;
pub mod agent_runtime_manifest;
pub mod agent_task_secret_provider;
pub use homeboy_lab_contract::agent_task_config;
pub mod api_jobs;
pub mod artifact_address;
pub mod artifact_contract;
pub mod artifact_dom_boxes;
pub mod artifact_inputs;
pub mod artifact_links;
pub mod artifact_manifest;
pub mod artifact_metadata;
pub mod artifact_origin;
pub mod artifact_postprocess;
pub mod artifact_preview;
pub mod artifact_ref;
pub mod broker_auth;
pub mod browser_evidence;
pub mod browser_visual_compare;
pub mod build_identity;
pub mod change_artifact;
pub mod ci_failure_log_triage;
pub mod ci_gate;
pub mod ci_plan;
pub mod ci_profile;
pub mod ci_scope;
pub mod cleanup;
// code_audit lives in its own crate (homeboy-code-audit). Re-exported as
// `crate::code_audit` so the in-crate consumers and the provider impls (which
// reference code_audit's provider traits) keep working unchanged.
pub use homeboy_code_audit as code_audit;
pub mod command_execution_plan;
pub use homeboy_command_contract as command_invocation;
pub mod component;
pub mod context;
pub mod controller_pin_reference;
pub mod controller_runtime;
pub mod daemon;
pub mod db;
pub mod deps;
pub mod deterministic_loop;
pub mod engine;
pub use homeboy_lab_contract::env_materialization_plan;
// error moved to the internal `homeboy-error` crate. Re-exported here so existing
// `crate::error::*` call sites keep working unchanged.
pub use homeboy_error as error;
pub mod build_artifact_path;
pub mod component_build_provider;
pub mod component_install_provider;
pub mod component_script_provider;
pub mod evidence_manifest;
pub mod execution;
pub mod execution_contract;
pub mod expand;
pub mod extension_execution;
pub mod extension_invocation_context;
pub mod extension_provider_discovery;
pub mod extension_readiness;
pub mod extension_scope;
pub mod extension_store;
pub mod extension_update_check;
// finding moved to the internal `homeboy-finding` crate. Re-exported so existing
// `crate::finding::*` call sites keep working unchanged.
pub use homeboy_finding as finding;
pub mod fleet;
pub use homeboy_gate_contract::gate;
pub mod gate_feedback_baseline;
pub mod gh_actions_cache;
pub mod git;
pub mod host_mutation_lifecycle;
pub mod http_api;
pub mod http_probe;
pub mod http_request;
pub mod hygiene;
pub mod io;
pub mod keychain;
pub mod lab_offload;
pub mod lab_routing;
pub mod lab_workspace_provenance;
pub mod local_permissions;
pub use homeboy_lifecycle_contract::lifecycle;
pub mod loop_lifecycle;
pub mod markdown;
pub mod matrix_artifact_summary;
pub use homeboy_lab_contract::notification_route;
pub mod notify;
pub mod observation;
// output moved to the internal `homeboy-output` crate. Re-exported so existing
// `crate::output::*` call sites keep working unchanged.
pub use homeboy_output as output;
pub(crate) mod ownership;
pub use homeboy_lab_contract::path_materialization;
pub mod performance_hotspots;
// `phase_timing` (PhaseTimer/PhaseSpan/PhaseStatus/PhaseTimingReport) is a
// std-only timing primitive shared by deploy, release, and the audit engine. It
// lives in homeboy-engine-primitives so a future homeboy-code-audit crate can
// depend on the slim primitives base rather than all of homeboy-core; this
// re-export keeps the existing crate::phase_timing path working.
pub use homeboy_engine_primitives::phase_timing;
pub use homeboy_gate_contract::plan;
// process moved to the internal `homeboy-process` crate. Re-exported so existing
// `crate::process::*` call sites keep working unchanged.
pub use homeboy_process as process;
// product_identity moved to the internal `homeboy-product-identity` crate.
// Re-exported so `crate::product_identity::*` call sites keep working.
pub use homeboy_product_identity as product_identity;
pub mod project;
pub mod proof;
pub mod publication_artifacts;
pub mod quality;
// redaction moved to the internal `homeboy-redaction` crate. Re-exported here so
// existing `crate::redaction::*` call sites keep working unchanged.
pub use homeboy_redaction as redaction;
pub mod refactor_transform_provider;
pub mod release_provider;
pub mod release_set;
pub mod report_compare;
pub(crate) mod report_compare_render;
pub mod resource_cleanup_intent;
pub mod resource_lifecycle_index;
pub mod resource_policy_context;
pub mod resources;
pub mod rig_provider;
pub mod rig_toolchain_provider;
pub use homeboy_run_lifecycle_contract as run_lifecycle_record;
pub mod run_lifecycle_status;
pub mod run_outcome_envelope;
pub mod runner_execution_envelope;
pub mod runtime_package;
pub mod runtime_promotion;
pub mod scope;
pub mod stack_provider;
pub use homeboy_lab_contract::secret_env_plan;

/// Flattened re-export of the lab-contract crate's Lab types (workload, handoff,
/// typed identifiers, labels). Core consumers import these from here rather than
/// from `command_contract`, so no core module depends upward on the CLI layer —
/// this is what breaks the former `core <-> command_contract` cycle.
pub mod lab_contract {
    pub use homeboy_lab_contract::lab::handoff::*;
    pub use homeboy_lab_contract::lab::labels::*;
    pub use homeboy_lab_contract::lab::types::*;
    pub use homeboy_lab_contract::lab::workload::*;
}
pub mod server;
pub mod setup;
pub mod source_snapshot;
pub mod stream_capture;
pub mod structured_sidecar;
pub mod tag_gap;
pub mod top_n;
pub mod trace_compare;
pub mod trace_secrets;
pub(crate) mod transient_workspace_policy;

/// Test-only fixtures and hermetic process contexts, shared across the workspace
/// (core, cli, and feature crates all rely on the same isolation contract).
/// Compiled for core's own tests and, via the `test-support` feature, for the
/// test builds of crates that depend on core.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
#[allow(dead_code)]
pub mod test_support;
pub mod update_check_cache;
pub mod validation_progress;
pub mod workspace_snapshot;
pub mod worktree;
pub mod worktree_providers;

// Internal path resolution helpers.
// paths moved to the internal `homeboy-paths` crate. Re-exported so existing
// `crate::paths::*` call sites keep working unchanged.
pub use homeboy_paths as paths;
#[cfg(test)]
mod paths_tests;

// Public extensions for CLI access
pub mod defaults;

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

/// Expand a leading tilde in a local path.
pub fn expand_tilde_path(path: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    paths::expand_tilde_path(path)
}

/// Resolve a remote path against an optional project base path.
pub fn join_remote_path(base_path: Option<&str>, path: &str) -> Result<String> {
    paths::join_remote_path(base_path, path)
}

/// Normalize a local path lexically without touching the filesystem.
pub fn normalize_local_path(path: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    paths::normalize_local_path(path)
}

/// Return whether `path` is inside `root` after lexical normalization.
pub fn local_path_is_contained(
    root: impl AsRef<std::path::Path>,
    path: impl AsRef<std::path::Path>,
) -> bool {
    paths::local_path_is_contained(root, path)
}

/// Resolve a local path against a root and reject paths that escape that root.
pub fn resolve_contained_local_path(
    root: impl AsRef<std::path::Path>,
    candidate: impl AsRef<std::path::Path>,
    field: &str,
) -> Result<std::path::PathBuf> {
    paths::resolve_contained_local_path(root, candidate, field)
}

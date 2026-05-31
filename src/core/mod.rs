// Public extensions (config first — exports entity_crud! macro used by entity extensions)
#[macro_use]
pub mod config;
pub mod api_jobs;
pub mod artifact_manifest;
pub(crate) mod artifact_metadata;
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
pub mod execution;
pub(crate) mod expand;
pub mod extension;
pub mod finding;
pub mod fleet;
pub mod git;
pub mod http_api;
pub(crate) mod http_probe;
pub mod http_request;
pub(crate) mod io;
pub mod issues;
pub mod keychain;
pub mod observation;
pub mod output;
pub mod plan;
pub mod process;
pub mod project;
pub mod quality;
pub mod redaction;
pub mod refactor;
pub mod release;
pub mod rig;
pub mod runner;
pub mod scope;
pub mod self_status;
pub mod server;
pub mod source_snapshot;
pub mod stack;
pub mod top_n;
pub mod triage;
pub mod tunnel;
pub mod update_check_cache;
pub mod upgrade;

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

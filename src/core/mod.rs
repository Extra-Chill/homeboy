// Public extensions (config first — exports entity_crud! macro used by entity extensions)
#[macro_use]
pub mod config;
pub mod api;
pub mod auth;
pub mod build;
pub mod changelog;
pub mod cleanup;
pub mod cli_tool;
pub mod code_audit;
pub mod component;
pub mod context;
pub mod db;
pub mod deploy;
pub mod docs_audit;
pub mod engine;
pub mod error;
pub mod extension;
pub mod extension_update_check;
pub mod files;
pub mod fleet;
pub mod git;
pub mod hooks;
pub mod lint_baseline;
pub mod logs;
pub mod output;
pub mod project;
pub mod refactor;
pub mod release;

pub mod server;
pub mod ssh;
pub mod test_analyze;
pub mod test_baseline;
pub mod test_drift;
pub mod test_scaffold;
pub mod update_check;
pub mod upgrade;
pub mod version;

// Internal extensions - not part of public API
pub(crate) mod http;
pub(crate) mod keychain;
pub(crate) mod local_files;
pub(crate) mod paths;
pub(crate) mod permissions;

// Public extensions for CLI access
pub mod defaults;

// Re-export common types for convenience
pub use error::{Error, ErrorCode, Result};
pub use output::{
    BatchResult, BatchResultItem, BulkResult, BulkSummary, CreateOutput, CreateResult,
    EntityCrudOutput, ItemOutcome, MergeOutput, MergeResult, NoExtra, RemoveResult,
};

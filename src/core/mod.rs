// Public extensions (config first — exports entity_crud! macro used by entity extensions)
#[macro_use]
pub mod config;
pub mod code_audit;
pub mod component;
pub mod context;
pub mod daemon;
pub mod db;
pub mod deploy;
pub mod engine;
pub mod error;
pub(crate) mod expand;
pub mod extension;
pub mod fleet;
pub mod git;
pub mod issues;
pub mod output;
pub mod project;
pub mod refactor;
pub mod release;
pub mod rig;
pub mod server;
pub mod stack;
pub mod top_n;
pub mod triage;
pub mod upgrade;

// Internal extensions - not part of public API
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
    BatchResult, BatchResultItem, BulkResult, BulkSummary, CreateOutput, CreateResult,
    EntityCrudOutput, ItemOutcome, MergeOutput, MergeResult, NoExtra, RemoveResult,
};

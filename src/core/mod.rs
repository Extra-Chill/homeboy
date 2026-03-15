// Public extensions (config first — exports entity_crud! macro used by entity extensions)
#[macro_use]
pub mod config;
pub mod code_audit;
pub mod component;
pub mod context;
pub mod db;
pub mod deploy;
pub mod engine;
pub mod error;
pub mod extension;
pub mod fleet;
pub mod git;
pub mod output;
pub mod project;
pub mod refactor;
pub mod release;
pub mod scaffold;
pub mod server;
pub mod undo;
pub mod upgrade;

// Internal extensions - not part of public API
pub(crate) mod local_files;
pub(crate) mod paths;

// Public extensions for CLI access
pub mod defaults;

pub use extension::build;

// Re-export relocated modules so existing `homeboy::api`, `homeboy::auth`, etc. paths keep working.
// Also re-exports for internal `crate::http`, `crate::hooks`, `crate::permissions` usage.
pub use code_audit::codebase_map;
pub use code_audit::docs;
pub(crate) use deploy::permissions;
pub use engine::cli_tool;
pub use engine::hooks;
pub use server::api;
pub use server::auth;
pub(crate) use server::http;

// Re-export common types for convenience
pub use error::{Error, ErrorCode, Result};
pub use output::{
    BatchResult, BatchResultItem, BulkResult, BulkSummary, CreateOutput, CreateResult,
    EntityCrudOutput, ItemOutcome, MergeOutput, MergeResult, NoExtra, RemoveResult,
};

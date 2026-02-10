// Public modules
pub mod api;
pub mod auth;
pub mod build;
pub mod changelog;
pub mod cli_tool;
pub mod component;
pub mod config;
pub mod context;
pub mod db;
pub mod deploy;
pub mod docs_audit;
pub mod engine;
pub mod error;
pub mod files;
pub mod fleet;
pub mod git;
pub mod logs;
pub mod module;
pub mod output;
pub mod project;
pub mod release;


pub mod server;
pub mod ssh;
pub mod upgrade;
pub mod version;

// Internal modules - not part of public API
pub(crate) mod http;
pub(crate) mod keychain;
pub(crate) mod local_files;
pub(crate) mod paths;
pub(crate) mod permissions;

// Public modules for CLI access
pub mod defaults;

// Re-export common types for convenience
pub use error::{Error, ErrorCode, Result};
pub use output::{
    BatchResult, BatchResultItem, BulkResult, BulkSummary, CreateOutput, CreateResult, ItemOutcome,
    MergeOutput, MergeResult, RemoveResult,
};

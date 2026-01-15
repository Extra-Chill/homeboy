// Public modules
pub mod api;
pub mod auth;
pub mod build;
pub mod changelog;
pub mod cli_tool;
pub mod component;
pub mod context;
pub mod db;
pub mod deploy;
pub mod error;
pub mod executor;
pub mod files;
pub mod git;
pub mod logs;
pub mod module;
pub mod output;
pub mod project;
pub mod release;
pub mod server;
pub mod shell;
pub mod ssh;
pub mod token;
pub mod upgrade;
pub mod version;

// Internal modules - not part of public API
pub(crate) mod base_path;
pub(crate) mod config;
pub(crate) mod http;
pub(crate) mod keychain;
pub(crate) mod local_files;
pub(crate) mod paths;
pub(crate) mod permissions;
pub(crate) mod slugify;
pub(crate) mod template;

// Public modules for CLI access
pub mod defaults;

// Re-export common types for convenience
pub use error::{Error, ErrorCode, Result};
pub use output::{
    BatchResult, BatchResultItem, BulkResult, BulkSummary, CreateOutput, CreateResult, ItemOutcome,
    MergeOutput, MergeResult, RemoveResult,
};

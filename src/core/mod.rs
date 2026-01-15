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
pub mod git;
pub mod logs;
pub mod module;
pub mod project;
pub mod remote_files;
pub mod server;
pub mod ssh;
pub mod token;
pub mod upgrade;
pub mod version;

// Internal modules - not part of public API
pub(crate) mod http;
pub(crate) mod json;
pub(crate) mod keychain;
pub(crate) mod local_files;
pub(crate) mod paths;
pub(crate) mod shell;
pub(crate) mod template;

pub(crate) mod base_path;
pub(crate) mod config;
pub(crate) mod slugify;

// Re-exports for convenient access
pub use config::{BatchResult, MergeOutput};
pub use error::{Error, ErrorCode, Result};
pub use json::{BulkResult, BulkSummary, ItemOutcome, MergeResult};

//! Generic utility primitives with zero domain knowledge.
//!
//! - `args` - CLI argument normalization
//! - `base_path` - Remote path joining utilities
//! - `command` - Command execution with error handling
//! - `entity_suggest` - Entity suggestion for unrecognized CLI subcommands
//! - `io` - File I/O with consistent error handling
//! - `parser` - Text extraction and manipulation
//! - `resolve` - Project/component argument resolution
//! - `shell` - Shell escaping and quoting
//! - `slugify` - String slug generation
//! - `template` - String template rendering
//! - `token` - String comparison and normalization
//! - `validation` - Input validation helpers

pub mod args;
pub mod artifact;
pub mod base_path;
pub mod command;
pub mod entity_suggest;
pub mod io;
pub mod parser;
pub mod resolve;
pub mod shell;
pub mod slugify;
pub mod token;
pub mod validation;
pub(crate) mod template;

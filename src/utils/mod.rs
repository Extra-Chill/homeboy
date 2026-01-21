//! Generic utility primitives with zero domain knowledge.
//!
//! - `base_path` - Remote path joining utilities
//! - `command` - Command execution with error handling
//! - `io` - File I/O with consistent error handling
//! - `parser` - Text extraction and manipulation
//! - `shell` - Shell escaping and quoting
//! - `slugify` - String slug generation
//! - `template` - String template rendering
//! - `token` - String comparison and normalization
//! - `validation` - Input validation helpers

pub mod base_path;
pub mod command;
pub mod io;
pub mod parser;
pub mod shell;
pub mod slugify;
pub mod token;
pub mod validation;
pub(crate) mod template;

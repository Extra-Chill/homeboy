//! Generic utility primitives with zero domain knowledge.
//!
//! - `command` - Command execution with error handling
//! - `parser` - Text extraction and manipulation
//! - `shell` - Shell escaping and quoting
//! - `template` - String template rendering
//! - `token` - String comparison and normalization

pub mod command;
pub mod parser;
pub mod shell;
pub mod token;
pub(crate) mod template;

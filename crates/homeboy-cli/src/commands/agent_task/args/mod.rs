//! Clap argument and subcommand definitions for the `agent-task` command tree.
//!
//! Definitions live in focused child modules so this root remains the stable
//! import surface for command handlers.

mod auth;
mod definitions;

pub use definitions::*;

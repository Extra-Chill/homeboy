//! Pure serializable command-invocation contract types.
//!
//! These behavior-free data structures describe the shape of a command
//! invocation (argv, cwd, env references, redaction) shared across homeboy.
//! They depend only on serde, which keeps this a leaf crate other crates can
//! depend on without pulling in core.

pub mod command_invocation;

pub use command_invocation::{
    CommandEnvRef, CommandInvocation, CommandRedaction, COMMAND_INVOCATION_SCHEMA,
};

//! Command-runtime infrastructure.
//!
//! This module is the command-dispatch runtime layer: routing, adapters,
//! response/output handling, summary helpers, and manifests that turn a parsed
//! `Commands` value into a dispatched, serialized result. It is a deliberate
//! architectural boundary, distinct from the per-command modules in
//! `crate::commands`, each of which owns exactly one user-facing command.
//! Shared dispatch/runtime plumbing belongs here by design; new user-facing
//! commands do not.
//!
//! All items remain reachable at their original `crate::commands::*` paths via
//! re-exports in `crate::commands` (see `src/commands/mod.rs`), so callers
//! import them unchanged.

pub(crate) mod adapter;
pub mod cli;
pub(crate) mod key_artifacts;
pub mod manifest;
pub mod output_runtime;
pub mod response;
pub mod route;
pub(crate) mod runs_dossier_summary;
pub mod runtime;
pub mod source_command;
pub(crate) mod summary_json;

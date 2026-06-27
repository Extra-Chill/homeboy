//! CLI-infrastructure / plumbing modules.
//!
//! These modules are command-runtime infrastructure (routing, adapters,
//! response/output handling, summary helpers, manifests) rather than
//! individual user-facing commands. They were grouped here to keep the
//! `src/commands/` directory under the structural file-count threshold.
//!
//! All items remain reachable at their original `crate::commands::*` paths
//! via re-exports in `crate::commands` (see `src/commands/mod.rs`), so this
//! is a pure relocation with zero API change.

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

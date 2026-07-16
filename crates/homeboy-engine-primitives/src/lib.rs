//! Low-level execution primitives extracted from the homeboy engine.
//!
//! These modules are leaf utilities (shell quoting, command construction,
//! text helpers, run-directory management, templating, output parsing, and
//! identifier helpers) that depend only on `homeboy-error`. They live in their
//! own crate so they compile as an independent unit and are re-exported under
//! `crate::core::engine::*` in the main binary for source compatibility.

pub mod baseline;
pub mod codebase_scan;
pub mod command;
pub mod detail_output;
pub mod edit_op;
pub mod edit_op_apply;
pub mod grammar;
pub mod identifier;
pub mod language;
pub mod local_files;
pub mod output_parse;
pub mod shell;
pub mod template;
pub mod test_path;
pub mod text;
pub mod validation;

//! Low-level execution primitives extracted from the homeboy engine.
//!
//! These modules are leaf utilities (shell quoting, command construction,
//! text helpers, run-directory management, templating, output parsing, and
//! identifier helpers) that depend only on `homeboy-error`. They live in their
//! own crate so they compile as an independent unit and are re-exported under
//! `crate::core::engine::*` in the main binary for source compatibility.

pub mod command;
pub mod identifier;
pub mod output_parse;
pub mod shell;
pub mod template;
pub mod text;

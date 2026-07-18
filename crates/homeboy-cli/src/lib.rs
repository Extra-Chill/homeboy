//! CLI layer for homeboy.
//!
//! Command dispatch (`commands`), the clap argument surface (`cli_surface`), the
//! command contract / Lab portability layer (`command_contract`), runtime wiring
//! (`cli_runtime`), and help topics. Sits on top of the `homeboy-core` engine
//! crate; the thin `homeboy` binary wires this together with `main`.

extern crate self as homeboy;

// The core engine lives in the homeboy-core crate. Re-exported as `core` so the
// existing `crate::core::*` call sites across this layer are unchanged.
pub use homeboy_agents as agents;
pub use homeboy_core as core;
pub use homeboy_refactor as refactor;
pub use homeboy_release as release;
pub use homeboy_review as review;
pub use homeboy_rig as rig;
pub use homeboy_stack as stack;

// The optional Lab-offload runner subsystem lives in the homeboy-runner crate.
// Re-exported as `runner` so CLI call sites reach it via `crate::runner::*` /
// `homeboy::runner::*` and register its behavior with core at startup.
pub use homeboy_fuzz as fuzz;
pub use homeboy_issues as issues;
pub use homeboy_lab_runner as runner;

// Re-export the `log_status!` macro and is_zero serde helpers from homeboy-core
// so `crate::log_status!` / `homeboy::log_status!` / `crate::is_zero` call sites
// across this layer resolve at the crate root.
pub use homeboy_core::{is_zero, is_zero_u32, log_status};

pub mod cli_runtime;
pub mod cli_surface;
pub mod command_contract;
#[doc(hidden)]
pub mod commands;
pub mod help_topics;

// Test-only fixtures / hermetic process contexts live in homeboy-core (the whole
// workspace shares one isolation contract). Re-exported as `test_support` so this
// layer's `crate::test_support::*` call sites are unchanged. Available only in
// test builds (core exposes it via its `test-support` feature, which this crate
// enables as a dev-dependency).
#[cfg(test)]
#[doc(hidden)]
pub use homeboy_core::test_support;

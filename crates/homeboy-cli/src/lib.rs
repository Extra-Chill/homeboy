//! CLI layer for homeboy.
//!
//! Command dispatch (`commands`), the clap argument surface (`cli_surface`), the
//! command contract / Lab portability layer (`command_contract`), runtime wiring
//! (`cli_runtime`), and help topics. Sits on top of the `homeboy-core` engine
//! crate; the thin `homeboy` binary wires this together with `main`.

extern crate self as homeboy;

// The core engine lives in the homeboy-core crate. Re-exported as `core` so the
// existing `crate::core::*` call sites across this layer are unchanged.
pub use homeboy_core as core;

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

/// Test-only fixtures and hermetic process contexts.
#[doc(hidden)]
#[allow(dead_code)]
pub mod test_support;

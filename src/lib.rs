extern crate self as homeboy;

// The CLI layer (command dispatch, clap surface, command contract, runtime) now
// lives in the homeboy-cli crate; the core engine in homeboy-core. Re-export both
// so `homeboy::cli_runtime::*`, `homeboy::commands::*`, `homeboy::core::*` etc.
// call sites (including the binary entry point and integration tests) are
// unchanged.
pub use homeboy_cli::{
    agents, cli_runtime, cli_surface, command_contract, commands, core, help_topics, refactor, rig,
    runner,
};
pub use homeboy_core::{is_zero, is_zero_u32, log_status};

// Shared hermetic test fixtures live in homeboy-core (exposed via its
// `test-support` feature, enabled here as a dev-dependency). Re-exported so
// integration tests reach `homeboy::test_support::*` unchanged.
#[cfg(any(test, feature = "test-support"))]
pub use homeboy_core::test_support;

pub mod extensions;

/// Macro for prefixed status logging to stderr (only when stderr is a terminal).
///
/// Usage:
/// ```ignore
/// log_status!("deploy", "Uploading {} to {}", artifact, server);
/// log_status!("release", "Version bumped to {}", version);
/// ```
#[macro_export]
macro_rules! log_status {
    ($prefix:expr, $($arg:tt)*) => {
        if ::std::io::IsTerminal::is_terminal(&::std::io::stderr()) {
            eprintln!(concat!("[", $prefix, "] {}"), format_args!($($arg)*));
        }
    };
}

extern crate self as homeboy;

pub mod cli_runtime;
pub mod cli_surface;
pub mod command_contract;
#[doc(hidden)]
pub mod commands;
// Core engine now lives in the homeboy-core crate. Re-exported as `core` so the
// existing `crate::core::*` call sites across commands / command_contract / the
// CLI runtime are unchanged.
pub use homeboy_core as core;
pub mod extensions;
pub mod help_topics;

/// Test-only fixtures and hermetic process contexts.
///
/// This is public so integration tests can use the same isolation contract as
/// unit tests. It is hidden from normal API documentation and has no role in
/// production command execution.
#[doc(hidden)]
#[allow(dead_code)] // Unit-test-only helpers share this module with public CLI fixtures.
pub mod test_support;

/// Helper for `#[serde(skip_serializing_if = "is_zero")]` on `usize` fields.
pub fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Helper for `#[serde(skip_serializing_if = "is_zero_u32")]` on `u32` fields.
pub fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

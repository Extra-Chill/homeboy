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

#[doc(hidden)]
pub mod commands;
pub mod core;
pub mod help_topics;

/// Read-only Homeboy CLI command surface derived from the Clap command tree.
pub mod cli_surface {
    pub use crate::commands::surface::{
        command_surface_from, current_command_surface, CommandSurface, CommandSurfaceEntry,
    };
}

// Re-export everything from core for ergonomic library use
// Users can write `homeboy::config` instead of `homeboy::core::config`
pub use core::release::changelog;
pub use core::release::version;
pub use core::*;

/// Helper for `#[serde(skip_serializing_if = "is_zero")]` on `usize` fields.
pub fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Helper for `#[serde(skip_serializing_if = "is_zero_u32")]` on `u32` fields.
pub fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

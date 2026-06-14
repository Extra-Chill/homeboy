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
pub mod core;
pub mod extensions;
pub mod help_topics;

#[cfg(test)]
pub(crate) mod test_support;

/// Helper for `#[serde(skip_serializing_if = "is_zero")]` on `usize` fields.
pub fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Helper for `#[serde(skip_serializing_if = "is_zero_u32")]` on `u32` fields.
pub fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

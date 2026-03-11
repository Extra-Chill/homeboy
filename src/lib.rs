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

pub mod core;
pub mod utils;

// Re-export everything from core for ergonomic library use
// Users can write `homeboy::config` instead of `homeboy::core::config`
pub use core::*;
pub use core::release::changelog;
pub use core::release::version;
pub use utils::*;

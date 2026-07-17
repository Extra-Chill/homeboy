//! Audit-local status logging.
//!
//! The audit engine emits prefixed progress lines to stderr (only when stderr is
//! a terminal). It defines its own `log_status!` here rather than depending on a
//! crate-root macro so `code_audit` stays self-contained — a prerequisite for
//! extracting it into its own crate. The macro is identical in behavior to the
//! shared one used elsewhere in the workspace.

/// Prefixed status logging to stderr, emitted only when stderr is a terminal.
///
/// Usage: `log_status!("audit", "Scanning {}...", path);`
macro_rules! log_status {
    ($prefix:expr, $($arg:tt)*) => {
        if ::std::io::IsTerminal::is_terminal(&::std::io::stderr()) {
            eprintln!(concat!("[", $prefix, "] {}"), format_args!($($arg)*));
        }
    };
}

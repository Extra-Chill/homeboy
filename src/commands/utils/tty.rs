//! Terminal I/O utilities for CLI.
//!
//! Provides TTY detection and user prompting.

use std::io::{self, BufRead, IsTerminal, Write};

pub fn is_stdin_tty() -> bool {
    io::stdin().is_terminal()
}

pub fn is_stdout_tty() -> bool {
    io::stdout().is_terminal()
}

pub fn require_tty_for_interactive() -> bool {
    is_stdin_tty() && is_stdout_tty()
}

pub fn prompt(message: &str) -> homeboy::Result<String> {
    eprint!("{}", message);
    io::stderr().flush().ok();

    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).map_err(|e| {
        homeboy::Error::new(
            homeboy::ErrorCode::InternalIoError,
            format!("Failed to read input: {}", e),
            serde_json::Value::Null,
        )
    })?;

    Ok(line.trim().to_string())
}

pub fn prompt_password(message: &str) -> homeboy::Result<String> {
    prompt(message)
}

/// Print status message to stderr if running in a terminal.
pub fn status(message: &str) {
    if io::stderr().is_terminal() {
        eprintln!("{}", message);
    }
}

// log_status! macro is defined in lib.rs (#[macro_export]) and available crate-wide.

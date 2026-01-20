//! Command execution primitives with consistent error handling.

use std::process::{Command, Output};

use crate::error::{Error, Result};

/// Run a command and return stdout on success.
///
/// Returns trimmed stdout if the command succeeds.
/// Returns an error with stderr (or stdout fallback) if it fails.
pub fn run(program: &str, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::other(format!("Failed to run {}: {}", context, e)))?;

    if !output.status.success() {
        return Err(Error::other(format!("{} failed: {}", context, error_text(&output))));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run a command in a specific directory.
///
/// Returns trimmed stdout if the command succeeds.
/// Returns an error with stderr (or stdout fallback) if it fails.
pub fn run_in(dir: &str, program: &str, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| Error::other(format!("Failed to run {}: {}", context, e)))?;

    if !output.status.success() {
        return Err(Error::other(format!("{} failed: {}", context, error_text(&output))));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run a command in a directory, returning Ok(None) on failure instead of error.
///
/// Useful when command failure is expected/acceptable (e.g., checking for optional tags).
pub fn run_in_optional(dir: &str, program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

/// Extract error text from command output.
///
/// Prefers stderr, falls back to stdout if stderr is empty.
pub fn error_text(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

/// Check if a command succeeds without capturing output.
///
/// Returns true if the command exits with status 0.
pub fn succeeded(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if a command succeeds in a directory without capturing output.
pub fn succeeded_in(dir: &str, program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_succeeds_with_valid_command() {
        let result = run("echo", &["hello"], "echo test");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn run_fails_with_invalid_command() {
        let result = run("nonexistent_command_xyz", &[], "test");
        assert!(result.is_err());
    }

    #[test]
    fn succeeded_returns_true_for_valid_command() {
        assert!(succeeded("true", &[]));
    }

    #[test]
    fn succeeded_returns_false_for_failing_command() {
        assert!(!succeeded("false", &[]));
    }

    #[test]
    fn run_in_optional_returns_none_on_failure() {
        let result = run_in_optional("/tmp", "false", &[]);
        assert!(result.is_none());
    }

    #[test]
    fn error_text_prefers_stderr() {
        let output = Output {
            status: std::process::ExitStatus::default(),
            stdout: b"stdout content".to_vec(),
            stderr: b"stderr content".to_vec(),
        };
        assert_eq!(error_text(&output), "stderr content");
    }

    #[test]
    fn error_text_falls_back_to_stdout() {
        let output = Output {
            status: std::process::ExitStatus::default(),
            stdout: b"stdout content".to_vec(),
            stderr: b"".to_vec(),
        };
        assert_eq!(error_text(&output), "stdout content");
    }
}

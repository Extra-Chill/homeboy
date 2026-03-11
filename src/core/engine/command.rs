//! Command execution primitives with consistent error handling.

use std::process::{Command, Output};

use crate::error::{Error, Result};
use serde::Serialize;

pub fn run(program: &str, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new(program).args(args).output().map_err(|e| {
        Error::internal_io(
            format!("Failed to run {}: {}", context, e),
            Some(context.to_string()),
        )
    })?;

    if !output.status.success() {
        return Err(Error::internal_io(
            format!("{} failed: {}", context, error_text(&output)),
            Some(context.to_string()),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn run_in(dir: &str, program: &str, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to run {}: {}", context, e),
                Some(context.to_string()),
            )
        })?;

    if !output.status.success() {
        return Err(Error::internal_io(
            format!("{} failed: {}", context, error_text(&output)),
            Some(context.to_string()),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

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

pub fn error_text(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

pub fn succeeded_in(dir: &str, program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn require_success(success: bool, stderr: &str, operation: &str) -> Result<()> {
    if success {
        Ok(())
    } else {
        Err(Error::internal_io(
            format!("{}_FAILED: {}", operation, stderr),
            Some(operation.to_string()),
        ))
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CapturedOutput {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

impl CapturedOutput {
    pub fn new(stdout: String, stderr: String) -> Self {
        Self { stdout, stderr }
    }

    pub fn is_empty(&self) -> bool {
        self.stdout.is_empty() && self.stderr.is_empty()
    }
}

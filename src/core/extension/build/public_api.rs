//! public_api — extracted from mod.rs.

use crate::component::{self, Component};
use crate::config::{is_json_input, parse_bulk_ids};
use crate::error::{Error, Result};
use serde::Serialize;
use std::path::PathBuf;
use crate::engine::command::CapturedOutput;
use crate::extension::{self, exec_context, ExtensionCapability, ExtensionExecutionContext};
use crate::output::{BulkResult, BulkSummary, ItemOutcome};
use crate::core::extension::build::BuildResult;


/// Run build for one or more components.
///
/// Accepts either:
/// - A single component ID: "extrachill-api"
/// - A JSON spec: {"componentIds": ["api", "users"]}
pub fn run(input: &str) -> Result<(BuildResult, i32)> {
    if is_json_input(input) {
        run_bulk(input)
    } else {
        run_single(input)
    }
}

/// Build a component for deploy context.
/// Returns (exit_code, error_message) - None error means success.
///
/// Thin wrapper around `execute_build_component` that adapts the return type
/// for the deploy pipeline's error handling convention.
pub(crate) fn build_component(component: &component::Component) -> (Option<i32>, Option<String>) {
    match execute_build_component(component) {
        Ok((output, exit_code)) => {
            if output.success {
                (Some(exit_code), None)
            } else {
                (
                    Some(exit_code),
                    Some(format_build_error(
                        &component.id,
                        &output.build_command,
                        &component.local_path,
                        exit_code,
                        &output.output.stderr,
                        &output.output.stdout,
                    )),
                )
            }
        }
        Err(e) => (Some(1), Some(e.to_string())),
    }
}

/// Format a build error message with context from stderr/stdout.
/// Only includes universal POSIX exit code hints - Homeboy is technology-agnostic.
pub(crate) fn format_build_error(
    component_id: &str,
    build_cmd: &str,
    working_dir: &str,
    exit_code: i32,
    stderr: &str,
    stdout: &str,
) -> String {
    // Get useful output (prefer stderr, fall back to stdout)
    let output_text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };

    // Get last 15 lines for context
    let tail: Vec<&str> = output_text.lines().rev().take(15).collect();
    let output_tail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");

    // Translate universal POSIX exit codes only (no tool-specific hints)
    let hint = match exit_code {
        127 => "\nHint: Command not found. Check that the build command and its dependencies are installed and in PATH.",
        126 => "\nHint: Permission denied. Check file permissions on the build script.",
        _ => "",
    };

    let mut msg = format!(
        "Build failed for '{}' (exit code {}).\n  Command: {}\n  Working directory: {}",
        component_id, exit_code, build_cmd, working_dir
    );

    if !output_tail.is_empty() {
        msg.push_str("\n\n--- Build output (last 15 lines) ---\n");
        msg.push_str(&output_tail);
        msg.push_str("\n--- End of output ---");
    }

    if !hint.is_empty() {
        msg.push_str(hint);
    }

    msg
}

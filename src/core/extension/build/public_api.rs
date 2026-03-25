//! public_api — extracted from mod.rs.

use crate::core::extension::build::command;
use crate::core::extension::build::BuildResult;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_default_path() {
        let input = "";
        let _result = run(&input);
    }

    #[test]
    fn test_build_component_match_execute_build_component_component() {

        let _result = build_component();
    }

    #[test]
    fn test_build_component_output_success() {

        let result = build_component();
        assert!(result.is_some(), "expected Some for: output.success");
    }

    #[test]
    fn test_build_component_else() {

        let result = build_component();
        assert!(result.is_some(), "expected Some for: else");
    }

    #[test]
    fn test_build_component_err_e_some_1_some_e_to_string() {

        let result = build_component();
        assert!(result.is_some(), "expected Some for: Err(e) => (Some(1), Some(e.to_string())),");
    }

    #[test]
    fn test_format_build_error_default_path() {

        let _result = format_build_error();
    }

}

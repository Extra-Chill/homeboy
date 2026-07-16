//! gh CLI invocation helpers and shared GitHub status-check summarization.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use homeboy_core::engine::command::{
    isolate_process_tree, wait_with_bounded_output_until_cancelled, BoundedCommandOutput,
    DEFAULT_CAPTURE_LIMIT_BYTES,
};

pub(crate) fn ensure_gh_ready() -> std::result::Result<(), String> {
    let version = run_gh_command(&["--version".to_string()]);
    if version.is_err() {
        return Err("gh CLI not found on PATH or did not respond within 30s".to_string());
    }
    if let Err(error) = run_gh_command(&[
        "auth".to_string(),
        "status".to_string(),
        "--hostname".to_string(),
        "github.com".to_string(),
    ]) {
        return Err(format!("gh is not authenticated for github.com: {error}"));
    }
    Ok(())
}

pub(crate) fn run_gh(args: &[String]) -> std::result::Result<String, String> {
    run_gh_command(args)
}

fn run_gh_command(args: &[String]) -> std::result::Result<String, String> {
    let output = run_command_with_timeout(
        Command::new("gh").args(args.iter().map(|s| s.as_str())),
        Duration::from_secs(30),
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(if stderr.is_empty() { stdout } else { stderr });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> std::result::Result<BoundedCommandOutput, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate_process_tree(command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to invoke gh: {e}"))?;
    let started = Instant::now();
    let mut timed_out = false;
    let output =
        wait_with_bounded_output_until_cancelled(&mut child, DEFAULT_CAPTURE_LIMIT_BYTES, || {
            timed_out = started.elapsed() >= timeout;
            timed_out
        })
        .map_err(|e| format!("failed to wait for gh: {e}"))?;
    if timed_out {
        return Err(format!("gh timed out after {}s", timeout.as_secs()));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_timeout_kills_hung_process() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 1"]);
        let err = run_command_with_timeout(&mut command, Duration::from_millis(10)).unwrap_err();
        assert!(err.contains("timed out"));
    }

    #[test]
    fn command_timeout_captures_success_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '{\"ok\":true}'"]);
        let output = run_command_with_timeout(&mut command, Duration::from_secs(1)).unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "{\"ok\":true}");
    }
}

pub(crate) fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(crate) fn summarize_checks(checks: &[Value]) -> Option<String> {
    if checks.is_empty() {
        return None;
    }
    let mut saw_pending = false;
    for check in checks {
        let conclusion = check.get("conclusion").and_then(Value::as_str);
        let status = check.get("status").and_then(Value::as_str);
        if matches!(
            conclusion,
            Some("FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED")
        ) {
            return Some("FAILURE".to_string());
        }
        if conclusion.is_none() && !matches!(status, Some("COMPLETED")) {
            saw_pending = true;
        }
    }
    Some(if saw_pending { "PENDING" } else { "SUCCESS" }.to_string())
}

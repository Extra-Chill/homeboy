use std::path::Path;

use homeboy_audit_contract::ExtensionPhaseTiming;
use homeboy_core::engine::run_dir::{self, RunDir};
use homeboy_core::error::{Error, Result};
use homeboy_core::server::CommandOutput;
use serde_json::json;

use super::{FAILURE_TAIL_LINES, STALE_VALIDATION_DEPENDENCY_PREFIX};

pub(super) fn failure_payload(
    phase: &str,
    command: &str,
    output: &CommandOutput,
) -> serde_json::Value {
    let (stdout_tail, stdout_truncated) = tail_lines(&output.stdout, FAILURE_TAIL_LINES);
    let (stderr_tail, stderr_truncated) = tail_lines(&output.stderr, FAILURE_TAIL_LINES);
    let mut payload = json!({
        "phase": phase,
        "command": command,
        "exit_code": output.exit_code,
        "stdout_tail": stdout_tail,
        "stderr_tail": stderr_tail,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
    });

    if let Some(detail) = parsed_detail(&output.stdout).or_else(|| parsed_detail(&output.stderr)) {
        payload["parsed_detail"] = detail;
    }

    payload
}

fn parsed_detail(output: &str) -> Option<serde_json::Value> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .map(str::trim)
            .find_map(|line| serde_json::from_str(line).ok())
    })
}

pub(crate) fn tail_lines(s: &str, max_lines: usize) -> (String, bool) {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        (s.to_string(), false)
    } else {
        let start = lines.len() - max_lines;
        (lines[start..].join("\n"), true)
    }
}

pub(crate) fn read_extension_phase_timings(
    run_dir_path: &Path,
) -> Result<Vec<ExtensionPhaseTiming>> {
    let run_dir = RunDir::from_existing(run_dir_path.to_path_buf())?;
    let Some(value) = run_dir.read_step_output(run_dir::files::PHASE_TIMINGS) else {
        return Ok(Vec::new());
    };

    if let Some(timings) = value.get("phase_timings") {
        return serde_json::from_value(timings.clone()).map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some("parse extension phase timings".to_string()),
            )
        });
    }

    serde_json::from_value(value).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("parse extension phase timings".to_string()),
        )
    })
}

pub(super) fn stale_validation_dependency_message(stdout: &str, stderr: &str) -> Option<String> {
    stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .find(|line| {
            line.contains(STALE_VALIDATION_DEPENDENCY_PREFIX)
                && line.contains(" is behind ")
                && line.contains("commit(s)")
        })
        .map(str::to_string)
}

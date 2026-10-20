//! A bounded, human- and agent-readable excerpt of a failed deterministic
//! gate (#16218).
//!
//! Gate reports already carry the command's stdout/stderr and failure
//! evidence tails. Consumers that only echoed the command and exit code left
//! the gate-fix agent and the operator guessing. This builds one excerpt —
//! gate id, command, exit code, working directory, environment mode and the
//! relevant output lines — that the gate-fix prompt, `status` and `diagnose`
//! share.

use serde_json::Value;

use super::{text_tail, AgentTaskGateReport};

/// Maximum output lines included in an excerpt.
pub const GATE_FAILURE_EXCERPT_MAX_LINES: usize = 60;
/// Maximum bytes of output included in an excerpt.
pub const GATE_FAILURE_EXCERPT_MAX_BYTES: usize = 4 * 1024;

/// Lines that identify where a failure is reported in typical runner output.
const FAILURE_MARKERS: &[&str] = &["FAILED", "error", "Error", "ERROR", "panicked"];
/// Context lines kept before and after each marker line.
const CONTEXT_BEFORE: usize = 3;
const CONTEXT_AFTER: usize = 6;

/// The failure excerpt for a gate report, or `None` when the gate did not
/// fail or its policy withholds detailed evidence.
pub fn gate_failure_excerpt(report: &AgentTaskGateReport) -> Option<String> {
    serde_json::to_value(report)
        .ok()
        .and_then(|value| gate_failure_excerpt_from_value(&value))
}

/// The failure excerpt for a serialized gate report (as persisted in a
/// promotion report's `deterministic_gates`).
pub fn gate_failure_excerpt_from_value(gate: &Value) -> Option<String> {
    let status = str_at(gate, "/status").or_else(|| str_at(gate, "/state"))?;
    if !matches!(status, "failed" | "failure" | "accepted_inherited_failure") {
        return None;
    }
    let private = str_at(gate, "/visibility") == Some("private");
    let reveal = str_at(gate, "/reveal_policy").unwrap_or("full_evidence");
    if private && reveal != "full_evidence" {
        return None;
    }

    let id = str_at(gate, "/id").or_else(|| str_at(gate, "/name"));
    let command = str_at(gate, "/failure_evidence/command")
        .map(str::to_string)
        .or_else(|| match gate.get("command")? {
            Value::String(command) => Some(command.clone()),
            Value::Array(parts) => Some(
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .filter(|command| !command.is_empty());
    let exit_code = gate
        .get("exit_code")
        .and_then(Value::as_i64)
        .or_else(|| gate.pointer("/failure_evidence/exit_code")?.as_i64())
        .or_else(|| gate.pointer("/evidence/exit_code")?.as_i64());

    let mut lines = Vec::new();
    if let Some(id) = id {
        lines.push(format!("gate: {id}"));
    }
    if let Some(command) = command {
        lines.push(format!("command: {command}"));
    }
    if let Some(exit_code) = exit_code {
        lines.push(format!("exit code: {exit_code}"));
    }
    if let Some(effective) = str_at(gate, "/cwd/effective") {
        match str_at(gate, "/cwd/requested").filter(|requested| *requested != effective) {
            Some(requested) => lines.push(format!("cwd: {effective} (requested {requested})")),
            None => lines.push(format!("cwd: {effective}")),
        }
    }
    let mode = str_at(gate, "/environment/mode");
    let lab_runner = str_at(gate, "/environment/lab_runner");
    match (mode, lab_runner) {
        (Some(mode), Some(runner)) => {
            lines.push(format!("environment: {mode} (lab runner {runner})"))
        }
        (Some(mode), None) => lines.push(format!("environment: {mode}")),
        (None, Some(runner)) => lines.push(format!("environment: lab runner {runner}")),
        (None, None) => {}
    }

    let stdout = str_at(gate, "/stdout")
        .or_else(|| str_at(gate, "/evidence/stdout"))
        .filter(|text| !text.trim().is_empty())
        .or_else(|| str_at(gate, "/failure_evidence/stdout_tail"))
        .or_else(|| str_at(gate, "/evidence/failure_evidence/stdout_tail"))
        .unwrap_or_default();
    let stderr = str_at(gate, "/stderr")
        .or_else(|| str_at(gate, "/evidence/stderr"))
        .filter(|text| !text.trim().is_empty())
        .or_else(|| str_at(gate, "/failure_evidence/stderr_tail"))
        .or_else(|| str_at(gate, "/evidence/failure_evidence/stderr_tail"))
        .unwrap_or_default();
    let output = failure_output_excerpt(stdout, stderr);
    if output.is_empty() {
        lines.push("output: (none captured)".to_string());
    } else {
        lines.push("output:".to_string());
        lines.push(output);
    }
    Some(lines.join("\n"))
}

/// The relevant lines of a failed command's combined stderr/stdout: the lines
/// around failure markers when present, else the plain tail, bounded to
/// [`GATE_FAILURE_EXCERPT_MAX_LINES`] lines and
/// [`GATE_FAILURE_EXCERPT_MAX_BYTES`] bytes.
pub fn failure_output_excerpt(stdout: &str, stderr: &str) -> String {
    let combined = [stderr.trim_end(), stdout.trim_end()]
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if combined.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = combined.lines().collect();
    let markers: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| FAILURE_MARKERS.iter().any(|marker| line.contains(marker)))
        .map(|(index, _)| index)
        .collect();
    let excerpt = if markers.is_empty() {
        text_tail(&combined, GATE_FAILURE_EXCERPT_MAX_LINES)
    } else {
        let mut selected = vec![false; lines.len()];
        for index in &markers {
            let start = index.saturating_sub(CONTEXT_BEFORE);
            let end = (index + CONTEXT_AFTER + 1).min(lines.len());
            selected[start..end]
                .iter_mut()
                .for_each(|line| *line = true);
        }
        let mut out = Vec::new();
        let mut kept = 0;
        let mut previous: Option<usize> = None;
        for (index, line) in lines.iter().enumerate() {
            if !selected[index] {
                continue;
            }
            if kept == GATE_FAILURE_EXCERPT_MAX_LINES {
                out.push("...".to_string());
                break;
            }
            if previous.is_some_and(|previous| previous + 1 != index)
                || (previous.is_none() && index > 0)
            {
                out.push("...".to_string());
            }
            out.push((*line).to_string());
            kept += 1;
            previous = Some(index);
        }
        out.join("\n")
    };
    bound_bytes(&excerpt, GATE_FAILURE_EXCERPT_MAX_BYTES)
}

fn bound_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... (truncated)", &text[..end])
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn cargo_failure_gate() -> Value {
        json!({
            "id": "gate-1",
            "status": "failed",
            "command": ["sh", "-c", "cargo test -p demo"],
            "exit_code": 101,
            "cwd": {"requested": "/work", "effective": "/work/crates/demo"},
            "environment": {"mode": "replace", "lab_runner": "lab-1"},
            "stdout": "running 3 tests\ntest ok_one ... ok\ntest parser::rejects_empty ... FAILED\ntest ok_two ... ok\n\nfailures:\n\n---- parser::rejects_empty stdout ----\nthread 'parser::rejects_empty' panicked at src/parser.rs:42:5:\nassertion failed: result.is_err()\n\nfailures:\n    parser::rejects_empty\n\ntest result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
            "stderr": "   Compiling demo v0.1.0\n    Finished test profile\n     Running unittests src/lib.rs\nerror: test failed, to rerun pass `--lib`\n",
        })
    }

    #[test]
    fn gate_failure_excerpt_names_failing_test_and_exit_code() {
        let excerpt = gate_failure_excerpt_from_value(&cargo_failure_gate()).expect("excerpt");
        assert!(excerpt.contains("gate: gate-1"), "{excerpt}");
        assert!(excerpt.contains("exit code: 101"), "{excerpt}");
        assert!(excerpt.contains("command: sh -c cargo test -p demo"));
        assert!(excerpt.contains("cwd: /work/crates/demo (requested /work)"));
        assert!(excerpt.contains("environment: replace (lab runner lab-1)"));
        assert!(excerpt.contains("test parser::rejects_empty ... FAILED"));
        assert!(excerpt.contains("panicked at src/parser.rs:42:5"));
    }

    #[test]
    fn gate_failure_excerpt_from_report_matches_value() {
        let report: AgentTaskGateReport =
            serde_json::from_value(cargo_failure_gate()).expect("gate report");
        let excerpt = gate_failure_excerpt(&report).expect("excerpt");
        assert!(excerpt.contains("parser::rejects_empty"));
        assert!(excerpt.contains("exit code: 101"));
    }

    #[test]
    fn gate_failure_excerpt_is_bounded_and_falls_back_to_tail() {
        let stdout = (0..500)
            .map(|line| format!("line {line} {}", "x".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");
        let excerpt = failure_output_excerpt(&stdout, "");
        assert!(excerpt.len() <= GATE_FAILURE_EXCERPT_MAX_BYTES + 32);
        assert!(excerpt.contains("line 499"));
        assert!(!excerpt.contains("line 10 "));
    }

    #[test]
    fn gate_failure_excerpt_skips_passing_and_withheld_private_gates() {
        let mut gate = cargo_failure_gate();
        gate["status"] = json!("succeeded");
        assert!(gate_failure_excerpt_from_value(&gate).is_none());
        let mut gate = cargo_failure_gate();
        gate["visibility"] = json!("private");
        gate["reveal_policy"] = json!("summary_only");
        assert!(gate_failure_excerpt_from_value(&gate).is_none());
    }
}

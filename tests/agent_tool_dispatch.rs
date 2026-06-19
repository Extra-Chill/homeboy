use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// Maximum number of bytes retained per captured stream in this test harness.
/// The dispatched homeboy process' stdout/stderr are bounded with truncation
/// metadata so the test exercises the same bounded-capture contract the
/// production control plane uses rather than slurping unbounded output (#5363).
const DISPATCH_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;

/// Truncation metadata describing how much of a captured stream was retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamCaptureMetadata {
    limit_bytes: usize,
    seen_bytes: usize,
    retained_bytes: usize,
    truncated: bool,
}

/// Bound a captured stream to a retained-byte cap, keeping the trailing bytes
/// (the most relevant tail) and returning the retained bytes plus truncation
/// metadata. Mirrors the `bound_captured_stream` pattern used by the production
/// control plane and `agent_task_promotion`.
fn bound_captured_stream(bytes: &[u8], limit: usize) -> (Vec<u8>, StreamCaptureMetadata) {
    let seen = bytes.len();
    let retained: &[u8] = if seen > limit {
        &bytes[seen - limit..]
    } else {
        bytes
    };
    let metadata = StreamCaptureMetadata {
        limit_bytes: limit,
        seen_bytes: seen,
        retained_bytes: retained.len(),
        truncated: seen > retained.len(),
    };
    (retained.to_vec(), metadata)
}

#[test]
fn agent_tool_dispatch_outputs_raw_denied_result_without_wrapper() {
    let output = run_tool_dispatch(
        json!({
            "schema": "homeboy/agent-tool-policy/v1",
            "default_location": "disabled"
        }),
        request_json("create_github_issue"),
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(
        output.stderr.is_empty(),
        "stderr should be empty: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: Value = serde_json::from_slice(&output.stdout).expect("raw result json");
    assert_eq!(stdout["schema"], "homeboy/agent-tool-result/v1");
    assert_eq!(stdout["status"], "denied");
    assert_eq!(stdout["diagnostics"][0]["class"], "agent_tool.disabled");
    assert!(
        stdout.get("success").is_none(),
        "must not emit command wrapper"
    );
    assert!(
        stdout.get("data").is_none(),
        "must not emit command wrapper"
    );
}

#[test]
fn agent_tool_dispatch_handles_workspace_write_and_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = json!({
        "schema": "homeboy/agent-tool-policy/v1",
        "default_location": "control_plane"
    });

    let write_output = run_tool_dispatch(
        policy.clone(),
        json!({
            "schema": "homeboy/agent-tool-request/v1",
            "request_id": "request-write",
            "task_id": "task-1",
            "tool": "workspace_write",
            "input": {
                "workspace_path": dir.path(),
                "path": "notes/result.txt",
                "content": "hello dispatcher\n"
            }
        }),
    );
    assert_eq!(write_output.status.code(), Some(0));
    let write_stdout: Value = serde_json::from_slice(&write_output.stdout).expect("write json");
    assert_eq!(write_stdout["status"], "succeeded");

    let read_output = run_tool_dispatch(
        policy,
        json!({
            "schema": "homeboy/agent-tool-request/v1",
            "request_id": "request-read",
            "task_id": "task-1",
            "tool": "workspace_read",
            "input": {
                "workspace_path": dir.path(),
                "path": "notes/result.txt"
            }
        }),
    );
    assert_eq!(read_output.status.code(), Some(0));
    let read_stdout: Value = serde_json::from_slice(&read_output.stdout).expect("read json");
    assert_eq!(read_stdout["status"], "succeeded");
    assert_eq!(read_stdout["output"]["content"], "hello dispatcher\n");
}

#[test]
fn agent_tool_dispatch_outputs_raw_control_plane_validation_result() {
    let output = run_tool_dispatch(
        json!({
            "schema": "homeboy/agent-tool-policy/v1",
            "default_location": "control_plane"
        }),
        request_json("create_github_issue"),
    );

    assert_eq!(output.status.code(), Some(0));
    let stdout: Value = serde_json::from_slice(&output.stdout).expect("raw result json");
    assert_eq!(stdout["schema"], "homeboy/agent-tool-result/v1");
    assert_eq!(stdout["status"], "failed");
    assert_eq!(stdout["diagnostics"][0]["class"], "agent_tool.validation");
    assert!(
        stdout.get("success").is_none(),
        "must not emit command wrapper"
    );
}

fn run_tool_dispatch(policy: Value, request: Value) -> std::process::Output {
    let mut child = Command::new(homeboy_bin())
        .args(["agent-task", "tool", "dispatch"])
        .env("HOMEBOY_AGENT_TOOL_POLICY_JSON", policy.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn homeboy");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write request");
    let mut output = child.wait_with_output().expect("homeboy output");
    // Bound the retained stdout/stderr with truncation metadata so a runaway
    // dispatched process cannot force unbounded capture into the test harness
    // (#5363). The cap is far above any legitimate dispatch result size, so it
    // is transparent for normal runs.
    let (stdout, stdout_capture) =
        bound_captured_stream(&output.stdout, DISPATCH_CAPTURE_LIMIT_BYTES);
    let (stderr, stderr_capture) =
        bound_captured_stream(&output.stderr, DISPATCH_CAPTURE_LIMIT_BYTES);
    assert!(
        !stdout_capture.truncated,
        "dispatch stdout exceeded the {}-byte retained-capture cap (seen {} bytes); \
         a legitimate dispatch result should never be this large",
        stdout_capture.limit_bytes, stdout_capture.seen_bytes
    );
    assert!(
        !stderr_capture.truncated,
        "dispatch stderr exceeded the {}-byte retained-capture cap (seen {} bytes); \
         a legitimate dispatch result should never be this large",
        stderr_capture.limit_bytes, stderr_capture.seen_bytes
    );
    output.stdout = stdout;
    output.stderr = stderr;
    output
}

#[test]
fn bound_captured_stream_keeps_trailing_tail_when_truncated() {
    let blob = format!("{}TAIL", "x".repeat(100));
    let (retained, capture) = bound_captured_stream(blob.as_bytes(), 4);
    assert_eq!(retained, b"TAIL");
    assert_eq!(capture.limit_bytes, 4);
    assert_eq!(capture.seen_bytes, blob.len());
    assert_eq!(capture.retained_bytes, 4);
    assert!(capture.truncated);
}

#[test]
fn bound_captured_stream_retains_full_source_within_limit() {
    let (retained, capture) = bound_captured_stream(b"ok", DISPATCH_CAPTURE_LIMIT_BYTES);
    assert_eq!(retained, b"ok");
    assert_eq!(capture.seen_bytes, 2);
    assert_eq!(capture.retained_bytes, 2);
    assert!(!capture.truncated);
}

fn request_json(tool: &str) -> Value {
    json!({
        "schema": "homeboy/agent-tool-request/v1",
        "request_id": "request-1",
        "task_id": "task-1",
        "tool": tool,
        "input": {}
    })
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

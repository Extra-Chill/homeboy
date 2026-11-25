use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// Dispatch prints the raw `agent-tool-result/v1` document, never the command
/// envelope, whether the policy denies the tool or the control plane rejects
/// its input.
#[test]
fn agent_tool_dispatch_outputs_raw_results_without_command_wrapper() {
    for (location, status, class) in [
        ("disabled", "denied", "agent_tool.disabled"),
        ("control_plane", "failed", "agent_tool.validation"),
    ] {
        let output = run_tool_dispatch(
            json!({
                "schema": "homeboy/agent-tool-policy/v1",
                "default_location": location
            }),
            request_json("create_github_issue"),
        );

        assert_eq!(output.status.code(), Some(0), "{location}");
        assert!(
            output.stderr.is_empty(),
            "{location}: stderr should be empty: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout: Value = serde_json::from_slice(&output.stdout).expect("raw result json");
        assert_eq!(stdout["schema"], "homeboy/agent-tool-result/v1");
        assert_eq!(stdout["status"], status, "{location}");
        assert_eq!(stdout["diagnostics"][0]["class"], class, "{location}");
        assert!(
            stdout.get("success").is_none() && stdout.get("data").is_none(),
            "{location}: must not emit command wrapper: {stdout}"
        );
    }
}

/// Workspace tools round-trip content, and a command deny list constrains
/// commands rather than blocking unrelated tools.
#[test]
fn agent_tool_dispatch_handles_workspace_write_and_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = json!({
        "schema": "homeboy/agent-tool-policy/v1",
        "default_location": "control_plane",
        "commands": { "deny": [{ "pattern": "cargo *" }] }
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

/// A denied command must be refused at the `homeboy agent-task tool dispatch`
/// boundary itself — the one command-execution boundary Homeboy actually owns —
/// and the refusal must tell the agent why and what to do instead (#11481).
#[test]
fn agent_tool_dispatch_refuses_a_denied_command_with_a_reason() {
    let output = run_tool_dispatch(
        json!({
            "schema": "homeboy/agent-tool-policy/v1",
            "default_location": "control_plane",
            "commands": {
                "deny": [{ "pattern": "cargo test" }, { "pattern": "cargo build" }],
                "reason": "this host routes builds to CI; make your edits and push"
            }
        }),
        json!({
            "schema": "homeboy/agent-tool-request/v1",
            "request_id": "request-denied",
            "task_id": "task-1",
            "tool": "bash",
            "input": { "command": "timeout 1200 cargo test -q -j3 -p homeboy-agents" }
        }),
    );

    assert_eq!(output.status.code(), Some(0));
    let stdout: Value = serde_json::from_slice(&output.stdout).expect("raw result json");
    assert_eq!(stdout["status"], "denied");
    let diagnostic = &stdout["diagnostics"][0];
    assert_eq!(diagnostic["class"], "agent_tool.command_denied");
    assert_eq!(diagnostic["data"]["matched_pattern"], "cargo test");
    assert_eq!(
        diagnostic["data"]["reason"],
        "this host routes builds to CI; make your edits and push"
    );
    assert!(diagnostic["data"]["remediation"]
        .as_str()
        .expect("remediation")
        .contains("Make your edits"));
}

fn run_tool_dispatch(policy: Value, request: Value) -> std::process::Output {
    run_tool_dispatch_with_env(policy, request, &[])
}

fn run_tool_dispatch_with_env(
    policy: Value,
    request: Value,
    envs: &[(&str, String)],
) -> std::process::Output {
    let mut command = Command::new(homeboy_bin());
    command
        .args(["agent-task", "tool", "dispatch"])
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("HOMEBOY_AGENT_TOOL_POLICY_JSON", policy.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn homeboy");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write request");
    child.wait_with_output().expect("homeboy output")
}

#[test]
fn agent_tool_github_issue_create_uses_body_file_for_shell_sensitive_markdown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gh_path = dir.path().join("gh");
    let capture_prefix = dir.path().join("gh-capture");
    homeboy_test_support::write_executable(
        &gh_path,
        r#"#!/bin/sh
set -eu
out="$HOMEBOY_FAKE_GH_OUTPUT"
printf '%s
' "$@" > "$out.args"
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--body-file" ]; then
    shift
    cp "$1" "$out.body"
  fi
  shift || true
done
printf '%s
' 'https://github.com/Extra-Chill/homeboy/issues/1'
"#,
        0o755,
    )
    .expect("write fake gh");

    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", dir.path().display(), old_path);
    let body = "`homeboy-lab run`\n/home/chubes/.cache/homeboy/sample-runtime/source\n> quoted";
    let output = run_tool_dispatch_with_env(
        json!({
            "schema": "homeboy/agent-tool-policy/v1",
            "default_location": "control_plane"
        }),
        json!({
            "schema": "homeboy/agent-tool-request/v1",
            "request_id": "request-gh-issue",
            "task_id": "task-1",
            "tool": "create_github_issue",
            "input": {
                "repo": "Extra-Chill/homeboy",
                "title": "Safe issue body files",
                "body": body
            }
        }),
        &[
            ("PATH", path),
            (
                "HOMEBOY_FAKE_GH_OUTPUT",
                capture_prefix.to_string_lossy().into_owned(),
            ),
        ],
    );

    assert_eq!(output.status.code(), Some(0));
    let stdout: Value = serde_json::from_slice(&output.stdout).expect("dispatch json");
    assert_eq!(stdout["status"], "succeeded");
    let args = std::fs::read_to_string(capture_prefix.with_extension("args")).expect("gh args");
    assert!(args.contains("--body-file\n"));
    assert!(!args.contains(body));
    let captured_body =
        std::fs::read_to_string(capture_prefix.with_extension("body")).expect("gh body");
    assert_eq!(captured_body, body);
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

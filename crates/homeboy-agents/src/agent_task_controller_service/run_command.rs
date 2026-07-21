//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
//!
//! Controller `run_command` action execution: runs the configured command with
//! a timeout under an isolated process group, captures/persists its outputs and
//! artifacts, and validates required command artifacts.
#![allow(unused_imports)]
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use super::actions::{
    collect_capped_command_output, read_capped_command_output, CappedCommandOutput,
    RUN_COMMAND_DEFAULT_TIMEOUT_SECONDS,
};
use super::*;

pub(super) fn execute_run_command_action(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    request: &Value,
) -> Result<(Value, i32)> {
    let request = hydrate_consumed_artifacts(record, request);
    let request = request_with_required_workflow_artifacts(record, &request);
    let execution = request.get("execution").unwrap_or(&Value::Null);
    let command = required_string(execution, "command")?;
    let args = execution
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let required_artifacts = request
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|artifacts| {
            artifacts
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let timeout_seconds = execution
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
        .unwrap_or(RUN_COMMAND_DEFAULT_TIMEOUT_SECONDS);

    let io_dir = loop_action_io_dir(&record.loop_id, &action.action_id)?;
    fs::create_dir_all(&io_dir).map_err(|error| Error::internal_io(error.to_string(), None))?;
    let input_path = io_dir.join("input.json");
    let output_path = io_dir.join("output.json");
    let input = serde_json::json!({
        "schema": "homeboy/agent-task-loop-command-input/v1",
        "loop_id": record.loop_id,
        "action_id": action.action_id,
        "dedupe_key": dedupe_key,
        "entity_id": entity_id,
        "request": request,
        "controller": &*record,
    });
    write_json_file(&input_path, &input)?;

    let mut process = Command::new(&command);
    process.args(&args);
    if let Some(cwd) = execution
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
    {
        process.current_dir(cwd);
    }
    process.env("HOMEBOY_LOOP_ACTION_INPUT", &input_path);
    process.env("HOMEBOY_LOOP_ACTION_OUTPUT", &output_path);
    process.env("HOMEBOY_LOOP_ID", &record.loop_id);
    process.env("HOMEBOY_LOOP_ACTION_ID", &action.action_id);
    process.env("HOMEBOY_LOOP_ACTION_DEDUPE_KEY", dedupe_key);

    process.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = run_controller_command_with_timeout(process, &command, timeout_seconds)?;
    let exit_code = output.exit_code.unwrap_or(1);
    let result = if output_path.exists() {
        read_json_file(&output_path)?
    } else {
        serde_json::json!({})
    };
    let artifacts = result.get("artifacts").cloned().unwrap_or(Value::Null);
    let missing = missing_required_command_artifacts(&required_artifacts, &artifacts);
    let command_success = exit_code == 0 && missing.is_empty();
    let effective_exit_code = if command_success { 0 } else { 1 };

    if command_success {
        record_run_command_outputs(record, action, dedupe_key, entity_id, &request, &artifacts)?;
    }

    Ok((
        serde_json::json!({
            "mode": "run_command",
            "command": command,
            "args": args,
            "input_path": input_path,
            "output_path": output_path,
            "timeout_seconds": timeout_seconds,
            "timed_out": output.timed_out,
            "exit_code": exit_code,
            "signal": output.signal,
            "stdout": String::from_utf8_lossy(&output.stdout.bytes),
            "stderr": String::from_utf8_lossy(&output.stderr.bytes),
            "stdout_bytes": output.stdout.total_bytes,
            "stderr_bytes": output.stderr.total_bytes,
            "stdout_stored_bytes": output.stdout.bytes.len(),
            "stderr_stored_bytes": output.stderr.bytes.len(),
            "stdout_truncated": output.stdout.truncated,
            "stderr_truncated": output.stderr.truncated,
            "missing_artifacts": missing,
            "result": result,
        }),
        effective_exit_code,
    ))
}

struct ControllerCommandOutput {
    exit_code: Option<i32>,
    signal: Option<String>,
    timed_out: bool,
    stdout: CappedCommandOutput,
    stderr: CappedCommandOutput,
}

fn run_controller_command_with_timeout(
    mut process: Command,
    command: &str,
    timeout_seconds: u64,
) -> Result<ControllerCommandOutput> {
    configure_controller_command_process_group(&mut process);
    let mut child = process.spawn().map_err(|error| {
        Error::internal_io(
            format!("failed to execute controller command '{command}': {error}"),
            None,
        )
    })?;
    let stdout = child.stdout.take().map(read_capped_command_output);
    let stderr = child.stderr.take().map(read_capped_command_output);
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut timed_out = false;
    let exit_status = loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            Error::internal_io(
                format!("failed to poll controller command '{command}': {error}"),
                None,
            )
        })? {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            kill_controller_command_process_group(&mut child);
            break child.wait().map_err(|error| {
                Error::internal_io(
                    format!("failed to reap timed out controller command '{command}': {error}"),
                    None,
                )
            })?;
        }
        thread::sleep(Duration::from_millis(25));
    };
    Ok(ControllerCommandOutput {
        exit_code: exit_status.code(),
        signal: exit_status_signal(&exit_status),
        timed_out,
        stdout: collect_capped_command_output(stdout, command, "stdout")?,
        stderr: collect_capped_command_output(stderr, command, "stderr")?,
    })
}

#[cfg(unix)]
fn configure_controller_command_process_group(process: &mut Command) {
    use std::os::unix::process::CommandExt;

    process.process_group(0);
}

#[cfg(not(unix))]
fn configure_controller_command_process_group(_process: &mut Command) {}

#[cfg(unix)]
fn kill_controller_command_process_group(child: &mut std::process::Child) {
    let pid = child.id();
    if pid > i32::MAX as u32 {
        let _ = child.kill();
        return;
    }
    let pgid = -(pid as i32);
    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_controller_command_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

#[cfg(unix)]
fn exit_status_signal(status: &std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|signal| signal.to_string())
}

#[cfg(not(unix))]
fn exit_status_signal(_status: &std::process::ExitStatus) -> Option<String> {
    None
}

fn loop_action_io_dir(loop_id: &str, action_id: &str) -> Result<PathBuf> {
    Ok(homeboy_core::paths::homeboy_data()?
        .join("agent-task-loop-actions")
        .join(sanitize_loop_action_id(loop_id))
        .join(action_id))
}

pub(super) fn sanitize_loop_action_id(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn write_json_file(path: &PathBuf, value: &Value) -> Result<()> {
    let payload = serde_json::to_string_pretty(value)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    fs::write(path, format!("{payload}\n"))
        .map_err(|error| Error::internal_io(error.to_string(), None))
}

fn read_json_file(path: &PathBuf) -> Result<Value> {
    let payload =
        fs::read_to_string(path).map_err(|error| Error::internal_io(error.to_string(), None))?;
    serde_json::from_str(&payload).map_err(|error| Error::internal_json(error.to_string(), None))
}

fn missing_required_command_artifacts(required: &[String], artifacts: &Value) -> Vec<String> {
    required
        .iter()
        .filter(|artifact| artifacts.get(artifact.as_str()).is_none_or(Value::is_null))
        .cloned()
        .collect()
}

fn record_run_command_outputs(
    record: &mut AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
    dedupe_key: &str,
    entity_id: Option<&str>,
    request: &Value,
    artifacts: &Value,
) -> Result<()> {
    let run_id = format!("{}:{}", record.loop_id, action.action_id);
    let artifact_refs = artifact_refs_from_command_result(artifacts);
    persist_controller_artifacts(&record.loop_id, &action.action_id, artifacts)?;
    if let Some(entity_id) = entity_id {
        if let Some(entity) = record.entities.get_mut(entity_id) {
            entity.artifact_refs.extend(artifact_refs.clone());
        }
    }
    if !record
        .task_lineage
        .iter()
        .any(|lineage| lineage.run_id == run_id)
    {
        record.task_lineage.push(AgentTaskLoopTaskLineage {
            run_id: run_id.clone(),
            task_id: None,
            parent_run_id: None,
            parent_task_id: None,
            entity_id: entity_id.map(str::to_string),
            dedupe_key: Some(dedupe_key.to_string()),
            artifact_refs,
            inputs: request.clone(),
            outputs: serde_json::json!({ "artifacts": artifacts }),
        });
    }
    Ok(())
}

fn persist_controller_artifacts(loop_id: &str, action_id: &str, artifacts: &Value) -> Result<()> {
    let Some(object) = artifacts.as_object() else {
        return Ok(());
    };
    if object.is_empty() {
        return Ok(());
    }
    let root = homeboy_core::paths::artifact_root()?
        .join("agent-task-loop-controller")
        .join(sanitize_loop_action_id(loop_id))
        .join(action_id);
    fs::create_dir_all(&root).map_err(|error| Error::internal_io(error.to_string(), None))?;
    for (artifact_id, artifact) in object {
        let path = root.join(format!("{}.json", sanitize_loop_action_id(artifact_id)));
        write_json_file(&path, artifact)?;
    }
    Ok(())
}

fn artifact_refs_from_command_result(artifacts: &Value) -> Vec<AgentTaskLoopArtifactRef> {
    let Some(object) = artifacts.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .map(|(artifact_id, artifact)| {
            let uri = artifact
                .get("artifact_url")
                .or_else(|| artifact.get("url"))
                .or_else(|| artifact.get("path"))
                .and_then(Value::as_str)
                .unwrap_or(artifact_id)
                .to_string();
            AgentTaskLoopArtifactRef {
                uri,
                kind: artifact
                    .get("schema")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                role: None,
                label: Some(artifact_id.clone()),
                semantic_key: None,
            }
        })
        .collect()
}

//! Shared process execution for extension API capabilities: spawn retry,
//! bounded output capture, and process evidence.

use std::process::{Command, Stdio};

use homeboy_engine_primitives::command::{
    wait_with_bounded_output, BoundedCommandOutput, DEFAULT_CAPTURE_LIMIT_BYTES,
};
use homeboy_extension_contract::api::v1::ExtensionApiInvocationProcessEvidence;

pub(super) fn process_evidence(
    output: &BoundedCommandOutput,
) -> ExtensionApiInvocationProcessEvidence {
    ExtensionApiInvocationProcessEvidence {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        parsed_output: serde_json::from_slice(&output.stdout).ok(),
    }
}

/// A capability process that could not start, receive input, or exit
/// successfully, with whatever process evidence was captured.
pub(super) struct CapabilityExecutionFailure {
    pub process: Option<ExtensionApiInvocationProcessEvidence>,
}

/// Execute one capability process with common spawn retry and bounded capture.
pub(super) fn execute_capability_process(
    script_path: &std::path::Path,
    working_directory: &str,
    environment: &[(String, String)],
) -> Result<BoundedCommandOutput, CapabilityExecutionFailure> {
    let child = spawn_capability(script_path, working_directory, environment)
        .map_err(|_| CapabilityExecutionFailure { process: None })?;
    let output = wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES)
        .map_err(|_| CapabilityExecutionFailure { process: None })?;
    if !output.status.success() {
        return Err(CapabilityExecutionFailure {
            process: Some(process_evidence(&output)),
        });
    }
    Ok(output)
}

fn spawn_capability(
    script_path: &std::path::Path,
    working_directory: &str,
    environment: &[(String, String)],
) -> std::io::Result<std::process::Child> {
    let mut last_error = None;
    for attempt in 0..3 {
        let mut command = Command::new(script_path);
        command
            .current_dir(working_directory)
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if is_transient_spawn_error(&error) && attempt < 2 => {
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(25 * (attempt + 1)));
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.expect("transient spawn error captured before retry"))
}

fn is_transient_spawn_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(11) | Some(26))
}

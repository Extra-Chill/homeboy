//! Controller materialize spec-source resolution.
//!
//! Resolves the `materialize` command's `--spec` input into a concrete repo
//! loop spec. The input may be a literal spec or a generator manifest that
//! declares a command to run; in the latter case this module executes the
//! generator (process execution), reads the generated spec from disk, validates
//! it, and assembles generator evidence. This orchestration belongs in core so
//! the CLI adapter stays thin.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::agent_task_loop_definition::{
    materialize_repo_loop_spec, AgentTaskLoopSpecMaterializationRequest,
};
use crate::core::config;
use crate::core::proof::validate_proof_value;
use crate::core::{Error, Result};

use super::AgentTaskRepoLoopSpec;

const CONTROLLER_SPEC_GENERATOR_SCHEMA: &str = "homeboy/agent-task-loop-spec-generator/v1";
const DEFAULT_GENERATOR_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_GENERATOR_MAX_STDOUT_BYTES: usize = 64 * 1024;
const DEFAULT_GENERATOR_MAX_STDERR_BYTES: usize = 64 * 1024;

/// Resolved controller materialize spec plus optional generator evidence.
pub struct MaterializeSpecSource {
    pub spec: AgentTaskRepoLoopSpec,
    pub generator_evidence: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ControllerSpecGeneratorManifest {
    schema: String,
    command: Vec<String>,
    output_path: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    max_stdout_bytes: Option<usize>,
    #[serde(default)]
    max_stderr_bytes: Option<usize>,
    #[serde(default)]
    inputs: Value,
}

struct GeneratorExecutionBounds {
    timeout: Duration,
    timeout_seconds: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

struct BoundedOutput {
    text: String,
    truncated: bool,
    bytes_read: usize,
}

struct GeneratorCommandOutput {
    status: Option<ExitStatus>,
    timed_out: bool,
    stdout: BoundedOutput,
    stderr: BoundedOutput,
    cwd: String,
}

/// Load the materialize spec source, executing a generator manifest if the
/// input declares one rather than an inline spec.
pub fn load_materialize_spec_source(source: &str) -> Result<MaterializeSpecSource> {
    let raw = config::read_json_spec_to_string(source)?;
    match serde_json::from_str::<AgentTaskRepoLoopSpec>(&raw) {
        Ok(spec) => Ok(MaterializeSpecSource {
            spec,
            generator_evidence: None,
        }),
        Err(spec_error) => {
            let manifest: ControllerSpecGeneratorManifest =
                serde_json::from_str(&raw).map_err(|_| {
                    Error::validation_invalid_argument(
                        "spec",
                        spec_error.to_string(),
                        Some(source.to_string()),
                        None,
                    )
                })?;
            load_generated_materialize_spec(source, manifest)
        }
    }
}

fn load_generated_materialize_spec(
    source: &str,
    manifest: ControllerSpecGeneratorManifest,
) -> Result<MaterializeSpecSource> {
    validate_generator_manifest(source, &manifest)?;
    let manifest_dir = manifest_base_dir(source);
    let output_path = resolve_manifest_path(manifest_dir.as_deref(), &manifest.output_path);
    let command_status = run_spec_generator_command(&manifest, manifest_dir.as_deref())?;
    let generated_raw = std::fs::read_to_string(&output_path).map_err(|error| {
        Error::validation_invalid_argument(
            "spec.output_path",
            format!(
                "generator command completed but generated spec was not found at {}; rerun the manifest command or update output_path: {error}",
                output_path.display()
            ),
            Some(source.to_string()),
            Some(vec![format!(
                "{} must write {}",
                manifest.command.join(" "),
                manifest.output_path
            )]),
        )
    })?;
    let spec: AgentTaskRepoLoopSpec = serde_json::from_str(&generated_raw).map_err(|error| {
        Error::validation_invalid_argument(
            "spec.output_path",
            format!("generated spec JSON is invalid: {error}"),
            Some(output_path.display().to_string()),
            None,
        )
    })?;
    let materialized = materialize_repo_loop_spec(AgentTaskLoopSpecMaterializationRequest {
        spec: &spec,
        run_inputs: &Value::Null,
        policy_results: &[],
    })?;
    let validation_result =
        validate_proof_value(serde_json::to_value(materialized).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("controller.materialize.generator.serialize".to_string()),
            )
        })?);
    let spec_hash = format!("{:x}", Sha256::digest(generated_raw.as_bytes()));

    Ok(MaterializeSpecSource {
        spec,
        generator_evidence: Some(serde_json::json!({
            "schema": "homeboy/agent-task-loop-spec-generator-evidence/v1",
            "manifest": source,
            "command": manifest.command,
            "inputs": manifest.inputs,
            "output_path": output_path.display().to_string(),
            "spec_hash": spec_hash,
            "validation_result": validation_result,
            "status": command_status,
        })),
    })
}

fn validate_generator_manifest(
    source: &str,
    manifest: &ControllerSpecGeneratorManifest,
) -> Result<()> {
    if manifest.schema != CONTROLLER_SPEC_GENERATOR_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "spec.schema",
            format!(
                "controller materialize generator manifests must use schema {CONTROLLER_SPEC_GENERATOR_SCHEMA}"
            ),
            Some(source.to_string()),
            None,
        ));
    }
    if manifest.command.is_empty() || manifest.command.iter().any(|part| part.trim().is_empty()) {
        return Err(Error::validation_invalid_argument(
            "spec.command",
            "generator command must be a non-empty array of program and arguments",
            Some(source.to_string()),
            None,
        ));
    }
    if manifest.output_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "spec.output_path",
            "generator manifest must declare the spec file path written by the command",
            Some(source.to_string()),
            None,
        ));
    }
    if !manifest.inputs.is_null() && !manifest.inputs.is_object() {
        return Err(Error::validation_invalid_argument(
            "spec.inputs",
            "generator manifest inputs must be a JSON object when present",
            Some(source.to_string()),
            None,
        ));
    }
    if manifest.timeout_seconds == Some(0) {
        return Err(Error::validation_invalid_argument(
            "spec.timeout_seconds",
            "generator timeout_seconds must be greater than zero when present",
            Some(source.to_string()),
            None,
        ));
    }
    Ok(())
}

fn manifest_base_dir(source: &str) -> Option<PathBuf> {
    let file = source.strip_prefix('@')?;
    if file == "-" {
        return None;
    }
    Path::new(file).parent().map(Path::to_path_buf)
}

fn resolve_manifest_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path)
    } else {
        path
    }
}

fn run_spec_generator_command(
    manifest: &ControllerSpecGeneratorManifest,
    manifest_dir: Option<&Path>,
) -> Result<Value> {
    let bounds = generator_execution_bounds(manifest);
    let cwd = generator_cwd_display(manifest_dir);
    let mut command = Command::new(&manifest.command[0]);
    command.args(&manifest.command[1..]);
    if let Some(manifest_dir) = manifest_dir {
        command.current_dir(manifest_dir);
    }
    let output = run_bounded_command(command, &bounds, cwd.clone()).map_err(|error| {
        Error::validation_invalid_argument(
            "spec.command",
            format!("failed to execute generator command in {cwd}: {error}"),
            Some(manifest.command.join(" ")),
            None,
        )
    })?;
    let code = output.status.and_then(|status| status.code());
    if output.timed_out {
        let mut error = Error::validation_invalid_argument(
            "spec.command",
            format!(
                "generator command timed out after {} seconds",
                bounds.timeout_seconds
            ),
            Some(manifest.command.join(" ")),
            None,
        );
        attach_generator_failure_diagnostics(
            &mut error,
            "generator.timeout",
            &output,
            &bounds,
            "generator command exceeded timeout_seconds",
        );
        return Err(error);
    }
    if !output
        .status
        .map(|status| status.success())
        .unwrap_or(false)
    {
        let mut error = Error::validation_invalid_argument(
            "spec.command",
            format!(
                "generator command exited with status {}; stderr: {}",
                code.map(|value| value.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string()),
                output.stderr.text.trim()
            ),
            Some(manifest.command.join(" ")),
            None,
        );
        attach_generator_failure_diagnostics(
            &mut error,
            "generator.exit_status",
            &output,
            &bounds,
            "generator command exited unsuccessfully",
        );
        return Err(error);
    }
    Ok(serde_json::json!({
        "exit_code": code,
        "stdout": output.stdout.text.trim(),
        "stderr": output.stderr.text.trim(),
        "stdout_truncated": output.stdout.truncated,
        "stderr_truncated": output.stderr.truncated,
        "stdout_bytes_read": output.stdout.bytes_read,
        "stderr_bytes_read": output.stderr.bytes_read,
        "max_stdout_bytes": bounds.max_stdout_bytes,
        "max_stderr_bytes": bounds.max_stderr_bytes,
        "timeout_seconds": bounds.timeout_seconds,
        "timed_out": false,
        "cwd": output.cwd,
    }))
}

fn generator_execution_bounds(
    manifest: &ControllerSpecGeneratorManifest,
) -> GeneratorExecutionBounds {
    let timeout_seconds = manifest
        .timeout_seconds
        .unwrap_or(DEFAULT_GENERATOR_TIMEOUT_SECONDS);
    GeneratorExecutionBounds {
        timeout: Duration::from_secs(timeout_seconds),
        timeout_seconds,
        max_stdout_bytes: manifest
            .max_stdout_bytes
            .unwrap_or(DEFAULT_GENERATOR_MAX_STDOUT_BYTES),
        max_stderr_bytes: manifest
            .max_stderr_bytes
            .unwrap_or(DEFAULT_GENERATOR_MAX_STDERR_BYTES),
    }
}

fn generator_cwd_display(manifest_dir: Option<&Path>) -> String {
    manifest_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn run_bounded_command(
    mut command: Command,
    bounds: &GeneratorExecutionBounds,
    cwd: String,
) -> std::io::Result<GeneratorCommandOutput> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let stdout_handle = thread::spawn({
        let max_bytes = bounds.max_stdout_bytes;
        move || read_bounded_output(stdout, max_bytes)
    });
    let stderr_handle = thread::spawn({
        let max_bytes = bounds.max_stderr_bytes;
        move || read_bounded_output(stderr, max_bytes)
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= bounds.timeout {
            timed_out = true;
            let _ = child.kill();
            break Some(child.wait()?);
        }
        thread::sleep(Duration::from_millis(25));
    };

    Ok(GeneratorCommandOutput {
        status,
        timed_out,
        stdout: join_bounded_output(stdout_handle),
        stderr: join_bounded_output(stderr_handle),
        cwd,
    })
}

fn read_bounded_output(mut reader: impl Read, max_bytes: usize) -> BoundedOutput {
    let mut stored = Vec::new();
    let mut bytes_read = 0usize;
    let mut buffer = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        bytes_read = bytes_read.saturating_add(read);
        let remaining = max_bytes.saturating_sub(stored.len());
        if remaining > 0 {
            stored.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    BoundedOutput {
        text: String::from_utf8_lossy(&stored).to_string(),
        truncated: bytes_read > stored.len(),
        bytes_read,
    }
}

fn join_bounded_output(handle: thread::JoinHandle<BoundedOutput>) -> BoundedOutput {
    handle.join().unwrap_or_else(|_| BoundedOutput {
        text: String::new(),
        truncated: false,
        bytes_read: 0,
    })
}

fn attach_generator_failure_diagnostics(
    error: &mut Error,
    class: &str,
    output: &GeneratorCommandOutput,
    bounds: &GeneratorExecutionBounds,
    message: &str,
) {
    error.details["diagnostics"] = serde_json::json!([{
        "class": class,
        "message": message,
        "data": {
            "cwd": output.cwd,
            "exit_code": output.status.and_then(|status| status.code()),
            "timed_out": output.timed_out,
            "timeout_seconds": bounds.timeout_seconds,
            "stdout": output.stdout.text.trim(),
            "stderr": output.stderr.text.trim(),
            "stdout_truncated": output.stdout.truncated,
            "stderr_truncated": output.stderr.truncated,
            "stdout_bytes_read": output.stdout.bytes_read,
            "stderr_bytes_read": output.stderr.bytes_read,
            "max_stdout_bytes": bounds.max_stdout_bytes,
            "max_stderr_bytes": bounds.max_stderr_bytes,
        }
    }]);
}

use std::io::Write;
use std::process::{Command, Stdio};

use homeboy_engine_primitives::command::{
    wait_with_bounded_output, BoundedCommandOutput, DEFAULT_CAPTURE_LIMIT_BYTES,
};
use homeboy_extension_contract::api::v1::{
    ExtensionApiInvocationProcessEvidence, ExtensionApiInvokeRequest, ExtensionApiInvokeResponse,
    ExtensionApiOperationFailure, ExtensionApiOperationFailureCode, ExtensionApiResolveRequest,
    COMPILER_WARNINGS_CAPABILITY_ID, COMPILER_WARNING_FIXES_CAPABILITY_ID,
    EXTENSION_API_INVOKE_REQUEST_SCHEMA, EXTENSION_API_INVOKE_RESPONSE_SCHEMA,
    EXTENSION_API_RESOLVE_REQUEST_SCHEMA, EXTENSION_API_V1, FINGERPRINT_FILE_CAPABILITY_PREFIX,
    REFACTOR_FILE_CAPABILITY_PREFIX,
};
use homeboy_extension_contract::ExtensionManifest;

use crate::extension::catalog::{load_extension, resolve_api, validate_operation_request};

pub fn invoke_api(request: &ExtensionApiInvokeRequest) -> ExtensionApiInvokeResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_INVOKE_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return failure_response(failure);
    }

    let resolved = resolve_api(&ExtensionApiResolveRequest {
        schema: EXTENSION_API_RESOLVE_REQUEST_SCHEMA.to_string(),
        api_version: request.api_version,
        extension_id: request.extension_id.clone(),
        capability_id: request.capability_id.clone(),
    });
    if let Some(resolve_failure) = resolved.failure {
        return failure_response(resolve_failure);
    }

    let extension = match load_extension(&request.extension_id) {
        Ok(extension) => extension,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::ExtensionInvalid,
                error.to_string(),
            );
        }
    };
    let Some(script) = capability_script(&extension, &request.capability_id) else {
        return failure(
            ExtensionApiOperationFailureCode::CapabilityNotProvided,
            format!(
                "Extension '{}' does not provide an invokable implementation for capability '{}'",
                request.extension_id, request.capability_id
            ),
        );
    };
    let Some(extension_path) = extension.extension_path.as_deref() else {
        return failure(
            ExtensionApiOperationFailureCode::ExtensionInvalid,
            format!(
                "Extension '{}' has no installation path",
                request.extension_id
            ),
        );
    };
    let script_path = std::path::Path::new(extension_path).join(script);

    let input = request.input.to_string();
    let output = match execute_capability_process(
        &script_path,
        &request.working_directory,
        Some(input.as_bytes()),
        &[],
        &request.extension_id,
        &request.capability_id,
    ) {
        Ok(output) => output,
        Err(execution_failure) => {
            return match execution_failure.process {
                Some(process) => failure_with_process(
                    ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
                    execution_failure.message,
                    process,
                ),
                None => failure(
                    ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
                    execution_failure.message,
                ),
            };
        }
    };
    let evidence = process_evidence(&output);
    let value = match evidence.parsed_output.clone() {
        Some(value) => value,
        None => {
            let error = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .expect_err("process evidence only omits parsed output for invalid JSON");
            return failure_with_process(
                ExtensionApiOperationFailureCode::CapabilityOutputInvalid,
                format!(
                    "Capability '{}' returned invalid JSON for extension '{}': {error}",
                    request.capability_id, request.extension_id,
                ),
                evidence,
            );
        }
    };

    ExtensionApiInvokeResponse {
        schema: EXTENSION_API_INVOKE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        output: Some(value),
        failure: None,
        process: None,
    }
}

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

pub(super) struct CapabilityExecutionFailure {
    pub message: String,
    pub process: Option<ExtensionApiInvocationProcessEvidence>,
}

/// Execute one capability process with common spawn retry and bounded capture.
pub(super) fn execute_capability_process(
    script_path: &std::path::Path,
    working_directory: &str,
    input: Option<&[u8]>,
    environment: &[(String, String)],
    extension_id: &str,
    capability_id: &str,
) -> Result<BoundedCommandOutput, CapabilityExecutionFailure> {
    let mut child = spawn_capability(script_path, working_directory, environment, input.is_some())
        .map_err(|error| CapabilityExecutionFailure {
            message: format!(
                "Failed to start capability '{capability_id}' for extension '{extension_id}': {error}"
            ),
            process: None,
        })?;
    if let Some(input) = input {
        if let Err(error) = child
            .stdin
            .take()
            .ok_or_else(|| "capability stdin was unavailable".to_string())
            .and_then(|mut stdin| stdin.write_all(input).map_err(|error| error.to_string()))
        {
            let _ = child.kill();
            let message = format!(
                "Failed to send input to capability '{capability_id}' for extension '{extension_id}': {error}"
            );
            return match wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES) {
                Ok(output) => Err(CapabilityExecutionFailure {
                    message,
                    process: Some(process_evidence(&output)),
                }),
                Err(_) => Err(CapabilityExecutionFailure {
                    message,
                    process: None,
                }),
            };
        }
    }
    let output = wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES).map_err(|error| {
        CapabilityExecutionFailure {
            message: format!(
                "Failed to collect capability '{capability_id}' output for extension '{extension_id}': {error}"
            ),
            process: None,
        }
    })?;
    if !output.status.success() {
        let process = process_evidence(&output);
        return Err(CapabilityExecutionFailure {
            message: format!(
                "Capability '{capability_id}' failed for extension '{extension_id}': {}",
                process.stderr.trim()
            ),
            process: Some(process),
        });
    }
    Ok(output)
}

fn capability_script<'a>(extension: &'a ExtensionManifest, capability_id: &str) -> Option<&'a str> {
    match capability_id {
        COMPILER_WARNINGS_CAPABILITY_ID => extension.compiler_warnings_script(),
        COMPILER_WARNING_FIXES_CAPABILITY_ID => extension.compiler_warning_fixes_script(),
        capability if capability.starts_with(FINGERPRINT_FILE_CAPABILITY_PREFIX) => {
            extension.fingerprint_script()
        }
        capability if capability.starts_with(REFACTOR_FILE_CAPABILITY_PREFIX) => {
            extension.refactor_script()
        }
        _ => None,
    }
}

fn failure(code: ExtensionApiOperationFailureCode, message: String) -> ExtensionApiInvokeResponse {
    failure_response(ExtensionApiOperationFailure { code, message })
}

fn failure_response(failure: ExtensionApiOperationFailure) -> ExtensionApiInvokeResponse {
    ExtensionApiInvokeResponse {
        schema: EXTENSION_API_INVOKE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        output: None,
        failure: Some(failure),
        process: None,
    }
}

fn failure_with_process(
    code: ExtensionApiOperationFailureCode,
    message: String,
    process: ExtensionApiInvocationProcessEvidence,
) -> ExtensionApiInvokeResponse {
    let mut response = failure(code, message);
    response.process = Some(process);
    response
}

fn spawn_capability(
    script_path: &std::path::Path,
    working_directory: &str,
    environment: &[(String, String)],
    pipe_stdin: bool,
) -> std::io::Result<std::process::Child> {
    let mut last_error = None;
    for attempt in 0..3 {
        let mut command = Command::new(script_path);
        command
            .current_dir(working_directory)
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if pipe_stdin {
            command.stdin(Stdio::piped());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_executable(path: &std::path::Path, content: &str) {
        fs::write(path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    #[test]
    fn invoke_api_rejects_non_json_output() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/example");
            fs::create_dir_all(extension_dir.join("scripts")).unwrap();
            fs::write(
                extension_dir.join("example.json"),
                r#"{
                    "name": "Example",
                    "version": "1.0.0",
                    "scripts": { "compiler_warnings": "scripts/warnings.sh" }
                }"#,
            )
            .unwrap();
            write_executable(
                &extension_dir.join("scripts/warnings.sh"),
                "#!/usr/bin/env bash\ncat >/dev/null\nprintf 'not json'\n",
            );
            let root = tempfile::TempDir::new().unwrap();

            let response = invoke_api(&ExtensionApiInvokeRequest {
                schema: EXTENSION_API_INVOKE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: "example".to_string(),
                capability_id: COMPILER_WARNINGS_CAPABILITY_ID.to_string(),
                working_directory: root.path().to_string_lossy().into_owned(),
                input: serde_json::json!({ "root": root.path() }),
            });

            assert_eq!(
                response.failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::CapabilityOutputInvalid)
            );
            assert!(response.output.is_none());
            let process = response.process.expect("process evidence");
            assert_eq!(process.stdout, "not json");
            assert!(process.parsed_output.is_none());
        });
    }

    #[test]
    fn invoke_api_preserves_nonzero_process_evidence() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/example");
            fs::create_dir_all(extension_dir.join("scripts")).unwrap();
            fs::write(extension_dir.join("example.json"), r#"{"name":"Example","version":"1.0.0","scripts":{"refactor":"scripts/refactor.sh"},"provides":{"file_extensions":["rs"]}}"#).unwrap();
            write_executable(&extension_dir.join("scripts/refactor.sh"), "#!/bin/sh\ncat >/dev/null\nprintf '{\"detail\":\"failed\"}'\nprintf 'script failed' >&2\nexit 7\n");
            let root = tempfile::TempDir::new().unwrap();
            let response = invoke_api(&ExtensionApiInvokeRequest {
                schema: EXTENSION_API_INVOKE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: "example".to_string(),
                capability_id: "refactor.rs".to_string(),
                working_directory: root.path().to_string_lossy().into_owned(),
                input: serde_json::json!({}),
            });
            assert_eq!(
                response.failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::CapabilityExecutionFailed)
            );
            assert_eq!(
                response.process.expect("process evidence"),
                ExtensionApiInvocationProcessEvidence {
                    exit_code: Some(7),
                    stdout: "{\"detail\":\"failed\"}".to_string(),
                    stderr: "script failed".to_string(),
                    parsed_output: Some(serde_json::json!({"detail":"failed"}))
                }
            );
        });
    }

    #[test]
    fn invoke_api_preserves_process_evidence_when_stdin_closes() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/example");
            fs::create_dir_all(extension_dir.join("scripts")).unwrap();
            fs::write(extension_dir.join("example.json"), r#"{"name":"Example","version":"1.0.0","scripts":{"refactor":"scripts/refactor.sh"},"provides":{"file_extensions":["rs"]}}"#).unwrap();
            write_executable(
                &extension_dir.join("scripts/refactor.sh"),
                "#!/bin/sh\nexec 0<&-\nsleep 5\n",
            );
            let root = tempfile::TempDir::new().unwrap();
            let response = invoke_api(&ExtensionApiInvokeRequest {
                schema: EXTENSION_API_INVOKE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: "example".to_string(),
                capability_id: "refactor.rs".to_string(),
                working_directory: root.path().to_string_lossy().into_owned(),
                input: serde_json::json!({"content": "x".repeat(1024 * 1024)}),
            });

            assert_eq!(
                response.failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::CapabilityExecutionFailed)
            );
            assert!(response.process.is_some());
        });
    }

    #[test]
    fn transient_spawn_errors_are_classified_for_retry() {
        assert!(is_transient_spawn_error(&std::io::Error::from(
            std::io::ErrorKind::WouldBlock
        )));
        assert!(!is_transient_spawn_error(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
    }
}

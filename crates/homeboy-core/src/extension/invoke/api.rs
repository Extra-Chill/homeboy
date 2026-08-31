use std::io::Write;
use std::process::{Command, Stdio};

use homeboy_engine_primitives::command::{wait_with_bounded_output, DEFAULT_CAPTURE_LIMIT_BYTES};
use homeboy_extension_contract::api::v1::{
    ExtensionApiInvokeRequest, ExtensionApiInvokeResponse, ExtensionApiOperationFailure,
    ExtensionApiOperationFailureCode, ExtensionApiResolveRequest, COMPILER_WARNINGS_CAPABILITY_ID,
    COMPILER_WARNING_FIXES_CAPABILITY_ID, EXTENSION_API_INVOKE_REQUEST_SCHEMA,
    EXTENSION_API_INVOKE_RESPONSE_SCHEMA, EXTENSION_API_RESOLVE_REQUEST_SCHEMA, EXTENSION_API_V1,
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

    let mut child = match Command::new(&script_path)
        .current_dir(&request.working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
                format!(
                    "Failed to start capability '{}' for extension '{}': {error}",
                    request.capability_id, request.extension_id
                ),
            );
        }
    };
    let input = request.input.to_string();
    if let Err(error) = child
        .stdin
        .take()
        .ok_or_else(|| "capability stdin was unavailable".to_string())
        .and_then(|mut stdin| {
            stdin
                .write_all(input.as_bytes())
                .map_err(|error| error.to_string())
        })
    {
        let _ = child.kill();
        let _ = child.wait();
        return failure(
            ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
            format!(
                "Failed to send input to capability '{}' for extension '{}': {error}",
                request.capability_id, request.extension_id
            ),
        );
    }
    let output = match wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES) {
        Ok(output) => output,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
                format!(
                    "Failed to collect capability '{}' output for extension '{}': {error}",
                    request.capability_id, request.extension_id
                ),
            );
        }
    };
    if !output.status.success() {
        return failure(
            ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
            format!(
                "Capability '{}' failed for extension '{}': {}",
                request.capability_id,
                request.extension_id,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        );
    }
    let value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::CapabilityOutputInvalid,
                format!(
                    "Capability '{}' returned invalid JSON for extension '{}': {error}",
                    request.capability_id, request.extension_id
                ),
            );
        }
    };

    ExtensionApiInvokeResponse {
        schema: EXTENSION_API_INVOKE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        output: Some(value),
        failure: None,
    }
}

fn capability_script<'a>(extension: &'a ExtensionManifest, capability_id: &str) -> Option<&'a str> {
    match capability_id {
        COMPILER_WARNINGS_CAPABILITY_ID => extension.compiler_warnings_script(),
        COMPILER_WARNING_FIXES_CAPABILITY_ID => extension.compiler_warning_fixes_script(),
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
    }
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
        });
    }
}

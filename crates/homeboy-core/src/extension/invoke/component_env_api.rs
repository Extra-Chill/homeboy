use std::path::Path;

use homeboy_extension_contract::api::v1::{
    ExtensionApiComponentEnvDetectRequest, ExtensionApiComponentEnvDetectResponse,
    ExtensionApiOperationFailure, ExtensionApiOperationFailureCode, ExtensionApiResolveRequest,
    ExtensionApiRuntimeRequirement, COMPONENT_ENV_CAPABILITY_ID,
    EXTENSION_API_COMPONENT_ENV_DETECT_REQUEST_SCHEMA,
    EXTENSION_API_COMPONENT_ENV_DETECT_RESPONSE_SCHEMA, EXTENSION_API_RESOLVE_REQUEST_SCHEMA,
    EXTENSION_API_V1,
};
use homeboy_extension_contract::manifest_capability_config::RuntimeRequirementsConfig;

use crate::extension::catalog::{load_extension, resolve_api, validate_operation_request};

use super::api::{execute_capability_process, process_evidence};

/// Runtime-only inputs that must not enter the serialized Extension API request.
pub struct ComponentEnvDetectionContext<'a> {
    component_path: &'a Path,
}

impl<'a> ComponentEnvDetectionContext<'a> {
    pub fn new(component_path: &'a Path) -> Self {
        Self { component_path }
    }
}

pub fn detect_component_env_api(
    request: &ExtensionApiComponentEnvDetectRequest,
    context: ComponentEnvDetectionContext<'_>,
) -> ExtensionApiComponentEnvDetectResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_COMPONENT_ENV_DETECT_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return failure_response(failure, Vec::new(), None);
    }

    let resolved = resolve_api(&ExtensionApiResolveRequest {
        schema: EXTENSION_API_RESOLVE_REQUEST_SCHEMA.to_string(),
        api_version: request.api_version,
        extension_id: request.extension_id.clone(),
        capability_id: COMPONENT_ENV_CAPABILITY_ID.to_string(),
    });
    let extension_runtimes = resolved
        .descriptor
        .as_ref()
        .map(|descriptor| descriptor.execution_requirements.runtimes.clone())
        .unwrap_or_default();
    if let Some(failure) = resolved.failure {
        return failure_response(failure, extension_runtimes, None);
    }

    let extension = match load_extension(&request.extension_id) {
        Ok(extension) => extension,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::ExtensionInvalid,
                error.to_string(),
                extension_runtimes,
                None,
            );
        }
    };
    let Some(config) = extension.component_env.as_ref() else {
        return failure(
            ExtensionApiOperationFailureCode::CapabilityNotProvided,
            format!(
                "Extension '{}' does not provide capability '{COMPONENT_ENV_CAPABILITY_ID}'",
                request.extension_id
            ),
            extension_runtimes,
            None,
        );
    };
    let Some(extension_path) = extension.extension_path.as_deref() else {
        return failure(
            ExtensionApiOperationFailureCode::ExtensionInvalid,
            format!(
                "Extension '{}' has no installation path",
                request.extension_id
            ),
            extension_runtimes,
            None,
        );
    };
    let script_path = Path::new(extension_path).join(&config.detect_script);
    if !script_path.exists() {
        return failure(
            ExtensionApiOperationFailureCode::ExtensionInvalid,
            format!(
                "Extension '{}' component env detector is missing {}",
                extension.id,
                script_path.display()
            ),
            extension_runtimes,
            None,
        );
    }

    let working_directory = context.component_path.to_string_lossy();
    let output = match execute_capability_process(
        &script_path,
        &working_directory,
        None,
        &[],
        &request.extension_id,
        COMPONENT_ENV_CAPABILITY_ID,
    ) {
        Ok(output) => output,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
                format!(
                    "Component env detector for extension '{}' failed{}",
                    request.extension_id,
                    error
                        .process
                        .as_ref()
                        .and_then(|process| process.exit_code)
                        .map(|code| format!(" with exit code {code}"))
                        .unwrap_or_default()
                ),
                extension_runtimes,
                error.process,
            );
        }
    };
    let process = process_evidence(&output);
    let trimmed = String::from_utf8_lossy(&output.stdout);
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        return ExtensionApiComponentEnvDetectResponse {
            schema: EXTENSION_API_COMPONENT_ENV_DETECT_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            detected_runtimes: Vec::new(),
            extension_runtimes,
            failure: None,
            process: None,
        };
    }
    let detected = match serde_json::from_str::<RuntimeRequirementsConfig>(trimmed) {
        Ok(detected) => detected,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::CapabilityOutputInvalid,
                format!(
                    "parse component env detector output for extension '{}': {error}",
                    request.extension_id
                ),
                extension_runtimes,
                Some(process),
            );
        }
    };
    let mut detected_runtimes = detected
        .runtimes
        .into_iter()
        .map(|(id, requirement)| ExtensionApiRuntimeRequirement {
            id,
            version: requirement.version,
        })
        .collect::<Vec<_>>();
    detected_runtimes.sort_by(|left, right| left.id.cmp(&right.id));

    ExtensionApiComponentEnvDetectResponse {
        schema: EXTENSION_API_COMPONENT_ENV_DETECT_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        detected_runtimes,
        extension_runtimes,
        failure: None,
        process: None,
    }
}

fn failure(
    code: ExtensionApiOperationFailureCode,
    message: String,
    extension_runtimes: Vec<ExtensionApiRuntimeRequirement>,
    process: Option<homeboy_extension_contract::api::v1::ExtensionApiInvocationProcessEvidence>,
) -> ExtensionApiComponentEnvDetectResponse {
    failure_response(
        ExtensionApiOperationFailure { code, message },
        extension_runtimes,
        process,
    )
}

fn failure_response(
    failure: ExtensionApiOperationFailure,
    extension_runtimes: Vec<ExtensionApiRuntimeRequirement>,
    process: Option<homeboy_extension_contract::api::v1::ExtensionApiInvocationProcessEvidence>,
) -> ExtensionApiComponentEnvDetectResponse {
    ExtensionApiComponentEnvDetectResponse {
        schema: EXTENSION_API_COMPONENT_ENV_DETECT_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        detected_runtimes: Vec::new(),
        extension_runtimes,
        failure: Some(failure),
        process,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn install_detector(manifest: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;

        let extension_dir = homeboy_paths::extensions()
            .expect("extensions directory")
            .join("fixture");
        std::fs::create_dir_all(extension_dir.join("scripts/env")).expect("extension directory");
        std::fs::write(extension_dir.join("fixture.json"), manifest).expect("manifest");
        let script_path = extension_dir.join("scripts/env/detect.sh");
        std::fs::write(&script_path, script).expect("detector script");
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("detector executable");
    }

    fn detect_request() -> ExtensionApiComponentEnvDetectRequest {
        ExtensionApiComponentEnvDetectRequest {
            schema: EXTENSION_API_COMPONENT_ENV_DETECT_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: "fixture".to_string(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn detect_api_executes_fixture_detector_and_returns_typed_requirements() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_detector(
                r#"{"id":"fixture","name":"Fixture","version":"1.0.0","component_env":{"detect_script":"scripts/env/detect.sh"},"runtime":{"runtimes":{"node":{"version":"24"}}}}"#,
                "#!/bin/sh\nprintf '{\"runtimes\":{\"php\":{\"version\":\"8.2\"},\"node\":{\"version\":\"22\"}}}'\n",
            );
            let component = tempfile::tempdir().expect("component");
            let response = detect_component_env_api(
                &detect_request(),
                ComponentEnvDetectionContext::new(component.path()),
            );

            assert!(response.failure.is_none());
            assert!(response.process.is_none());
            assert_eq!(
                response.detected_runtimes,
                [
                    ExtensionApiRuntimeRequirement {
                        id: "node".to_string(),
                        version: "22".to_string(),
                    },
                    ExtensionApiRuntimeRequirement {
                        id: "php".to_string(),
                        version: "8.2".to_string(),
                    }
                ]
            );
            assert_eq!(
                response.extension_runtimes,
                [ExtensionApiRuntimeRequirement {
                    id: "node".to_string(),
                    version: "24".to_string(),
                }]
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn detect_api_reports_missing_capability_without_running_a_detector() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let extension_dir = homeboy_paths::extensions()
                .expect("extensions directory")
                .join("fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("fixture.json"),
                r#"{"id":"fixture","name":"Fixture","version":"1.0.0","runtime":{"runtimes":{"php":{"version":"8.3"}}}}"#,
            )
            .expect("manifest");
            let component = tempfile::tempdir().expect("component");
            let response = detect_component_env_api(
                &detect_request(),
                ComponentEnvDetectionContext::new(component.path()),
            );

            assert_eq!(
                response.failure.expect("failure").code,
                ExtensionApiOperationFailureCode::CapabilityNotProvided
            );
            assert!(response.detected_runtimes.is_empty());
            assert_eq!(
                response.extension_runtimes,
                [ExtensionApiRuntimeRequirement {
                    id: "php".to_string(),
                    version: "8.3".to_string(),
                }]
            );
            assert!(response.process.is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn detect_api_preserves_failed_detector_execution() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_detector(
                r#"{"id":"fixture","name":"Fixture","version":"1.0.0","component_env":{"detect_script":"scripts/env/detect.sh"}}"#,
                "#!/bin/sh\nprintf 'detector boom' >&2\nexit 7\n",
            );
            let component = tempfile::tempdir().expect("component");
            let response = detect_component_env_api(
                &detect_request(),
                ComponentEnvDetectionContext::new(component.path()),
            );

            assert_eq!(
                response.failure.expect("failure").code,
                ExtensionApiOperationFailureCode::CapabilityExecutionFailed
            );
            let process = response.process.expect("process evidence");
            assert_eq!(process.exit_code, Some(7));
            assert_eq!(process.stderr.trim(), "detector boom");
            assert!(response.detected_runtimes.is_empty());
        });
    }

    #[cfg(unix)]
    #[test]
    fn detect_api_rejects_invalid_detector_output() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_detector(
                r#"{"id":"fixture","name":"Fixture","version":"1.0.0","component_env":{"detect_script":"scripts/env/detect.sh"}}"#,
                "#!/bin/sh\nprintf 'not json'\n",
            );
            let component = tempfile::tempdir().expect("component");
            let response = detect_component_env_api(
                &detect_request(),
                ComponentEnvDetectionContext::new(component.path()),
            );

            assert_eq!(
                response.failure.expect("failure").code,
                ExtensionApiOperationFailureCode::CapabilityOutputInvalid
            );
            assert_eq!(
                response.process.expect("process evidence").stdout.trim(),
                "not json"
            );
            assert!(response.detected_runtimes.is_empty());
        });
    }
}

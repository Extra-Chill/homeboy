use homeboy_core::error::{Error, Result};
use homeboy_extension_contract::api::v1::{
    ExtensionApiActionInvokeRequest, ExtensionApiActionInvokeResponse,
    ExtensionApiOperationFailure, ExtensionApiOperationFailureCode, ExtensionApiResolveRequest,
    ACTION_CAPABILITY_PREFIX, EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA,
    EXTENSION_API_ACTION_INVOKE_RESPONSE_SCHEMA, EXTENSION_API_RESOLVE_REQUEST_SCHEMA,
    EXTENSION_API_V1,
};

use super::action::execute_action_implementation;
use crate::extension::catalog::{resolve_api, validate_operation_request};

pub fn invoke_action_api(
    request: &ExtensionApiActionInvokeRequest,
) -> ExtensionApiActionInvokeResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return failure_response(failure);
    }

    let capability_id = format!("{ACTION_CAPABILITY_PREFIX}{}", request.action_id);
    let resolved = resolve_api(&ExtensionApiResolveRequest {
        schema: EXTENSION_API_RESOLVE_REQUEST_SCHEMA.to_string(),
        api_version: request.api_version,
        extension_id: request.extension_id.clone(),
        capability_id,
    });
    if let Some(failure) = resolved.failure {
        return failure_response(failure);
    }

    match execute_action_implementation(
        &request.extension_id,
        &request.action_id,
        request.project_id.as_deref(),
        &request.selected,
        request.payload.as_ref(),
    ) {
        Ok(execution) => ExtensionApiActionInvokeResponse {
            schema: EXTENSION_API_ACTION_INVOKE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            output: execution.output,
            process: execution.process,
            failure: None,
        },
        Err(error) => failure_response(ExtensionApiOperationFailure {
            code: ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
            message: error.to_string(),
        }),
    }
}

/// Project a successful typed response into the value expected by workflow callers.
pub fn response_value(response: ExtensionApiActionInvokeResponse) -> Result<serde_json::Value> {
    if let Some(failure) = response.failure {
        return Err(Error::internal_unexpected(failure.message));
    }
    if let Some(output) = response.output {
        return Ok(output);
    }
    if let Some(process) = response.process {
        let exit_code = process.exit_code.unwrap_or(-1);
        return Ok(serde_json::json!({
            "stdout": process.stdout,
            "stderr": process.stderr,
            "exitCode": exit_code,
            "success": exit_code == 0,
        }));
    }
    Ok(serde_json::Value::Null)
}

pub fn invoke_action(
    extension_id: &str,
    action_id: &str,
    project_id: Option<&str>,
    selected: &[serde_json::Value],
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    response_value(invoke_action_api(&ExtensionApiActionInvokeRequest {
        schema: EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        extension_id: extension_id.to_string(),
        action_id: action_id.to_string(),
        project_id: project_id.map(str::to_string),
        selected: selected.to_vec(),
        payload: payload.cloned(),
    }))
}

fn failure_response(failure: ExtensionApiOperationFailure) -> ExtensionApiActionInvokeResponse {
    ExtensionApiActionInvokeResponse {
        schema: EXTENSION_API_ACTION_INVOKE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        output: None,
        process: None,
        failure: Some(failure),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_extension_contract::ExtensionManifest;

    #[test]
    fn command_action_resolves_through_v1_without_exposing_execution_inputs() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
                "name": "Registry",
                "version": "1.0.0",
                "actions": [{
                    "id": "release.publish",
                    "label": "Publish release",
                    "type": "command",
                    "command": "printf action-output"
                }]
            }))
            .expect("manifest");
            manifest.id = "registry".to_string();
            crate::extension::catalog::save_manifest(&manifest).expect("save manifest");

            let resolved = resolve_api(&ExtensionApiResolveRequest {
                schema: EXTENSION_API_RESOLVE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: "registry".to_string(),
                capability_id: "action.release.publish".to_string(),
            });
            let capability = resolved.capability.expect("advertised action capability");
            assert_eq!(
                capability.input_schema.map(|schema| schema.schema),
                Some(EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA.to_string())
            );
            assert_eq!(
                capability.output_schema.map(|schema| schema.schema),
                Some(EXTENSION_API_ACTION_INVOKE_RESPONSE_SCHEMA.to_string())
            );

            let response = invoke_action_api(&ExtensionApiActionInvokeRequest {
                schema: EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: "registry".to_string(),
                action_id: "release.publish".to_string(),
                project_id: None,
                selected: Vec::new(),
                payload: Some(serde_json::json!({"private_input": "must-not-echo"})),
            });

            assert!(response.failure.is_none());
            assert!(response.output.is_none());
            let process = response.process.expect("process evidence");
            assert_eq!(process.exit_code, Some(0));
            assert_eq!(process.stdout, "action-output");
            let wire = serde_json::to_string(&process).expect("response JSON");
            assert!(!wire.contains("printf action-output"));
            assert!(!wire.contains("must-not-echo"));
            assert!(!wire.contains("cwd"));
        });
    }

    #[test]
    fn action_invocation_rejects_unadvertised_action() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
                "name": "Registry",
                "version": "1.0.0"
            }))
            .expect("manifest");
            manifest.id = "registry".to_string();
            crate::extension::catalog::save_manifest(&manifest).expect("save manifest");

            let response = invoke_action_api(&ExtensionApiActionInvokeRequest {
                schema: EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: "registry".to_string(),
                action_id: "release.publish".to_string(),
                project_id: None,
                selected: Vec::new(),
                payload: None,
            });

            assert_eq!(
                response.failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::CapabilityNotProvided)
            );
        });
    }

    #[test]
    fn api_action_validation_failure_is_typed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
                "name": "Registry",
                "version": "1.0.0",
                "actions": [{
                    "id": "registry.publish",
                    "label": "Publish",
                    "type": "api",
                    "endpoint": "/publish"
                }]
            }))
            .expect("manifest");
            manifest.id = "registry".to_string();
            crate::extension::catalog::save_manifest(&manifest).expect("save manifest");

            let response = invoke_action_api(&ExtensionApiActionInvokeRequest {
                schema: EXTENSION_API_ACTION_INVOKE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: "registry".to_string(),
                action_id: "registry.publish".to_string(),
                project_id: None,
                selected: Vec::new(),
                payload: None,
            });

            let failure = response.failure.expect("typed validation failure");
            assert_eq!(
                failure.code,
                ExtensionApiOperationFailureCode::CapabilityExecutionFailed
            );
            assert!(failure.message.contains("--project is required"));
        });
    }
}

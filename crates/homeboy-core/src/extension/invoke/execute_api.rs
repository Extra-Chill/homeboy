use homeboy_core::error::{Error, Result};
use homeboy_engine_primitives::command::CapturedOutput;
use homeboy_extension_contract::api::v1::{
    ExtensionApiExecuteOutput, ExtensionApiExecuteRequest, ExtensionApiExecuteResponse,
    ExtensionApiExecuteState, ExtensionApiExecutionMode, ExtensionApiOperationFailure,
    ExtensionApiOperationFailureCode, ExtensionApiResolveRequest, EXECUTE_CAPABILITY_ID,
    EXTENSION_API_EXECUTE_IDEMPOTENCY_KEY_MAX_CHARS, EXTENSION_API_EXECUTE_REQUEST_SCHEMA,
    EXTENSION_API_EXECUTE_RESPONSE_SCHEMA, EXTENSION_API_RESOLVE_REQUEST_SCHEMA, EXTENSION_API_V1,
};

use super::environment::execute_extension_runtime;
use super::execute_idempotency::{self, IdempotencyClaim};
use super::{ExtensionExecutionMode, ExtensionRunResult, ExtensionStepFilter};
use crate::extension::catalog::{resolve_api, validate_operation_request};

pub fn execute_run_request(
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    argv: Vec<String>,
    mode: ExtensionExecutionMode,
    filter: &ExtensionStepFilter,
    idempotency_key: String,
) -> ExtensionApiExecuteRequest {
    execute_run_request_with_control_plane(
        extension_id,
        project_id,
        component_id,
        None,
        inputs,
        argv,
        mode,
        filter,
        idempotency_key,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn execute_run_request_with_control_plane(
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    control_plane: Option<homeboy_extension_contract::api::v1::ExtensionApiControlPlaneIdentity>,
    inputs: Vec<(String, String)>,
    argv: Vec<String>,
    mode: ExtensionExecutionMode,
    filter: &ExtensionStepFilter,
    idempotency_key: String,
) -> ExtensionApiExecuteRequest {
    use homeboy_extension_contract::api::v1::{
        ExtensionApiExecuteInput, ExtensionApiExecuteStepFilter,
    };

    ExtensionApiExecuteRequest {
        schema: EXTENSION_API_EXECUTE_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        extension_id: extension_id.to_string(),
        capability_id: EXECUTE_CAPABILITY_ID.to_string(),
        project_id: project_id.map(str::to_string),
        component_id: component_id.map(str::to_string),
        control_plane,
        inputs: inputs
            .into_iter()
            .map(|(id, value)| ExtensionApiExecuteInput { id, value })
            .collect(),
        argv,
        mode: match mode {
            ExtensionExecutionMode::Interactive => ExtensionApiExecutionMode::Interactive,
            ExtensionExecutionMode::Captured => ExtensionApiExecutionMode::Captured,
        },
        step_filter: (filter.step.is_some() || filter.skip.is_some()).then_some(
            ExtensionApiExecuteStepFilter {
                step: filter.step.clone(),
                skip: filter.skip.clone(),
            },
        ),
        idempotency_key,
    }
}

pub fn execute_api(request: &ExtensionApiExecuteRequest) -> ExtensionApiExecuteResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_EXECUTE_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return failure_response(failure, None);
    }
    if request.extension_id.trim().is_empty() {
        return failure(
            ExtensionApiOperationFailureCode::InvalidRequestSchema,
            "Execute requests require a non-empty extension_id".to_string(),
        );
    }
    if request.capability_id != EXECUTE_CAPABILITY_ID {
        return failure(
            ExtensionApiOperationFailureCode::CapabilityNotProvided,
            format!(
                "Execute API only invokes the '{EXECUTE_CAPABILITY_ID}' capability; received '{}'",
                request.capability_id
            ),
        );
    }
    if let Some(message) = invalid_idempotency_key(&request.idempotency_key) {
        return failure(
            ExtensionApiOperationFailureCode::InvalidIdempotencyKey,
            message,
        );
    }
    let fingerprint = request_fingerprint(request);
    match execute_idempotency::claim(&request.idempotency_key, &fingerprint) {
        Ok(IdempotencyClaim::Replayed(mut response)) => {
            response.state = Some(ExtensionApiExecuteState::Replayed);
            response
        }
        Ok(IdempotencyClaim::InProgress) => outcome(
            ExtensionApiExecuteState::InProgress,
            ExtensionApiOperationFailureCode::InvocationInProgress,
            format!(
                "Execute request with idempotency key '{}' is already in progress",
                request.idempotency_key
            ),
            Some(request.extension_id.as_str()),
        ),
        Ok(IdempotencyClaim::Conflict) => outcome(
            ExtensionApiExecuteState::Conflict,
            ExtensionApiOperationFailureCode::IdempotencyConflict,
            format!(
                "Idempotency key '{}' was reused with a different execute request",
                request.idempotency_key
            ),
            Some(request.extension_id.as_str()),
        ),
        Ok(IdempotencyClaim::Accepted) => invoke_once(request, &fingerprint),
        Err(error) => failure(
            ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
            format!("Failed to persist execute idempotency record: {error}"),
        ),
    }
}

fn validate_control_plane_identity(
    extension_id: &str,
    identity: &homeboy_extension_contract::api::v1::ExtensionApiControlPlaneIdentity,
) -> std::result::Result<(), homeboy_control_plane_contract::ControlPlaneError> {
    if identity.attempt_number == 0 {
        return Err(
            homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                "Extension execute control-plane attempt_number must be positive",
            ),
        );
    }
    let run = crate::control_plane::run(&identity.run)?;
    if run.mission.as_ref() != Some(&identity.mission) {
        return Err(
            homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                "Extension execute control-plane run does not belong to the supplied mission",
            ),
        );
    }
    let task = crate::control_plane::task(&identity.run, &identity.task)?;
    if task.run != identity.run || task.mission.as_ref() != Some(&identity.mission) {
        return Err(
            homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                "Extension execute control-plane task does not belong to the supplied run",
            ),
        );
    }
    let attempt =
        crate::control_plane::attempt(&identity.run, &identity.task, identity.attempt_number)?;
    if attempt.run != identity.run
        || attempt.task != identity.task
        || attempt.attempt != identity.attempt
        || attempt.attempt_number != identity.attempt_number
        || attempt.execution.as_ref() != Some(&identity.execution)
    {
        return Err(
            homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                "Extension execute control-plane attempt does not match its number",
            ),
        );
    }
    let execution = crate::control_plane::execution(
        &identity.run,
        &identity.task,
        identity.attempt_number,
        &identity.execution,
    )?;
    if execution.run != identity.run
        || execution.task != identity.task
        || execution.attempt != identity.attempt
        || execution.execution != identity.execution
    {
        return Err(
            homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                "Extension execute control-plane execution does not belong to the supplied attempt",
            ),
        );
    }
    if !crate::control_plane::authorize_extension_execution(
        extension_id,
        &identity.run,
        &identity.task,
    )? {
        return Err(
            homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                "Extension is not the execution owner selected by the canonical task",
            ),
        );
    }
    Ok(())
}

pub fn execute_response_result(
    response: ExtensionApiExecuteResponse,
) -> Result<ExtensionRunResult> {
    if let Some(failure) = response.failure {
        return Err(Error::config(failure.message));
    }
    let output = response.output.and_then(|output| {
        let captured = CapturedOutput::new(output.stdout, output.stderr);
        (!captured.is_empty()).then_some(captured)
    });
    Ok(ExtensionRunResult {
        exit_code: response.exit_code.unwrap_or(0),
        project_id: response.project_id,
        output,
    })
}

fn invoke_once(
    request: &ExtensionApiExecuteRequest,
    fingerprint: &serde_json::Value,
) -> ExtensionApiExecuteResponse {
    if let Some(identity) = request.control_plane.as_ref() {
        if let Err(error) = validate_control_plane_identity(&request.extension_id, identity) {
            return complete_response(
                request,
                fingerprint,
                failure(
                    ExtensionApiOperationFailureCode::InvalidRequestSchema,
                    error.message,
                ),
            );
        }
    }
    let resolved = resolve_api(&ExtensionApiResolveRequest {
        schema: EXTENSION_API_RESOLVE_REQUEST_SCHEMA.to_string(),
        api_version: request.api_version,
        extension_id: request.extension_id.clone(),
        capability_id: request.capability_id.clone(),
    });
    if let Some(failure) = resolved.failure {
        return complete_response(
            request,
            fingerprint,
            failure_response(failure, Some(request.extension_id.as_str())),
        );
    }

    let mode = match request.mode {
        ExtensionApiExecutionMode::Interactive => ExtensionExecutionMode::Interactive,
        ExtensionApiExecutionMode::Captured => ExtensionExecutionMode::Captured,
    };
    let filter = request
        .step_filter
        .as_ref()
        .map(|filter| ExtensionStepFilter {
            step: filter.step.clone(),
            skip: filter.skip.clone(),
        })
        .unwrap_or_default();
    let inputs = request
        .inputs
        .iter()
        .map(|input| (input.id.clone(), input.value.clone()))
        .collect();

    let execution = match execute_extension_runtime(
        &request.extension_id,
        request.project_id.as_deref(),
        request.component_id.as_deref(),
        request.control_plane.as_ref(),
        inputs,
        request.argv.clone(),
        None,
        None,
        mode,
        &filter,
    ) {
        Ok(execution) => execution,
        Err(error) => {
            return complete_response(
                request,
                fingerprint,
                failure_response(
                    ExtensionApiOperationFailure {
                        code: ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
                        message: error.to_string(),
                    },
                    Some(request.extension_id.as_str()),
                ),
            );
        }
    };

    let output = match request.mode {
        ExtensionApiExecutionMode::Captured if !execution.result.output.is_empty() => {
            Some(ExtensionApiExecuteOutput {
                stdout: execution.result.output.stdout,
                stderr: execution.result.output.stderr,
            })
        }
        _ => None,
    };
    let response = ExtensionApiExecuteResponse {
        schema: EXTENSION_API_EXECUTE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        extension_id: Some(request.extension_id.clone()),
        project_id: execution.project_id,
        control_plane: request.control_plane.clone(),
        exit_code: Some(execution.result.exit_code),
        output,
        state: Some(ExtensionApiExecuteState::Completed),
        failure: None,
    };
    complete_response(request, fingerprint, response)
}

fn complete_response(
    request: &ExtensionApiExecuteRequest,
    fingerprint: &serde_json::Value,
    response: ExtensionApiExecuteResponse,
) -> ExtensionApiExecuteResponse {
    if let Err(error) =
        execute_idempotency::complete(&request.idempotency_key, fingerprint, &response)
    {
        return failure(
            ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
            format!("Failed to persist execute result: {error}"),
        );
    }
    response
}

fn invalid_idempotency_key(key: &str) -> Option<String> {
    if key.is_empty() || key.chars().all(char::is_whitespace) {
        return Some("Execute requests require a non-empty idempotency key".to_string());
    }
    if key.len() > EXTENSION_API_EXECUTE_IDEMPOTENCY_KEY_MAX_CHARS {
        return Some(format!(
            "Idempotency key must be at most {EXTENSION_API_EXECUTE_IDEMPOTENCY_KEY_MAX_CHARS} characters"
        ));
    }
    if !key
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
    {
        return Some(
            "Idempotency key may contain only ASCII letters, digits, '-', '_', '.', and ':'"
                .to_string(),
        );
    }
    None
}

fn request_fingerprint(request: &ExtensionApiExecuteRequest) -> serde_json::Value {
    serde_json::json!({
        "extension_id": request.extension_id,
        "capability_id": request.capability_id,
        "project_id": request.project_id,
        "component_id": request.component_id,
        "control_plane": request.control_plane,
        "inputs": request.inputs,
        "argv": request.argv,
        "mode": request.mode,
        "step_filter": request.step_filter,
    })
}

fn failure(code: ExtensionApiOperationFailureCode, message: String) -> ExtensionApiExecuteResponse {
    failure_response(ExtensionApiOperationFailure { code, message }, None)
}

fn failure_response(
    failure: ExtensionApiOperationFailure,
    extension_id: Option<&str>,
) -> ExtensionApiExecuteResponse {
    ExtensionApiExecuteResponse {
        schema: EXTENSION_API_EXECUTE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        extension_id: extension_id.map(str::to_string),
        project_id: None,
        control_plane: None,
        exit_code: None,
        output: None,
        state: None,
        failure: Some(failure),
    }
}

fn outcome(
    state: ExtensionApiExecuteState,
    code: ExtensionApiOperationFailureCode,
    message: String,
    extension_id: Option<&str>,
) -> ExtensionApiExecuteResponse {
    let mut response =
        failure_response(ExtensionApiOperationFailure { code, message }, extension_id);
    response.state = Some(state);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_control_plane_contract::{
        AttemptId, ControlPlaneAttempt, ControlPlaneExecution, ControlPlaneRun, ControlPlaneState,
        ControlPlaneTask, ExecutionId, MissionId, RunId, TaskId, CONTROL_PLANE_ATTEMPT_SCHEMA,
        CONTROL_PLANE_EXECUTION_SCHEMA, CONTROL_PLANE_TASK_SCHEMA,
    };
    use homeboy_extension_contract::api::v1::{
        ExtensionApiControlPlaneIdentity, ExtensionApiExecuteInput,
    };
    use std::fs;

    struct ExtensionIdentityProvider;
    struct UnavailableIdentityProvider;

    impl crate::control_plane::ControlPlaneProvider for UnavailableIdentityProvider {}

    impl crate::control_plane::ControlPlaneProvider for ExtensionIdentityProvider {
        fn authorize_extension_execution(
            &self,
            extension_id: &str,
            _run: &RunId,
            _task: &TaskId,
        ) -> std::result::Result<bool, homeboy_control_plane_contract::ControlPlaneError> {
            Ok(extension_id == "fixture")
        }

        fn run(
            &self,
            requested: &RunId,
        ) -> std::result::Result<ControlPlaneRun, homeboy_control_plane_contract::ControlPlaneError>
        {
            let mut run = ControlPlaneRun::new(requested.clone());
            run.mission = Some(MissionId::new("mission-extension").unwrap());
            Ok(run)
        }

        fn task(
            &self,
            run: &RunId,
            task: &TaskId,
        ) -> std::result::Result<ControlPlaneTask, homeboy_control_plane_contract::ControlPlaneError>
        {
            Ok(ControlPlaneTask {
                schema: CONTROL_PLANE_TASK_SCHEMA.to_string(),
                mission: Some(MissionId::new("mission-extension").unwrap()),
                run: run.clone(),
                task: task.clone(),
                state: ControlPlaneState::Running,
            })
        }

        fn attempt(
            &self,
            run: &RunId,
            task: &TaskId,
            attempt_number: u32,
        ) -> std::result::Result<
            ControlPlaneAttempt,
            homeboy_control_plane_contract::ControlPlaneError,
        > {
            Ok(ControlPlaneAttempt {
                schema: CONTROL_PLANE_ATTEMPT_SCHEMA.to_string(),
                run: run.clone(),
                task: task.clone(),
                attempt: AttemptId::new("attempt-extension").unwrap(),
                attempt_number,
                state: ControlPlaneState::Running,
                started_at: String::new(),
                finished_at: None,
                execution: Some(ExecutionId::new("execution-extension").unwrap()),
            })
        }

        fn execution(
            &self,
            run: &RunId,
            task: &TaskId,
            _attempt_number: u32,
            execution: &ExecutionId,
        ) -> std::result::Result<
            ControlPlaneExecution,
            homeboy_control_plane_contract::ControlPlaneError,
        > {
            Ok(ControlPlaneExecution {
                schema: CONTROL_PLANE_EXECUTION_SCHEMA.to_string(),
                run: run.clone(),
                task: task.clone(),
                attempt: AttemptId::new("attempt-extension").unwrap(),
                execution: execution.clone(),
                state: ControlPlaneState::Running,
                started_at: String::new(),
                finished_at: None,
            })
        }
    }

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

    fn write_runnable_extension(home: &std::path::Path, id: &str, script: &str) {
        let dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": id,
                "version": "1.0.0",
                "executable": { "runtime": { "run_command": "sh {{extension_path}}/run.sh {{args}}" } }
            })
            .to_string(),
        )
        .unwrap();
        write_executable(&dir.join("run.sh"), script);
    }

    fn captured_request(
        extension_id: &str,
        key: &str,
        argv: Vec<String>,
    ) -> ExtensionApiExecuteRequest {
        ExtensionApiExecuteRequest {
            schema: EXTENSION_API_EXECUTE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: extension_id.to_string(),
            capability_id: EXECUTE_CAPABILITY_ID.to_string(),
            project_id: None,
            component_id: None,
            control_plane: None,
            inputs: Vec::new(),
            argv,
            mode: ExtensionApiExecutionMode::Captured,
            step_filter: None,
            idempotency_key: key.to_string(),
        }
    }

    #[test]
    fn execute_api_rejects_invalid_schema_and_idempotency_keys() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut request = captured_request("fixture", "run-1", Vec::new());
            request.schema = "homeboy/extension-api-execute-request/v0".to_string();
            assert_eq!(
                execute_api(&request).failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::InvalidRequestSchema)
            );

            request.schema = EXTENSION_API_EXECUTE_REQUEST_SCHEMA.to_string();
            request.idempotency_key.clear();
            assert_eq!(
                execute_api(&request).failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::InvalidIdempotencyKey)
            );

            request.idempotency_key = "bad key".to_string();
            assert_eq!(
                execute_api(&request).failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::InvalidIdempotencyKey)
            );
        });
    }

    #[test]
    fn execute_api_rejects_non_execute_capabilities() {
        homeboy_core::test_support::with_isolated_home(|home| {
            write_runnable_extension(home.path(), "fixture", "#!/bin/sh\nprintf once\n");
            let mut request = captured_request("fixture", "run-1", Vec::new());
            request.capability_id = "test".to_string();
            assert_eq!(
                execute_api(&request).failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::CapabilityNotProvided)
            );
        });
    }

    #[test]
    fn execute_api_replays_duplicate_idempotency_keys_without_a_second_run() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let marker = home.path().join("ran");
            write_runnable_extension(
                home.path(),
                "fixture",
                &format!(
                    "#!/bin/sh\nprintf x >> '{}'\nprintf out\n",
                    marker.display()
                ),
            );
            let request = captured_request("fixture", "run-1", Vec::new());
            let first = execute_api(&request);
            fs::remove_dir_all(home.path().join(".config/homeboy/extensions/fixture"))
                .expect("remove extension after execution");
            let second = execute_api(&request);

            assert_eq!(first.state, Some(ExtensionApiExecuteState::Completed));
            assert_eq!(second.state, Some(ExtensionApiExecuteState::Replayed));
            assert_eq!(first.exit_code, Some(0));
            assert_eq!(second.exit_code, first.exit_code);
            assert_eq!(second.output, first.output);
            assert_eq!(fs::read_to_string(&marker).expect("marker"), "x");
        });
    }

    #[test]
    fn execute_api_replays_terminal_runtime_failures() {
        homeboy_core::test_support::with_isolated_home(|home| {
            write_runnable_extension(home.path(), "fixture", "#!/bin/sh\nprintf out\n");
            let mut request = captured_request("fixture", "run-failure", Vec::new());
            request.component_id = Some("missing-component".to_string());

            let first = execute_api(&request);
            let second = execute_api(&request);

            assert_eq!(
                first.failure.as_ref().map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::CapabilityExecutionFailed)
            );
            assert_eq!(second.state, Some(ExtensionApiExecuteState::Replayed));
            assert_eq!(second.failure, first.failure);
        });
    }

    #[test]
    fn execute_api_conflicts_when_the_same_key_covers_a_different_request() {
        homeboy_core::test_support::with_isolated_home(|home| {
            write_runnable_extension(home.path(), "fixture", "#!/bin/sh\nprintf out\n");
            let first = execute_api(&captured_request("fixture", "run-1", vec!["a".to_string()]));
            let second = execute_api(&captured_request("fixture", "run-1", vec!["b".to_string()]));
            assert_eq!(first.state, Some(ExtensionApiExecuteState::Completed));
            assert_eq!(second.state, Some(ExtensionApiExecuteState::Conflict));
            assert_eq!(
                second.failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::IdempotencyConflict)
            );
        });
    }

    #[test]
    fn execute_api_preserves_captured_output_and_structured_inputs() {
        homeboy_core::test_support::with_isolated_home(|home| {
            write_runnable_extension(home.path(), "fixture", "#!/bin/sh\nprintf '%s' \"$1\"\n");
            let mut request = captured_request("fixture", "run-inputs", vec!["arg".to_string()]);
            request.inputs = vec![ExtensionApiExecuteInput {
                id: "unused".to_string(),
                value: "private-input-value".to_string(),
            }];
            let response = execute_api(&request);
            assert!(response.failure.is_none());
            assert_eq!(
                response.output.map(|output| output.stdout),
                Some("arg".to_string())
            );
            let records = home
                .path()
                .join(".local/share/homeboy/extension-execute-idempotency");
            let record_path = fs::read_dir(records)
                .expect("idempotency records")
                .next()
                .expect("idempotency record")
                .expect("idempotency entry")
                .path();
            assert!(!fs::read_to_string(record_path)
                .expect("idempotency record contents")
                .contains("private-input-value"));
        });
    }

    #[test]
    fn execute_api_carries_canonical_identity_into_the_extension_and_response() {
        homeboy_core::test_support::with_isolated_home(|home| {
            crate::control_plane::register_control_plane_provider(Box::new(
                ExtensionIdentityProvider,
            ));
            write_runnable_extension(
                home.path(),
                "fixture",
                "#!/bin/sh\nprintf '%s|%s|%s|%s|%s' \"$HOMEBOY_CONTROL_PLANE_MISSION_ID\" \"$HOMEBOY_CONTROL_PLANE_RUN_ID\" \"$HOMEBOY_CONTROL_PLANE_TASK_ID\" \"$HOMEBOY_CONTROL_PLANE_ATTEMPT_ID\" \"$HOMEBOY_CONTROL_PLANE_EXECUTION_ID\"\n",
            );
            let identity = ExtensionApiControlPlaneIdentity {
                mission: MissionId::new("mission-extension").unwrap(),
                run: RunId::new("run-extension").unwrap(),
                task: TaskId::new("task-extension").unwrap(),
                attempt: AttemptId::new("attempt-extension").unwrap(),
                attempt_number: 1,
                execution: ExecutionId::new("execution-extension").unwrap(),
            };
            let request = execute_run_request_with_control_plane(
                "fixture",
                None,
                None,
                Some(identity.clone()),
                Vec::new(),
                Vec::new(),
                ExtensionExecutionMode::Captured,
                &ExtensionStepFilter::default(),
                "identity-run".to_string(),
            );

            let response = execute_api(&request);

            assert_eq!(response.control_plane, Some(identity));
            assert_eq!(
                response.output.map(|output| output.stdout),
                Some("mission-extension|run-extension|task-extension|attempt-extension|execution-extension".to_string())
            );
        });
    }

    #[test]
    fn execute_api_rejects_unverified_canonical_identity() {
        homeboy_core::test_support::with_isolated_home(|_| {
            crate::control_plane::register_control_plane_provider(Box::new(
                ExtensionIdentityProvider,
            ));
            let mut request = captured_request("fixture", "forged-identity", Vec::new());
            request.control_plane = Some(ExtensionApiControlPlaneIdentity {
                mission: MissionId::new("different-mission").unwrap(),
                run: RunId::new("run-extension").unwrap(),
                task: TaskId::new("task-extension").unwrap(),
                attempt: AttemptId::new("attempt-extension").unwrap(),
                attempt_number: 1,
                execution: ExecutionId::new("execution-extension").unwrap(),
            });

            let response = execute_api(&request);

            assert_eq!(
                response.failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::InvalidRequestSchema)
            );
        });
    }

    #[test]
    fn execute_api_rejects_an_extension_that_does_not_own_the_canonical_task() {
        homeboy_core::test_support::with_isolated_home(|_| {
            crate::control_plane::register_control_plane_provider(Box::new(
                ExtensionIdentityProvider,
            ));
            let mut request = captured_request("different-extension", "wrong-owner", Vec::new());
            request.control_plane = Some(ExtensionApiControlPlaneIdentity {
                mission: MissionId::new("mission-extension").unwrap(),
                run: RunId::new("run-extension").unwrap(),
                task: TaskId::new("task-extension").unwrap(),
                attempt: AttemptId::new("attempt-extension").unwrap(),
                attempt_number: 1,
                execution: ExecutionId::new("execution-extension").unwrap(),
            });

            let response = execute_api(&request);

            assert_eq!(
                response.failure.map(|failure| failure.code),
                Some(ExtensionApiOperationFailureCode::InvalidRequestSchema)
            );
        });
    }

    #[test]
    fn execute_api_replays_verified_identity_after_control_plane_becomes_unavailable() {
        homeboy_core::test_support::with_isolated_home(|home| {
            crate::control_plane::register_control_plane_provider(Box::new(
                ExtensionIdentityProvider,
            ));
            write_runnable_extension(home.path(), "fixture", "#!/bin/sh\nprintf ok\n");
            let identity = ExtensionApiControlPlaneIdentity {
                mission: MissionId::new("mission-extension").unwrap(),
                run: RunId::new("run-extension").unwrap(),
                task: TaskId::new("task-extension").unwrap(),
                attempt: AttemptId::new("attempt-extension").unwrap(),
                attempt_number: 1,
                execution: ExecutionId::new("execution-extension").unwrap(),
            };
            let request = execute_run_request_with_control_plane(
                "fixture",
                None,
                None,
                Some(identity),
                Vec::new(),
                Vec::new(),
                ExtensionExecutionMode::Captured,
                &ExtensionStepFilter::default(),
                "identity-replay".to_string(),
            );
            assert_eq!(
                execute_api(&request).state,
                Some(ExtensionApiExecuteState::Completed)
            );
            crate::control_plane::register_control_plane_provider(Box::new(
                UnavailableIdentityProvider,
            ));

            let replay = execute_api(&request);

            assert_eq!(replay.state, Some(ExtensionApiExecuteState::Replayed));
            assert!(replay.failure.is_none());
        });
    }
}

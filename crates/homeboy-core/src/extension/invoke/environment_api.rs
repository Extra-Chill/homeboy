use std::collections::BTreeMap;
use std::path::Path;

use homeboy_core::error::{Error, Result as HomeboyResult};
use homeboy_core::redaction::RedactionPolicy;
use homeboy_core::runner_job_execution_context::RunnerJobExecutionContext;
use homeboy_extension_contract::api::v1::{
    ExtensionApiEnvironmentContribution, ExtensionApiEnvironmentResolveRequest,
    ExtensionApiEnvironmentResolveResponse, ExtensionApiOperationFailure,
    ExtensionApiOperationFailureCode, ExtensionApiResolveRequest, ENVIRONMENT_CAPABILITY_ID,
    EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA,
    EXTENSION_API_ENVIRONMENT_RESOLVE_RESPONSE_SCHEMA, EXTENSION_API_RESOLVE_REQUEST_SCHEMA,
    EXTENSION_API_V1,
};

use crate::extension::catalog::{
    load_extension, load_extension_from_dir, resolve_api, validate_operation_request,
};

use super::api::{execute_capability_process, process_evidence};

pub const ENV_PROVIDER_COMMAND_PAYLOAD_ENV: &str = "HOMEBOY_ENV_PROVIDER_COMMAND_PAYLOAD";
const ENV_PROVIDER_COMMAND_PAYLOAD_SCHEMA: &str = "homeboy/env-provider-command/v1";
const MAX_PROVIDER_COMMAND_PAYLOAD_BYTES: usize = 8 * 1024;

/// Runtime-only inputs that must not enter the serialized Extension API request.
pub struct EnvironmentResolutionContext<'a> {
    execution_context: &'a RunnerJobExecutionContext,
    working_directory: &'a Path,
    base_env: &'a [(String, String)],
    extension_directory: Option<&'a Path>,
}

impl<'a> EnvironmentResolutionContext<'a> {
    pub fn installed(
        execution_context: &'a RunnerJobExecutionContext,
        working_directory: &'a Path,
        base_env: &'a [(String, String)],
    ) -> Self {
        Self {
            execution_context,
            working_directory,
            base_env,
            extension_directory: None,
        }
    }

    pub(crate) fn from_directory(
        execution_context: &'a RunnerJobExecutionContext,
        working_directory: &'a Path,
        base_env: &'a [(String, String)],
        extension_directory: &'a Path,
    ) -> Self {
        Self {
            execution_context,
            working_directory,
            base_env,
            extension_directory: Some(extension_directory),
        }
    }
}

#[derive(serde::Serialize)]
struct EnvProviderCommandPayload<'a> {
    schema: &'static str,
    execution_context: &'a RunnerJobExecutionContext,
}

pub fn resolve_environment_api(
    request: &ExtensionApiEnvironmentResolveRequest,
    context: EnvironmentResolutionContext<'_>,
) -> ExtensionApiEnvironmentResolveResponse {
    resolve_environment(request, context, true)
}

pub(crate) fn resolve_environment_from_directory(
    execution_context: &RunnerJobExecutionContext,
    extension_id: &str,
    extension_directory: &Path,
    working_directory: &Path,
    base_env: &[(String, String)],
) -> HomeboyResult<Vec<(String, String)>> {
    let response = resolve_environment(
        &ExtensionApiEnvironmentResolveRequest {
            schema: EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: extension_id.to_string(),
        },
        EnvironmentResolutionContext::from_directory(
            execution_context,
            working_directory,
            base_env,
            extension_directory,
        ),
        false,
    );
    if let Some(failure) = response.failure {
        if matches!(
            failure.code,
            ExtensionApiOperationFailureCode::CapabilityNotProvided
                | ExtensionApiOperationFailureCode::ExtensionInvalid
        ) {
            return Ok(Vec::new());
        }
        return Err(Error::validation_invalid_argument(
            "extension_env",
            failure.message,
            Some(extension_id.to_string()),
            None,
        ));
    }
    response
        .contribution
        .map(|contribution| contribution.public_env)
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Environment resolution for extension '{extension_id}' returned no contribution"
            ))
        })
}

fn resolve_environment(
    request: &ExtensionApiEnvironmentResolveRequest,
    context: EnvironmentResolutionContext<'_>,
    retain_public_provenance: bool,
) -> ExtensionApiEnvironmentResolveResponse {
    let explicit_directory = context.extension_directory.is_some();
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return failure_response(failure, None);
    }

    if context.extension_directory.is_none() {
        let resolved = resolve_api(&ExtensionApiResolveRequest {
            schema: EXTENSION_API_RESOLVE_REQUEST_SCHEMA.to_string(),
            api_version: request.api_version,
            extension_id: request.extension_id.clone(),
            capability_id: ENVIRONMENT_CAPABILITY_ID.to_string(),
        });
        if let Some(failure) = resolved.failure {
            return failure_response(failure, None);
        }
    }

    let extension = match context.extension_directory {
        Some(directory) => load_extension_from_dir(directory),
        None => load_extension(&request.extension_id),
    };
    let extension = match extension {
        Ok(extension) => extension,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::ExtensionInvalid,
                error.to_string(),
                None,
            );
        }
    };
    if !explicit_directory && extension.id != request.extension_id {
        return failure(
            ExtensionApiOperationFailureCode::ExtensionInvalid,
            format!(
                "Extension directory resolved '{}' instead of requested '{}'",
                extension.id, request.extension_id
            ),
            None,
        );
    }
    let Some(config) = extension.env_provider.as_ref() else {
        return failure(
            ExtensionApiOperationFailureCode::CapabilityNotProvided,
            format!(
                "Extension '{}' does not provide capability '{ENVIRONMENT_CAPABILITY_ID}'",
                request.extension_id
            ),
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
            None,
        );
    };
    let payload = match provider_command_payload(context.execution_context) {
        Ok(payload) => payload,
        Err(failure) => return failure_response(failure, None),
    };
    if context
        .base_env
        .iter()
        .any(|(key, _)| key == ENV_PROVIDER_COMMAND_PAYLOAD_ENV)
    {
        return failure(
            ExtensionApiOperationFailureCode::InvalidRequestSchema,
            format!("request environment cannot override {ENV_PROVIDER_COMMAND_PAYLOAD_ENV}"),
            None,
        );
    }

    let mut environment = context.base_env.to_vec();
    environment.push((ENV_PROVIDER_COMMAND_PAYLOAD_ENV.to_string(), payload));
    let mut secret_env_names = config.secret_env.clone();
    secret_env_names.sort();
    secret_env_names.dedup();
    let script_path = Path::new(extension_path).join(&config.script);
    let working_directory = context.working_directory.to_string_lossy();
    let output = match execute_capability_process(
        &script_path,
        &working_directory,
        None,
        &environment,
        &request.extension_id,
        ENVIRONMENT_CAPABILITY_ID,
    ) {
        Ok(output) => output,
        Err(error) => {
            return failure(
                ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
                format!(
                    "Capability '{ENVIRONMENT_CAPABILITY_ID}' failed for extension '{}'",
                    request.extension_id
                ),
                error.process.map(|process| {
                    safe_process_evidence(process, context.base_env, &secret_env_names)
                }),
            );
        }
    };
    let process = safe_process_evidence(
        process_evidence(&output),
        context.base_env,
        &secret_env_names,
    );
    let public_env = if output.stdout.iter().all(u8::is_ascii_whitespace) {
        BTreeMap::new()
    } else {
        match serde_json::from_slice::<BTreeMap<String, String>>(&output.stdout) {
            Ok(values) => values,
            Err(error) => {
                return failure(
                    ExtensionApiOperationFailureCode::CapabilityOutputInvalid,
                    format!(
                        "Capability '{ENVIRONMENT_CAPABILITY_ID}' returned invalid environment JSON for extension '{}': {error}",
                        request.extension_id
                    ),
                    Some(process),
                );
            }
        }
    };
    if let Some(name) = public_env.keys().find(|name| {
        secret_env_names.iter().any(|secret| secret == *name)
            || (retain_public_provenance && RedactionPolicy::default().is_sensitive_key(name))
    }) {
        return failure(
            ExtensionApiOperationFailureCode::CapabilityOutputInvalid,
            format!(
                "Extension '{}' emitted sensitive '{name}' as public env",
                request.extension_id
            ),
            Some(process),
        );
    }

    ExtensionApiEnvironmentResolveResponse {
        schema: EXTENSION_API_ENVIRONMENT_RESOLVE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        contribution: Some(ExtensionApiEnvironmentContribution {
            extension_id: extension.id,
            version: extension.version,
            public_env: public_env.into_iter().collect(),
            secret_env_names,
        }),
        failure: None,
        process: None,
    }
}

fn safe_process_evidence(
    mut process: homeboy_extension_contract::api::v1::ExtensionApiInvocationProcessEvidence,
    base_env: &[(String, String)],
    declared_secret_names: &[String],
) -> homeboy_extension_contract::api::v1::ExtensionApiInvocationProcessEvidence {
    let policy = declared_secret_names
        .iter()
        .fold(RedactionPolicy::default(), |policy, name| {
            policy.with_sensitive_key(name)
        });
    let secret_values = base_env
        .iter()
        .filter(|(name, value)| {
            !value.is_empty()
                && (declared_secret_names.iter().any(|secret| secret == name)
                    || policy.is_sensitive_key(name))
        })
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    process.stderr = redact_known_values(policy.redact_string(&process.stderr), &secret_values);
    process.stdout = match process.parsed_output.as_ref() {
        Some(value) => serde_json::to_string(&policy.redact_json(value)).unwrap_or_default(),
        None => policy.redact_string(&process.stdout),
    };
    process.stdout = redact_known_values(process.stdout, &secret_values);
    process.parsed_output = serde_json::from_str(&process.stdout).ok();
    process
}

fn redact_known_values(mut text: String, secret_values: &[&String]) -> String {
    for value in secret_values {
        text = text.replace(value.as_str(), "[REDACTED]");
    }
    text
}

pub fn declared_environment_secret_names(extension_id: &str) -> HomeboyResult<Vec<String>> {
    let extension = load_extension(extension_id)?;
    let Some(config) = extension.env_provider else {
        return Err(Error::validation_invalid_argument(
            "extension_env",
            format!("Extension '{extension_id}' does not declare an env_provider"),
            Some(extension_id.to_string()),
            None,
        ));
    };
    let mut names = config.secret_env;
    names.sort();
    names.dedup();
    Ok(names)
}

fn provider_command_payload(
    execution_context: &RunnerJobExecutionContext,
) -> Result<String, ExtensionApiOperationFailure> {
    execution_context
        .verify_integrity()
        .map_err(|_| ExtensionApiOperationFailure {
            code: ExtensionApiOperationFailureCode::InvalidRequestSchema,
            message:
                "extension environment providers require authenticated runner execution context"
                    .to_string(),
        })?;
    let payload = serde_json::to_string(&EnvProviderCommandPayload {
        schema: ENV_PROVIDER_COMMAND_PAYLOAD_SCHEMA,
        execution_context,
    })
    .map_err(|error| ExtensionApiOperationFailure {
        code: ExtensionApiOperationFailureCode::InvalidRequestSchema,
        message: format!("Failed to serialize environment provider context: {error}"),
    })?;
    if payload.len() > MAX_PROVIDER_COMMAND_PAYLOAD_BYTES {
        return Err(ExtensionApiOperationFailure {
            code: ExtensionApiOperationFailureCode::InvalidRequestSchema,
            message: "extension environment provider command payload exceeds its bounded record"
                .to_string(),
        });
    }
    Ok(payload)
}

fn failure(
    code: ExtensionApiOperationFailureCode,
    message: String,
    process: Option<homeboy_extension_contract::api::v1::ExtensionApiInvocationProcessEvidence>,
) -> ExtensionApiEnvironmentResolveResponse {
    failure_response(ExtensionApiOperationFailure { code, message }, process)
}

fn failure_response(
    failure: ExtensionApiOperationFailure,
    process: Option<homeboy_extension_contract::api::v1::ExtensionApiInvocationProcessEvidence>,
) -> ExtensionApiEnvironmentResolveResponse {
    ExtensionApiEnvironmentResolveResponse {
        schema: EXTENSION_API_ENVIRONMENT_RESOLVE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        contribution: None,
        failure: Some(failure),
        process,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn install_provider(manifest: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;

        let extension_dir = homeboy_paths::extensions()
            .expect("extensions directory")
            .join("fixture");
        std::fs::create_dir_all(&extension_dir).expect("extension directory");
        std::fs::write(extension_dir.join("fixture.json"), manifest).expect("manifest");
        let script_path = extension_dir.join("env.sh");
        std::fs::write(&script_path, script).expect("provider script");
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("provider executable");
    }

    #[test]
    fn command_payload_carries_verified_identity_without_claim_material() {
        let secret = "reservation-secret";
        let context = RunnerJobExecutionContext::direct_daemon(
            Some("run-1"),
            "runner-1",
            "job-1",
            "homeboy",
            secret,
        )
        .expect("context");
        let payload = provider_command_payload(&context).expect("payload");
        let value: serde_json::Value = serde_json::from_str(&payload).expect("JSON payload");

        assert_eq!(value["schema"], ENV_PROVIDER_COMMAND_PAYLOAD_SCHEMA);
        assert_eq!(value["execution_context"]["id"], context.id());
        assert!(!payload.contains(secret));
        assert!(payload.contains("claim_ref"));
    }

    #[cfg(unix)]
    #[test]
    fn environment_api_uses_private_context_and_returns_public_provenance() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_provider(
                r#"{"id":"fixture","name":"Fixture","version":"1.2.3","env_provider":{"script":"env.sh","secret_env":["FIXTURE_SECRET"]}}"#,
                "#!/bin/sh\ntest \"$PRIVATE_BASE\" = runtime-only || exit 23\ntest -n \"$HOMEBOY_ENV_PROVIDER_COMMAND_PAYLOAD\" || exit 24\nprintf '%s\\n' '{\"PUBLIC_PATH\":\"/opt/fixture/bin\"}'\n",
            );
            let working_directory = tempfile::tempdir().expect("working directory");
            let response = resolve_environment_api(
                &ExtensionApiEnvironmentResolveRequest {
                    schema: EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA.to_string(),
                    api_version: EXTENSION_API_V1,
                    extension_id: "fixture".to_string(),
                },
                EnvironmentResolutionContext::installed(
                    &RunnerJobExecutionContext::local("homeboy"),
                    working_directory.path(),
                    &[("PRIVATE_BASE".to_string(), "runtime-only".to_string())],
                ),
            );

            assert!(response.failure.is_none());
            assert!(response.process.is_none());
            let contribution = response.contribution.expect("contribution");
            assert_eq!(contribution.version, "1.2.3");
            assert_eq!(
                contribution.public_env,
                [("PUBLIC_PATH".to_string(), "/opt/fixture/bin".to_string())]
            );
            assert_eq!(contribution.secret_env_names, ["FIXTURE_SECRET"]);
        });
    }

    #[cfg(unix)]
    #[test]
    fn environment_api_preserves_failed_process_evidence() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_provider(
                r#"{"id":"fixture","name":"Fixture","version":"1.0.0","env_provider":{"script":"env.sh","secret_env":["FIXTURE_TOKEN"]}}"#,
                "#!/bin/sh\nprintf '%s' '{\"detail\":\"failed\"}'\nprintf 'provider failed:%s' \"$FIXTURE_TOKEN\" >&2\nexit 7\n",
            );
            let working_directory = tempfile::tempdir().expect("working directory");
            let response = resolve_environment_api(
                &ExtensionApiEnvironmentResolveRequest {
                    schema: EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA.to_string(),
                    api_version: EXTENSION_API_V1,
                    extension_id: "fixture".to_string(),
                },
                EnvironmentResolutionContext::installed(
                    &RunnerJobExecutionContext::local("homeboy"),
                    working_directory.path(),
                    &[("FIXTURE_TOKEN".to_string(), "runtime-secret".to_string())],
                ),
            );

            assert_eq!(
                response.failure.expect("failure").code,
                ExtensionApiOperationFailureCode::CapabilityExecutionFailed
            );
            let process = response.process.expect("process evidence");
            assert_eq!(process.exit_code, Some(7));
            assert_eq!(process.stderr, "provider failed:[REDACTED]");
            assert_eq!(
                process.parsed_output,
                Some(serde_json::json!({"detail": "failed"}))
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn installed_environment_api_rejects_declared_secrets_as_public_output() {
        homeboy_core::test_support::with_isolated_home(|_| {
            install_provider(
                r#"{"id":"fixture","name":"Fixture","version":"1.0.0","env_provider":{"script":"env.sh","secret_env":["BLORP"]}}"#,
                "#!/bin/sh\nprintf '%s\\n' '{\"BLORP\":\"must-not-serialize\"}'\n",
            );
            let working_directory = tempfile::tempdir().expect("working directory");
            let response = resolve_environment_api(
                &ExtensionApiEnvironmentResolveRequest {
                    schema: EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA.to_string(),
                    api_version: EXTENSION_API_V1,
                    extension_id: "fixture".to_string(),
                },
                EnvironmentResolutionContext::installed(
                    &RunnerJobExecutionContext::local("homeboy"),
                    working_directory.path(),
                    &[],
                ),
            );

            assert_eq!(
                response.failure.as_ref().expect("failure").code,
                ExtensionApiOperationFailureCode::CapabilityOutputInvalid
            );
            assert!(response.contribution.is_none());
            assert!(!serde_json::to_string(&response)
                .expect("response JSON")
                .contains("must-not-serialize"));

            let extension_directory = homeboy_paths::extensions()
                .expect("extensions directory")
                .join("fixture");
            let explicit_error = resolve_environment_from_directory(
                &RunnerJobExecutionContext::local("homeboy"),
                "fixture",
                &extension_directory,
                working_directory.path(),
                &[],
            )
            .expect_err("declared secret output must fail");
            assert!(explicit_error.message.contains("BLORP"));
        });
    }
}

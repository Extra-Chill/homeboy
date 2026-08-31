use std::collections::HashMap;
use std::path::Path;

use homeboy_core::error::{Error, Result};
use homeboy_core::runner_job_execution_context::RunnerJobExecutionContext;
use homeboy_extension_contract::api::v1::{
    ExtensionApiEnvironmentContribution, ExtensionApiEnvironmentResolveRequest,
    ExtensionApiOperationFailureCode, EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA,
    EXTENSION_API_V1,
};

use super::environment_api::{
    resolve_environment_api, resolve_environment_from_directory, EnvironmentResolutionContext,
};

pub type EnvProviderContribution = ExtensionApiEnvironmentContribution;

/// Resolve an installed extension's environment contribution on the machine
/// that will execute the workload. The result contains no secret values.
pub fn resolve_installed(
    execution_context: &RunnerJobExecutionContext,
    extension_id: &str,
    component_path: &Path,
    base_env: &[(String, String)],
) -> Result<EnvProviderContribution> {
    resolve(execution_context, extension_id, component_path, base_env)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension_env",
            format!("Extension '{extension_id}' does not declare an env_provider"),
            Some(extension_id.to_string()),
            Some(vec![format!(
                "Add env_provider.script to the '{extension_id}' extension manifest."
            )]),
        )
    })
}

/// Resolve providers in request order and reject any ambiguous contribution
/// before the workload can start. The returned values contain no secrets.
pub fn resolve_installed_all(
    execution_context: &RunnerJobExecutionContext,
    extension_ids: &[String],
    component_path: &Path,
    base_env: &[(String, String)],
) -> Result<Vec<EnvProviderContribution>> {
    if execution_context.verify_integrity().is_err() {
        return Err(Error::validation_invalid_argument(
            "execution_context",
            "extension environment providers require authenticated runner execution context",
            None,
            Some(vec![
                "Claim a fresh runner job through the controller before retrying.".to_string(),
            ]),
        ));
    }
    let mut effective_env = base_env.to_vec();
    let mut owners = HashMap::new();
    for (name, _) in base_env {
        owners.insert(name.clone(), "request".to_string());
    }
    let mut contributions = Vec::new();
    for extension_id in extension_ids {
        let contribution = resolve_installed(
            execution_context,
            extension_id,
            component_path,
            &effective_env,
        )?;
        for (name, value) in &contribution.public_env {
            if let Some(owner) = owners.get(name) {
                return Err(Error::validation_invalid_argument(
                    "extension_env",
                    format!(
                        "Extension '{}' contributes '{name}', which is already set by {owner}",
                        contribution.extension_id
                    ),
                    Some(name.clone()),
                    Some(vec!["Use one provider for each environment variable or remove the conflicting request environment value.".to_string()]),
                ));
            }
            owners.insert(name.clone(), contribution.extension_id.clone());
            effective_env.push((name.clone(), value.clone()));
        }
        contributions.push(contribution);
    }
    Ok(contributions)
}

/// Resolve an optional provider from an explicit extension directory.
pub(crate) fn env_vars(
    execution_context: &RunnerJobExecutionContext,
    extension_id: &str,
    extension_directory: Option<&Path>,
    component_path: &Path,
    base_env: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    if let Some(extension_directory) = extension_directory {
        return resolve_environment_from_directory(
            execution_context,
            extension_id,
            extension_directory,
            component_path,
            base_env,
        );
    }
    Ok(
        resolve(execution_context, extension_id, component_path, base_env)?
            .map(|contribution| contribution.public_env)
            .unwrap_or_default(),
    )
}

fn resolve(
    execution_context: &RunnerJobExecutionContext,
    extension_id: &str,
    component_path: &Path,
    base_env: &[(String, String)],
) -> Result<Option<EnvProviderContribution>> {
    let response = resolve_environment_api(
        &ExtensionApiEnvironmentResolveRequest {
            schema: EXTENSION_API_ENVIRONMENT_RESOLVE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: extension_id.to_string(),
        },
        EnvironmentResolutionContext::installed(execution_context, component_path, base_env),
    );
    if let Some(failure) = response.failure {
        if failure.code == ExtensionApiOperationFailureCode::CapabilityNotProvided {
            return Ok(None);
        }
        return Err(Error::validation_invalid_argument(
            "extension_env",
            failure.message,
            Some(extension_id.to_string()),
            None,
        ));
    }
    response.contribution.map(Some).ok_or_else(|| {
        Error::internal_unexpected(format!(
            "Environment resolution for extension '{extension_id}' returned no contribution"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn install_provider_fixture(root: &Path, id: &str, output: &str) {
        use std::os::unix::fs::PermissionsExt;

        let source = root.join(id);
        std::fs::create_dir_all(&source).expect("provider source directory");
        std::fs::write(
            source.join(format!("{id}.json")),
            format!(
                r#"{{"id":"{id}","name":"{id}","version":"1.0.0","env_provider":{{"script":"env.sh"}}}}"#
            ),
        )
        .expect("provider manifest");
        let script = source.join("env.sh");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{output}'\n"))
            .expect("provider script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("provider executable");
        crate::extension::lifecycle::install(&source.display().to_string(), Some(id))
            .expect("install provider fixture");
    }

    #[cfg(unix)]
    #[test]
    fn providers_cannot_contribute_the_same_public_environment_key() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("provider fixtures");
            install_provider_fixture(root.path(), "first", r#"{"SHARED_KEY":"first"}"#);
            install_provider_fixture(root.path(), "second", r#"{"SHARED_KEY":"second"}"#);

            let error = resolve_installed_all(
                &RunnerJobExecutionContext::local("homeboy"),
                &["first".to_string(), "second".to_string()],
                root.path(),
                &[],
            )
            .expect_err("provider-provider public env collision must fail");

            assert!(error.message.contains("SHARED_KEY"));
        });
    }
}

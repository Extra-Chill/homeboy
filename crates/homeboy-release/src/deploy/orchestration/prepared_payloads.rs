use std::collections::HashMap;

use homeboy_core::component::Component;
use homeboy_core::error::Error;
use homeboy_core::project::Project;

use super::super::binding::bind_project_payloads;
use super::super::execution::{prepare_component_deploy, PreparedComponentDeploy};
use super::super::preparation::{ComponentPayloadPreparationRequest, PreparedPayloadCollection};
use super::super::types::{ComponentDeployResult, DeployConfig};
use homeboy_core::git::release_download::{ReleaseArtifactLease, ReleaseArtifactStore};

pub(super) struct PreparedDeployments {
    deployments: Vec<PreparedComponentDeploy>,
    // Retains payload copies and release leases through every transfer.
    _payloads: PreparedPayloadCollection,
}

impl std::ops::Deref for PreparedDeployments {
    type Target = [PreparedComponentDeploy];

    fn deref(&self) -> &Self::Target {
        &self.deployments
    }
}

pub(super) fn prepare_component_deployments(
    components: &[Component],
    config: &DeployConfig,
    project: &Project,
    base_path: &str,
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    release_artifacts: &HashMap<String, ReleaseArtifactLease>,
) -> std::result::Result<PreparedDeployments, Vec<ComponentDeployResult>> {
    let mut prepared_deployments = Vec::new();
    let mut failures = Vec::new();
    let mut payloads = PreparedPayloadCollection::default();
    let mut release_artifact_store = ReleaseArtifactStore::default();

    let mut binding_payloads = config
        .prepared_artifact
        .as_ref()
        .map(|artifact| HashMap::from([(artifact.component_id.clone(), artifact.clone())]))
        .unwrap_or_default();
    for component in components {
        let source_path = component.local_path.clone();
        let mut component = homeboy_core::project::apply_component_overrides(component, project);
        if config.requested_ref_for(&component.id).is_some() {
            component.local_path = source_path;
        }
        let effective_config = config.clone();
        let is_artifact_deploy =
            !component.deploy_config().is_git_deploy() && !component.is_file_component();
        let effective_config =
            if is_artifact_deploy && !config.skip_build && config.prepared_artifact.is_none() {
                let mut preparation_config = effective_config.clone();
                // The existing detached checkout is the authoritative exact-ref source.
                preparation_config.requested_ref = None;
                preparation_config.requested_refs.clear();
                let mut request =
                    ComponentPayloadPreparationRequest::new(&component, &preparation_config);
                request.config.exact_ref_materialized =
                    config.requested_ref_for(&component.id).is_some();
                if let Some(lease) = release_artifacts.get(&component.id).cloned() {
                    if let Err(error) = payloads.insert(request.clone(), Some(lease)) {
                        failures.push(ComponentDeployResult::failed(
                            &component,
                            base_path,
                            local_versions.get(&component.id).cloned(),
                            remote_versions.get(&component.id).cloned(),
                            error.to_string(),
                        ));
                        continue;
                    }
                }
                match payloads.prepare(request, &mut release_artifact_store) {
                    Ok(payload) => {
                        binding_payloads.insert(component.id.clone(), payload.artifact.clone());
                        let mut prepared = effective_config;
                        prepared.prepared_artifact = Some(payload.artifact.clone());
                        prepared.skip_build = true;
                        prepared.requested_ref = None;
                        prepared
                    }
                    Err(error) => {
                        let mut failure = ComponentDeployResult::failed(
                            &component,
                            base_path,
                            local_versions.get(&component.id).cloned(),
                            remote_versions.get(&component.id).cloned(),
                            error.to_string(),
                        );
                        if let Some(exit_code) = preparation_build_exit_code(&error) {
                            failure = failure.with_build_exit_code(Some(exit_code));
                        }
                        failures.push(failure);
                        continue;
                    }
                }
            } else {
                effective_config
            };

        match prepare_component_deploy(
            &component,
            &effective_config,
            base_path,
            project,
            local_versions.get(&component.id).cloned(),
            remote_versions.get(&component.id).cloned(),
            release_artifacts.get(&component.id).cloned(),
        ) {
            Ok(prepared) => prepared_deployments.push(prepared),
            Err(result) => failures.push(result),
        }
    }

    // Bind payloads to this project's policy before execution preflight. The
    // collection above retains the process-local artifact cleanup guards.
    if !binding_payloads.is_empty() {
        let binding_components = prepared_deployments
            .iter()
            .map(|deployment| deployment.component.clone())
            .collect::<Vec<_>>();
        if let Err(error) =
            bind_project_payloads(project, base_path, &binding_components, &binding_payloads)
        {
            return Err(binding_components
                .iter()
                .map(|component| {
                    ComponentDeployResult::failed(
                        component,
                        base_path,
                        local_versions.get(&component.id).cloned(),
                        remote_versions.get(&component.id).cloned(),
                        error.to_string(),
                    )
                })
                .collect());
        }
    }

    if failures.is_empty() {
        Ok(PreparedDeployments {
            deployments: prepared_deployments,
            _payloads: payloads,
        })
    } else {
        Err(failures)
    }
}

/// Read the build exit code the preparation step recorded on the structured
/// error details. The producer (`preparation::…` build failure) sets
/// `details.exit_code`, so this never depends on the human-readable message.
fn preparation_build_exit_code(error: &Error) -> Option<i32> {
    error
        .details
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors what the preparation build-failure producer records, without
    /// depending on the wording of the failure message.
    fn build_failure(exit_code: Option<i32>) -> Error {
        let mut error = Error::validation_invalid_argument(
            "build",
            "Build failed (exit code 3): extension build reported failure",
            None,
            None,
        );
        if let (Some(code), Some(details)) = (exit_code, error.details.as_object_mut()) {
            details.insert("exit_code".to_string(), serde_json::Value::from(code));
        }
        error
    }

    #[test]
    fn reads_structured_build_exit_code() {
        assert_eq!(
            preparation_build_exit_code(&build_failure(Some(3))),
            Some(3)
        );
    }

    #[test]
    fn returns_none_when_no_structured_exit_code_present() {
        assert_eq!(preparation_build_exit_code(&build_failure(None)), None);
    }
}

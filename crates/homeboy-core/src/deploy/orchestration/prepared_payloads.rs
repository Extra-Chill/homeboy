use std::collections::HashMap;

use crate::component::Component;
use crate::project::Project;

use super::super::execution::{prepare_component_deploy, PreparedComponentDeploy};
use super::super::preparation::{ComponentPayloadPreparationRequest, PreparedPayloadCollection};
use super::super::types::{ComponentDeployResult, DeployConfig};
use crate::git::release_download::{ReleaseArtifactLease, ReleaseArtifactStore};

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

    for component in components {
        let source_path = component.local_path.clone();
        let mut component = crate::project::apply_component_overrides(component, project);
        if config.requested_ref.is_some() {
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
                let request =
                    ComponentPayloadPreparationRequest::new(&component, &preparation_config);
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
                        if let Some(exit_code) = preparation_build_exit_code(&error.to_string()) {
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

    if failures.is_empty() {
        Ok(PreparedDeployments {
            deployments: prepared_deployments,
            _payloads: payloads,
        })
    } else {
        Err(failures)
    }
}

fn preparation_build_exit_code(message: &str) -> Option<i32> {
    message
        .strip_prefix("Invalid argument 'build': Build failed (exit code ")?
        .split_once(')')?
        .0
        .parse()
        .ok()
}

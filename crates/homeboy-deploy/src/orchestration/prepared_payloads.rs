use std::collections::HashMap;

use homeboy_core::component::Component;
use homeboy_core::error::Error;
use homeboy_core::project::Project;

use super::super::binding::bind_project_payloads;
use super::super::execution::{prepare_component_deploy, PreparedComponentDeploy};
use super::super::preparation::{ComponentPayloadPreparationRequest, DeploymentArtifactStore};
use super::super::provenance::record_payload_preparation_build;
use super::super::types::{ComponentDeployResult, DeployConfig};
use homeboy_core::git::release_download::ReleaseArtifactLease;

pub(super) struct PreparedDeployments {
    deployments: Vec<PreparedComponentDeploy>,
    #[cfg(test)]
    _payloads: Option<super::super::preparation::PreparedPayloadCollection>,
}

impl std::ops::Deref for PreparedDeployments {
    type Target = [PreparedComponentDeploy];

    fn deref(&self) -> &Self::Target {
        &self.deployments
    }
}

pub(super) struct PrepareDeploymentsInput<'a> {
    pub(super) components: &'a [Component],
    pub(super) config: &'a DeployConfig,
    pub(super) project: &'a Project,
    pub(super) base_path: &'a str,
    pub(super) local_versions: &'a HashMap<String, String>,
    pub(super) remote_versions: &'a HashMap<String, String>,
    pub(super) release_artifacts: &'a HashMap<String, ReleaseArtifactLease>,
}

pub(super) fn prepare_component_deployments_with_payloads(
    input: PrepareDeploymentsInput<'_>,
    artifacts: &mut DeploymentArtifactStore,
) -> std::result::Result<PreparedDeployments, Vec<ComponentDeployResult>> {
    let PrepareDeploymentsInput {
        components,
        config,
        project,
        base_path,
        local_versions,
        remote_versions,
        release_artifacts,
    } = input;
    let mut prepared_deployments = Vec::new();
    let mut failures = Vec::new();

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
        let effective_config = if is_artifact_deploy
            && !config.skip_build
            && config.prepared_artifact.is_none()
            && !release_artifacts.contains_key(&component.id)
        {
            let mut preparation_config = effective_config.clone();
            // The existing detached checkout is the authoritative exact-ref source.
            preparation_config.requested_ref = None;
            preparation_config.requested_refs.clear();
            let mut request =
                ComponentPayloadPreparationRequest::new(&component, &preparation_config);
            request.config.exact_ref_materialized =
                config.requested_ref_for(&component.id).is_some();
            match artifacts
                .payloads
                .prepare(request, &mut artifacts.release_artifacts)
            {
                Ok(payload) => {
                    binding_payloads.insert(component.id.clone(), payload.artifact.clone());
                    let payload_build_ran = payload.build_ran;
                    let mut prepared = effective_config;
                    prepared.prepared_artifact = Some(payload.artifact.clone());
                    prepared.skip_build = true;
                    prepared.requested_ref = None;
                    (prepared, payload_build_ran)
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
            (effective_config, false)
        };

        match prepare_component_deploy(
            &component,
            &effective_config.0,
            base_path,
            project,
            local_versions.get(&component.id).cloned(),
            remote_versions.get(&component.id).cloned(),
            release_artifacts.get(&component.id).cloned(),
        ) {
            Ok(mut prepared) => {
                if effective_config.1 {
                    record_payload_preparation_build(&mut prepared.build_provenance);
                }
                prepared_deployments.push(prepared);
            }
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
            #[cfg(test)]
            _payloads: None,
        })
    } else {
        Err(failures)
    }
}

#[cfg(test)]
pub(super) fn prepare_component_deployments(
    components: &[Component],
    config: &DeployConfig,
    project: &Project,
    base_path: &str,
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    release_artifacts: &HashMap<String, ReleaseArtifactLease>,
) -> std::result::Result<PreparedDeployments, Vec<ComponentDeployResult>> {
    let mut artifacts = DeploymentArtifactStore::default();
    let mut prepared = prepare_component_deployments_with_payloads(
        PrepareDeploymentsInput {
            components,
            config,
            project,
            base_path,
            local_versions,
            remote_versions,
            release_artifacts,
        },
        &mut artifacts,
    )?;
    prepared._payloads = Some(artifacts.payloads);
    Ok(prepared)
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

    #[test]
    fn two_projects_share_one_prepared_payload() {
        let source = tempfile::tempdir().expect("source");
        let build_count = source.path().join("build-count");
        let mut component = Component::new(
            "fixture".to_string(),
            source.path().display().to_string(),
            "plugins/fixture".to_string(),
            Some("build/fixture.bin".to_string()),
        );
        component.scripts = Some(homeboy_core::component::ComponentScriptsConfig {
            build: vec![format!(
                "mkdir -p build && printf payload > build/fixture.bin && printf build >> {}",
                build_count.display()
            )],
            ..Default::default()
        });
        let config = DeployConfig {
            component_ids: vec![component.id.clone()],
            head: true,
            ..Default::default()
        };
        let mut artifacts = DeploymentArtifactStore::default();
        let versions = HashMap::new();
        let canonical_artifacts = HashMap::new();

        let first = prepare_component_deployments_with_payloads(
            PrepareDeploymentsInput {
                components: std::slice::from_ref(&component),
                config: &config,
                project: &Project {
                    id: "first".to_string(),
                    ..Default::default()
                },
                base_path: "/srv/first",
                local_versions: &versions,
                remote_versions: &versions,
                release_artifacts: &canonical_artifacts,
            },
            &mut artifacts,
        )
        .expect("first target preparation");
        let first_artifact = first[0]
            .config
            .prepared_artifact
            .clone()
            .expect("first prepared artifact");
        drop(first);

        let second = prepare_component_deployments_with_payloads(
            PrepareDeploymentsInput {
                components: &[component],
                config: &config,
                project: &Project {
                    id: "second".to_string(),
                    ..Default::default()
                },
                base_path: "/srv/second",
                local_versions: &versions,
                remote_versions: &versions,
                release_artifacts: &canonical_artifacts,
            },
            &mut artifacts,
        )
        .expect("second target preparation");
        let second_artifact = second[0]
            .config
            .prepared_artifact
            .as_ref()
            .expect("second prepared artifact");

        assert_eq!(
            std::fs::read_to_string(build_count).expect("build count"),
            "build"
        );
        assert_eq!(first_artifact.sha256, second_artifact.sha256);
        assert_eq!(first_artifact.durable_path, second_artifact.durable_path);
    }

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

use crate::deploy::{self, DeployConfig, PreparedDeployArtifact};

use super::executor::release_cleanup_paths;
use super::types::{
    ReleaseArtifact, ReleaseDeploymentResult, ReleaseDeploymentSummary, ReleaseProjectDeployResult,
    ReleaseRun, ReleaseStepResult, ReleaseStepStatus,
};

pub(super) fn plan_deployment(component_id: &str) -> ReleaseDeploymentResult {
    let projects = release_deploy_targets(component_id);

    let project_results: Vec<ReleaseProjectDeployResult> = projects
        .iter()
        .map(|project_id| ReleaseProjectDeployResult {
            project_id: project_id.clone(),
            status: "planned".to_string(),
            error: None,
            component_result: None,
        })
        .collect();

    ReleaseDeploymentResult {
        projects: project_results,
        summary: ReleaseDeploymentSummary {
            total_projects: projects.len() as u32,
            ..Default::default()
        },
    }
}

pub(super) fn run_deployment_step(
    component_id: &str,
    local_path: &str,
    expected_version: Option<&str>,
    artifacts: &[ReleaseArtifact],
) -> ReleaseStepResult {
    let deployment = execute_deployment(component_id, local_path, expected_version, artifacts);
    let deploy_failed = deployment.summary.failed > 0;

    ReleaseStepResult {
        id: "deploy".to_string(),
        step_type: "deploy".to_string(),
        status: if deploy_failed {
            ReleaseStepStatus::Failed
        } else {
            ReleaseStepStatus::Success
        },
        data: Some(serde_json::json!({ "deployment": deployment })),
        error: deploy_failed.then(|| "Deployment failed".to_string()),
        ..Default::default()
    }
}

pub(super) fn extract_deployment_from_run(run: &ReleaseRun) -> Option<ReleaseDeploymentResult> {
    run.result
        .steps
        .iter()
        .find(|step| step.step_type == "deploy")
        .and_then(|step| step.data.as_ref())
        .and_then(|data| data.get("deployment"))
        .and_then(|deployment| serde_json::from_value(deployment.clone()).ok())
}

fn execute_deployment(
    component_id: &str,
    local_path: &str,
    expected_version: Option<&str>,
    artifacts: &[ReleaseArtifact],
) -> ReleaseDeploymentResult {
    let projects = release_deploy_targets(component_id);

    if projects.is_empty() {
        cleanup_release_artifacts(local_path, artifacts);
        return ReleaseDeploymentResult {
            projects: vec![],
            summary: ReleaseDeploymentSummary::default(),
        };
    }

    homeboy_core::log_status!(
        "release",
        "Deploying '{}' to {} project(s)...",
        component_id,
        projects.len()
    );

    let prepared_artifact =
        match prepared_release_artifact(component_id, local_path, expected_version, artifacts) {
            Ok(artifact) => artifact,
            Err(error) => return failed_deployment(&projects, error.to_string()),
        };
    let config = release_deployment_config(component_id, expected_version, prepared_artifact);

    let deployment = match deploy::run_multi(&projects, &[component_id.to_string()], &config) {
        Ok(result) => ReleaseDeploymentResult {
            projects: result
                .projects
                .into_iter()
                .map(|project| ReleaseProjectDeployResult {
                    project_id: project.project_id,
                    status: project.status,
                    error: project.error,
                    component_result: project
                        .results
                        .into_iter()
                        .find(|result| result.id == component_id),
                })
                .collect(),
            summary: ReleaseDeploymentSummary {
                total_projects: result.summary.total_projects,
                succeeded: result.summary.succeeded,
                failed: result.summary.failed,
                skipped: result.summary.skipped,
                planned: result.summary.planned,
            },
        },
        Err(error) => ReleaseDeploymentResult {
            projects: projects
                .iter()
                .map(|project_id| ReleaseProjectDeployResult {
                    project_id: project_id.clone(),
                    status: "failed".to_string(),
                    error: Some(error.to_string()),
                    component_result: None,
                })
                .collect(),
            summary: ReleaseDeploymentSummary {
                total_projects: projects.len() as u32,
                failed: projects.len() as u32,
                ..Default::default()
            },
        },
    };

    if should_cleanup_release_artifacts(&deployment) {
        cleanup_release_artifacts(local_path, artifacts);
    } else {
        homeboy_core::log_status!(
            "release",
            "Retaining release artifacts after a failed deploy so the deployment can be resumed."
        );
    }
    deployment
}

fn should_cleanup_release_artifacts(deployment: &ReleaseDeploymentResult) -> bool {
    deployment.summary.failed == 0
}

fn failed_deployment(projects: &[String], error: String) -> ReleaseDeploymentResult {
    ReleaseDeploymentResult {
        projects: projects
            .iter()
            .map(|project_id| ReleaseProjectDeployResult {
                project_id: project_id.clone(),
                status: "failed".to_string(),
                error: Some(error.clone()),
                component_result: None,
            })
            .collect(),
        summary: ReleaseDeploymentSummary {
            total_projects: projects.len() as u32,
            failed: projects.len() as u32,
            ..Default::default()
        },
    }
}

/// Resolve the component-scoped release tag for a version (e.g.
/// `wp-native-auth-v0.2.0`), matching the tag the release's `git.tag` step
/// created. Falls back to the unscoped `v{version}` form when the component has
/// no release scope or cannot be resolved, preserving prior behavior for
/// single-component repos (#9888).
fn scoped_release_tag(component_id: &str, local_path: &str, version: &str) -> String {
    let unscoped = format!("v{}", version.trim_start_matches('v'));
    match homeboy_core::component::resolve_effective(Some(component_id), Some(local_path), None) {
        Ok(component) => {
            crate::release::component_tag_name(&component, version).unwrap_or(unscoped)
        }
        Err(_) => unscoped,
    }
}

fn prepared_release_artifact(
    component_id: &str,
    local_path: &str,
    expected_version: Option<&str>,
    artifacts: &[ReleaseArtifact],
) -> homeboy_core::error::Result<PreparedDeployArtifact> {
    let version = expected_version.ok_or_else(|| {
        homeboy_core::error::Error::validation_invalid_argument(
            "version",
            "Release deployment requires a released version",
            None,
            None,
        )
    })?;
    let artifact = artifacts
        .iter()
        .find(|artifact| artifact.durable_path.is_some())
        .ok_or_else(|| {
            homeboy_core::error::Error::validation_invalid_argument(
                "release.artifacts",
                "Release deployment requires a durable package artifact",
                None,
                None,
            )
        })?;
    let durable_path = artifact
        .durable_path
        .as_ref()
        .expect("filtered durable path");
    let path = std::path::Path::new(durable_path);
    let metadata = std::fs::metadata(path).map_err(|error| {
        homeboy_core::error::Error::internal_io(
            format!("Failed to read durable release artifact: {}", error),
            Some(durable_path.clone()),
        )
    })?;
    // Use the component-scoped tag the release actually created (e.g.
    // `wp-native-auth-v0.2.0`), not a reconstructed unscoped `v{version}`.
    // Monorepo components namespace their tags; deploying the unscoped form
    // fails to resolve to a source commit (#9888).
    let tag = scoped_release_tag(component_id, local_path, version);
    let source_commit = homeboy_core::engine::command::run_in_optional(
        local_path,
        "git",
        &["rev-parse", &format!("{}^{{commit}}", tag)],
    )
    .filter(|commit| !commit.trim().is_empty())
    .ok_or_else(|| {
        homeboy_core::error::Error::validation_invalid_argument(
            "release.tag",
            format!(
                "Could not resolve released tag '{}' to a source commit",
                tag
            ),
            None,
            None,
        )
    })?;
    Ok(PreparedDeployArtifact {
        component_id: component_id.to_string(),
        path: artifact.path.clone(),
        durable_path: durable_path.clone(),
        size_bytes: metadata.len(),
        sha256: crate::deploy::sha256_file(path)?,
        version: version.to_string(),
        tag,
        source_commit: source_commit.trim().to_string(),
    })
}

fn release_deployment_config(
    component_id: &str,
    expected_version: Option<&str>,
    prepared_artifact: PreparedDeployArtifact,
) -> DeployConfig {
    DeployConfig {
        component_ids: vec![component_id.to_string()],
        all: false,
        outdated: false,
        behind_upstream: false,
        dry_run: false,
        check: false,
        force: true,
        skip_build: true,
        keep_deps: false,
        skip_deps_hydration: false,
        expected_version: expected_version.map(str::to_string),
        no_pull: false,
        allow_stale_source: false,
        allow_downgrade: false,
        head: false,
        requested_ref: None,
        requested_refs: Default::default(),
        resolved_refs: Default::default(),
        preflighted_source_paths: Default::default(),
        preflighted_component_identities: Default::default(),
        tagged: false,
        prepared_artifact: Some(prepared_artifact),
        resume_run_id: None,
    }
}

fn release_deploy_targets(component_id: &str) -> Vec<String> {
    match deploy::resolve_shared_targets(&[component_id.to_string()]) {
        Ok(projects) => projects,
        Err(_) => {
            homeboy_core::log_status!(
                "release",
                "Warning: No projects use component '{}'. Nothing to deploy.",
                component_id
            );
            Vec::new()
        }
    }
}

fn cleanup_release_artifacts(local_path: &str, artifacts: &[ReleaseArtifact]) {
    for path in release_cleanup_paths(local_path, artifacts) {
        if !path.exists() {
            continue;
        }

        if let Err(error) = std::fs::remove_dir_all(&path) {
            homeboy_core::log_status!(
                "release",
                "Warning: failed to clean up {}: {}",
                path.display(),
                error
            );
        } else {
            homeboy_core::log_status!("release", "Cleaned up {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_deployment_from_run, plan_deployment, should_cleanup_release_artifacts};
    use crate::release::types::{
        ReleaseArtifact, ReleaseDeploymentResult, ReleaseDeploymentSummary, ReleaseRun,
        ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus,
    };

    #[test]
    fn test_plan_deployment() {
        let deployment = plan_deployment("definitely-not-used-by-projects");

        assert!(deployment.projects.is_empty());
        assert_eq!(deployment.summary.total_projects, 0);
    }

    #[test]
    fn scoped_release_tag_falls_back_to_unscoped_when_component_unresolvable() {
        // An id/path that resolves to no scoped component yields the plain
        // `v{version}` tag, preserving single-component-repo behavior. The
        // scoped path (e.g. `blocks-engine-v0.2.3`) is produced by
        // release::component_tag_name and covered by the ReleaseScope tag_name
        // tests in scope.rs.
        let temp = tempfile::tempdir().expect("tempdir");
        let tag = super::scoped_release_tag(
            "definitely-no-such-component-9888",
            temp.path().to_str().unwrap(),
            "0.2.0",
        );
        assert_eq!(tag, "v0.2.0");
        // A `v`-prefixed version is normalized, not doubled.
        let tag = super::scoped_release_tag(
            "definitely-no-such-component-9888",
            temp.path().to_str().unwrap(),
            "v0.2.0",
        );
        assert_eq!(tag, "v0.2.0");
    }

    #[test]
    fn test_run_deployment_step() {
        let result =
            super::run_deployment_step("definitely-not-used-by-projects", "/tmp", None, &[]);

        assert_eq!(result.id, "deploy");
        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(result.error.is_none());
        assert!(result.data.is_some());
    }

    #[test]
    fn deployment_step_cleans_release_build_artifacts_without_deploy_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        std::fs::create_dir_all(&build_dir).expect("build dir");
        let artifact_path = build_dir.join("fixture.zip");
        std::fs::write(&artifact_path, "artifact").expect("artifact");
        let artifacts = vec![ReleaseArtifact {
            path: artifact_path.display().to_string(),
            durable_path: None,
            artifact_type: None,
            platform: None,
        }];

        let result = super::run_deployment_step(
            "definitely-not-used-by-projects",
            &temp.path().to_string_lossy(),
            None,
            &artifacts,
        );

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(!build_dir.exists());
    }

    #[test]
    fn test_extract_deployment_from_run() {
        let deployment = plan_deployment("definitely-not-used-by-projects");
        let run = ReleaseRun {
            component_id: "fixture".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: vec![ReleaseStepResult {
                    id: "deploy".to_string(),
                    step_type: "deploy".to_string(),
                    status: ReleaseStepStatus::Success,
                    data: Some(serde_json::json!({ "deployment": deployment })),
                    ..Default::default()
                }],
                status: ReleaseStepStatus::Success,
                warnings: vec![],
                summary: None,
                phase_timings: None,
                rollback: None,
            },
        };

        let extracted = extract_deployment_from_run(&run).expect("deployment result");
        assert_eq!(extracted.summary.total_projects, 0);
    }

    #[test]
    fn release_deploy_config_reuses_prepared_release_package() {
        let artifact = crate::deploy::PreparedDeployArtifact {
            component_id: "demo".to_string(),
            path: "/source/demo.zip".to_string(),
            durable_path: "/durable/demo.zip".to_string(),
            size_bytes: 7,
            sha256: "hash".to_string(),
            version: "1.2.3".to_string(),
            tag: "v1.2.3".to_string(),
            source_commit: "commit".to_string(),
        };
        let config = super::release_deployment_config("demo", Some("1.2.3"), artifact.clone());

        assert_eq!(config.component_ids, vec!["demo".to_string()]);
        assert_eq!(config.expected_version, Some("1.2.3".to_string()));
        assert!(!config.tagged, "--tagged is an operator rebuild mode");
        assert!(config.skip_build, "release deploy must not package again");
        assert_eq!(config.prepared_artifact, Some(artifact));
        assert!(
            !config.head,
            "release deploy must not deploy the registered worktree HEAD"
        );
        assert!(
            !config.no_pull,
            "release deploy must fetch/pull before checking out the released tag"
        );
    }

    #[test]
    fn failed_release_deployment_retains_artifact_for_resume() {
        let deployment = ReleaseDeploymentResult {
            projects: vec![],
            summary: ReleaseDeploymentSummary {
                total_projects: 2,
                failed: 1,
                ..ReleaseDeploymentSummary::default()
            },
        };

        assert!(!should_cleanup_release_artifacts(&deployment));
    }
}

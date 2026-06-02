use crate::core::deploy::{self, DeployConfig};

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
        missing: Vec::new(),
        warnings: Vec::new(),
        hints: Vec::new(),
        data: Some(serde_json::json!({ "deployment": deployment })),
        error: deploy_failed.then(|| "Deployment failed".to_string()),
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

    log_status!(
        "release",
        "Deploying '{}' to {} project(s)...",
        component_id,
        projects.len()
    );

    let config = release_deployment_config(component_id, expected_version);

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

    cleanup_release_artifacts(local_path, artifacts);
    deployment
}

fn release_deployment_config(component_id: &str, expected_version: Option<&str>) -> DeployConfig {
    DeployConfig {
        component_ids: vec![component_id.to_string()],
        all: false,
        outdated: false,
        behind_upstream: false,
        dry_run: false,
        check: false,
        force: true,
        skip_build: false,
        keep_deps: false,
        expected_version: expected_version.map(str::to_string),
        no_pull: false,
        head: false,
        tagged: true,
    }
}

fn release_deploy_targets(component_id: &str) -> Vec<String> {
    match deploy::resolve_shared_targets(&[component_id.to_string()]) {
        Ok(projects) => projects,
        Err(_) => {
            log_status!(
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
            log_status!(
                "release",
                "Warning: failed to clean up {}: {}",
                path.display(),
                error
            );
        } else {
            log_status!("release", "Cleaned up {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_deployment_from_run, plan_deployment};
    use crate::core::release::types::{
        ReleaseArtifact, ReleaseRun, ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus,
    };

    #[test]
    fn test_plan_deployment() {
        let deployment = plan_deployment("definitely-not-used-by-projects");

        assert!(deployment.projects.is_empty());
        assert_eq!(deployment.summary.total_projects, 0);
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
                    missing: vec![],
                    warnings: vec![],
                    hints: vec![],
                    data: Some(serde_json::json!({ "deployment": deployment })),
                    error: None,
                }],
                status: ReleaseStepStatus::Success,
                warnings: vec![],
                summary: None,
            },
        };

        let extracted = extract_deployment_from_run(&run).expect("deployment result");
        assert_eq!(extracted.summary.total_projects, 0);
    }

    #[test]
    fn release_deploy_config_uses_tagged_release_version() {
        let config = super::release_deployment_config("demo", Some("1.2.3"));

        assert_eq!(config.component_ids, vec!["demo".to_string()]);
        assert_eq!(config.expected_version, Some("1.2.3".to_string()));
        assert!(
            config.tagged,
            "release deploy must use tagged deploy semantics"
        );
        assert!(
            !config.head,
            "release deploy must not deploy the registered worktree HEAD"
        );
        assert!(
            !config.no_pull,
            "release deploy must fetch/pull before checking out the released tag"
        );
    }
}

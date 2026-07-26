use crate::deploy::{self, DeployConfig, PreparedDeployArtifact, PreparedDeployProjection};
use homeboy_core::error::{Error, Result};
use std::collections::BTreeMap;
use std::fs;

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
    component: &homeboy_core::component::Component,
    expected_version: Option<&str>,
    released_tag: Option<&str>,
    artifacts: &[ReleaseArtifact],
    package_owned_paths: &[String],
) -> ReleaseStepResult {
    let deployment = execute_deployment(
        component,
        expected_version,
        released_tag,
        artifacts,
        package_owned_paths,
    );
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
    component: &homeboy_core::component::Component,
    expected_version: Option<&str>,
    released_tag: Option<&str>,
    artifacts: &[ReleaseArtifact],
    package_owned_paths: &[String],
) -> ReleaseDeploymentResult {
    let component_id = &component.id;
    let local_path = &component.local_path;
    let projects = release_deploy_targets(component_id);

    if projects.is_empty() {
        cleanup_release_artifacts(local_path, package_owned_paths);
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
        match prepared_release_artifact(component, expected_version, released_tag, artifacts) {
            Ok(artifact) => artifact,
            Err(error) => return failed_deployment(&projects, error.to_string()),
        };
    let config = match release_deployment_config(
        component,
        expected_version,
        prepared_artifact,
        &projects,
    ) {
        Ok(config) => config,
        Err(error) => return failed_deployment(&projects, error.to_string()),
    };

    let deployment = match deploy::run_multi(&projects, &[component_id.to_string()], &config) {
        Ok(result) => {
            if result.summary.failed > 0 {
                if let Some(run_id) = result.deploy_run_id.as_deref() {
                    if let Err(error) = save_recovery(
                        component,
                        expected_version,
                        &projects,
                        &config,
                        run_id,
                        package_owned_paths,
                    ) {
                        return failed_deployment(
                            &projects,
                            format!(
                                "Deployment failed and its durable recovery checkpoint could not be saved: {error}"
                            ),
                        );
                    }
                }
            } else {
                if let Err(error) = remove_recovery(&component.id) {
                    return failed_deployment(
                        &projects,
                        format!("Deployment succeeded but its recovery checkpoint could not be cleared: {error}"),
                    );
                }
            }
            ReleaseDeploymentResult {
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
                            .find(|result| result.id == *component_id),
                    })
                    .collect(),
                summary: ReleaseDeploymentSummary {
                    total_projects: result.summary.total_projects,
                    succeeded: result.summary.succeeded,
                    failed: result.summary.failed,
                    skipped: result.summary.skipped,
                    planned: result.summary.planned,
                },
            }
        }
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
        cleanup_release_artifacts(local_path, package_owned_paths);
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
    component: &homeboy_core::component::Component,
    expected_version: Option<&str>,
    released_tag: Option<&str>,
    artifacts: &[ReleaseArtifact],
) -> homeboy_core::error::Result<PreparedDeployArtifact> {
    let component_id = component.id.as_str();
    let local_path = component.local_path.as_str();
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
    // Prefer the tag the release actually created and recorded in state. Any
    // independently recomputed name can disagree with it — for monorepo
    // components a failed re-resolution silently degrades to the unscoped
    // `v{version}`, which never existed and cannot resolve to a commit
    // (#10099). Only fall back to deriving a name when no release tag was
    // recorded, such as a deploy that did not follow a release in-process.
    let tag = released_tag
        .map(str::to_string)
        .filter(|tag| !tag.trim().is_empty())
        .unwrap_or_else(|| scoped_release_tag(component_id, local_path, version));
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
    component: &homeboy_core::component::Component,
    expected_version: Option<&str>,
    prepared_artifact: PreparedDeployArtifact,
    projects: &[String],
) -> Result<DeployConfig> {
    let component_id = &component.id;
    let mut requested_refs = std::collections::BTreeMap::new();
    requested_refs.insert(component_id.to_string(), prepared_artifact.tag.clone());
    let mut resolved_refs = std::collections::BTreeMap::new();
    resolved_refs.insert(
        component_id.to_string(),
        prepared_artifact.source_commit.clone(),
    );
    let identity = component
        .canonical_attachment_identity()
        .expect("release component identity is serializable");
    let mut preflighted_source_paths = std::collections::BTreeMap::new();
    preflighted_source_paths.insert(component_id.to_string(), component.local_path.clone());
    let mut preflighted_component_identities = std::collections::BTreeMap::new();
    preflighted_component_identities.insert(component_id.to_string(), identity);

    let mut projection = PreparedDeployProjection {
        components: BTreeMap::from([(component_id.to_string(), component.clone())]),
    };
    for project_id in projects {
        let project = homeboy_core::project::load(project_id)?;
        let attachment = project
            .components
            .iter()
            .find(|entry| entry.id == *component_id)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "components",
                    format!(
                        "Project '{}' has no attached component '{}'",
                        project.id, component_id
                    ),
                    Some(project.id.clone()),
                    None,
                )
            })?;
        let mut target = component.clone();
        if let Some(remote_path) = attachment
            .remote_path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
        {
            target.remote_path = remote_path.to_string();
        }
        target = homeboy_core::project::apply_component_overrides(&target, &project);
        projection
            .components
            .insert(format!("{}:{component_id}", project.id), target);
    }
    Ok(DeployConfig {
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
        requested_refs,
        resolved_refs,
        preflighted_source_paths,
        preflighted_component_identities,
        prepared_projection: Some(projection),
        tagged: false,
        prepared_artifact: Some(prepared_artifact),
        resume_run_id: None,
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DeploymentRecovery {
    component_id: String,
    expected_version: Option<String>,
    projects: Vec<String>,
    artifact: PreparedDeployArtifact,
    projection: PreparedDeployProjection,
    deploy_run_id: String,
    #[serde(default)]
    component_path: String,
    #[serde(default)]
    package_owned_paths: Vec<String>,
}

fn recovery_path(component_id: &str) -> Result<std::path::PathBuf> {
    Ok(homeboy_core::paths::homeboy_data()?
        .join("release-deploy-runs")
        .join(format!("{}.json", component_id.replace('/', "_"))))
}

fn save_recovery(
    component: &homeboy_core::component::Component,
    expected_version: Option<&str>,
    projects: &[String],
    config: &DeployConfig,
    deploy_run_id: &str,
    package_owned_paths: &[String],
) -> Result<()> {
    let record = DeploymentRecovery {
        component_id: component.id.clone(),
        expected_version: expected_version.map(str::to_string),
        projects: projects.to_vec(),
        artifact: config
            .prepared_artifact
            .clone()
            .expect("release deploy has prepared artifact"),
        projection: config
            .prepared_projection
            .clone()
            .expect("release deploy has projection"),
        deploy_run_id: deploy_run_id.to_string(),
        component_path: component.local_path.clone(),
        package_owned_paths: package_owned_paths.to_vec(),
    };
    let path = recovery_path(&component.id)?;
    fs::create_dir_all(path.parent().expect("recovery path parent"))
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec(&record)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )
    .map_err(|error| {
        Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
    })?;
    fs::rename(&temporary, &path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

fn remove_recovery(component_id: &str) -> Result<()> {
    let path = recovery_path(component_id)?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| Error::internal_io(error.to_string(), None))?;
    }
    Ok(())
}

pub(super) fn resume_deployment(component_id: &str) -> Result<Option<ReleaseDeploymentResult>> {
    let path = recovery_path(component_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let record: DeploymentRecovery = serde_json::from_slice(&fs::read(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?)
    .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))?;
    if record.component_id != component_id {
        return Err(Error::validation_invalid_argument(
            "recover",
            "Release deployment recovery component identity does not match",
            None,
            None,
        ));
    }
    let config = release_deployment_config_from_record(&record);
    let result = deploy::run_multi(&record.projects, &[component_id.to_string()], &config)?;
    let deployment = ReleaseDeploymentResult {
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
                    .find(|entry| entry.id == component_id),
            })
            .collect(),
        summary: ReleaseDeploymentSummary {
            total_projects: result.summary.total_projects,
            succeeded: result.summary.succeeded,
            failed: result.summary.failed,
            skipped: result.summary.skipped,
            planned: result.summary.planned,
        },
    };
    if deployment.summary.failed == 0 {
        if !record.component_path.is_empty() {
            cleanup_release_artifacts(&record.component_path, &record.package_owned_paths);
        }
        remove_recovery(component_id)?;
    }
    Ok(Some(deployment))
}

fn release_deployment_config_from_record(record: &DeploymentRecovery) -> DeployConfig {
    let mut config = release_deployment_config(
        &record.projection.components[&record.component_id],
        record.expected_version.as_deref(),
        record.artifact.clone(),
        &[],
    )
    .expect("stored release deployment config is valid");
    config.prepared_projection = Some(record.projection.clone());
    config.resume_run_id = Some(record.deploy_run_id.clone());
    config
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

fn cleanup_release_artifacts(local_path: &str, package_owned_paths: &[String]) {
    for path in release_cleanup_paths(local_path, package_owned_paths) {
        if !path.exists() {
            continue;
        }

        let result = std::fs::symlink_metadata(&path).and_then(|metadata| {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            }
        });
        if let Err(error) = result {
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
    use super::{
        extract_deployment_from_run, plan_deployment, prepared_release_artifact,
        run_deployment_step, should_cleanup_release_artifacts,
    };
    use crate::release::types::{
        ReleaseArtifact, ReleaseCommandInput, ReleaseDeploymentResult, ReleaseDeploymentSummary,
        ReleasePipelineOptions, ReleaseRun, ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus,
    };
    use crate::release::workflow::run_command;
    use homeboy_core::component::{Component, VersionTarget};
    use homeboy_core::project::{self, Project, ProjectComponentAttachment};
    use homeboy_core::server::{self, Server};
    use homeboy_core::test_support::with_isolated_home;
    use std::io::Write;
    use std::path::Path;

    fn run_git(path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn write_release_artifact(path: &Path) {
        let file = std::fs::File::create(path).expect("create release artifact");
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("plugin.txt", zip::write::FileOptions::default())
            .expect("start archive entry");
        archive
            .write_all(b"published release\n")
            .expect("archive bytes");
        archive
            .start_file("VERSION", zip::write::FileOptions::default())
            .expect("start version entry");
        archive.write_all(b"1.2.4\n").expect("version bytes");
        archive.finish().expect("finish release artifact");
    }

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

    /// A monorepo component's release tag is namespaced (`<id>-v<version>`).
    /// The deploy step must use the tag the release actually recorded rather
    /// than recomputing a name, because an independently derived name can
    /// silently degrade to the unscoped `v{version}` that never existed
    /// (#10099).
    #[test]
    fn prepared_artifact_uses_the_recorded_release_tag() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init", "-q", "-b", "main"]);
        run_git(root, &["config", "user.email", "test@example.invalid"]);
        run_git(root, &["config", "user.name", "Test"]);

        let component_dir = root.join("plugins").join("scoped-component");
        std::fs::create_dir_all(&component_dir).expect("component dir");
        std::fs::write(component_dir.join("file.txt"), "content").expect("write");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "initial"]);

        let scoped_tag = "scoped-component-v0.3.0";
        run_git(root, &["tag", scoped_tag]);
        let head = run_git(root, &["rev-parse", "HEAD"]);

        let artifact_path = root.join("artifact.zip");
        std::fs::write(&artifact_path, b"zip").expect("write artifact");
        let artifacts = vec![ReleaseArtifact {
            path: artifact_path.display().to_string(),
            durable_path: Some(artifact_path.display().to_string()),
            artifact_type: None,
            platform: None,
        }];

        let component = Component {
            id: "scoped-component".to_string(),
            local_path: component_dir.display().to_string(),
            ..Default::default()
        };

        let prepared =
            prepared_release_artifact(&component, Some("0.3.0"), Some(scoped_tag), &artifacts)
                .expect("prepared artifact");

        assert_eq!(prepared.tag, scoped_tag);
        assert_eq!(prepared.source_commit, head);
    }

    /// An empty recorded tag must not be treated as authoritative; the derived
    /// name is used instead so the step still has something to resolve.
    #[test]
    fn prepared_artifact_ignores_a_blank_recorded_tag() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init", "-q", "-b", "main"]);
        run_git(root, &["config", "user.email", "test@example.invalid"]);
        run_git(root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("file.txt"), "content").expect("write");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "initial"]);
        run_git(root, &["tag", "v0.3.0"]);
        let head = run_git(root, &["rev-parse", "HEAD"]);

        let artifact_path = root.join("artifact.zip");
        std::fs::write(&artifact_path, b"zip").expect("write artifact");
        let artifacts = vec![ReleaseArtifact {
            path: artifact_path.display().to_string(),
            durable_path: Some(artifact_path.display().to_string()),
            artifact_type: None,
            platform: None,
        }];

        let component = Component {
            id: "definitely-no-such-component-10099".to_string(),
            local_path: root.display().to_string(),
            ..Default::default()
        };

        let prepared = prepared_release_artifact(&component, Some("0.3.0"), Some("  "), &artifacts)
            .expect("prepared artifact");

        assert_eq!(prepared.tag, "v0.3.0");
        assert_eq!(prepared.source_commit, head);
    }

    #[test]
    fn test_run_deployment_step() {
        let result = super::run_deployment_step(
            &homeboy_core::component::Component {
                id: "definitely-not-used-by-projects".to_string(),
                local_path: "/tmp".to_string(),
                ..Default::default()
            },
            None,
            None,
            &[],
            &[],
        );

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
            &homeboy_core::component::Component {
                id: "definitely-not-used-by-projects".to_string(),
                local_path: temp.path().to_string_lossy().to_string(),
                ..Default::default()
            },
            None,
            None,
            &artifacts,
            &["build".to_string()],
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
        let config = super::release_deployment_config(
            &homeboy_core::component::Component {
                id: "demo".to_string(),
                local_path: "/tmp".to_string(),
                ..Default::default()
            },
            Some("1.2.3"),
            artifact.clone(),
            &[],
        )
        .expect("release deploy config");

        assert_eq!(config.component_ids, vec!["demo".to_string()]);
        assert_eq!(config.expected_version, Some("1.2.3".to_string()));
        assert!(!config.tagged, "--tagged is an operator rebuild mode");
        assert!(config.skip_build, "release deploy must not package again");
        assert_eq!(config.prepared_artifact, Some(artifact));
        assert_eq!(config.requested_ref_for("demo"), Some("v1.2.3"));
        assert_eq!(config.resolved_ref_for("demo"), Some("commit"));
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

    #[test]
    fn release_recover_resumes_only_failed_project_with_published_source_projection() {
        with_isolated_home(|home| {
            let source = home.path().join("source");
            let successful_target = home.path().join("successful-target");
            let retry_target = home.path().join("retry-target");
            std::fs::create_dir_all(&source).expect("source directory");
            std::fs::create_dir_all(&successful_target).expect("successful target");
            std::fs::create_dir_all(&retry_target).expect("retry target");
            run_git(&source, &["init", "-q", "--initial-branch", "main"]);
            run_git(&source, &["config", "user.email", "test@example.com"]);
            run_git(&source, &["config", "user.name", "Homeboy Test"]);
            std::fs::write(source.join("VERSION"), "1.2.3\n").expect("source version");
            run_git(&source, &["add", "."]);
            run_git(&source, &["commit", "-q", "-m", "chore: initial"]);
            std::fs::write(source.join("VERSION"), "1.2.4\n").expect("released version");
            run_git(&source, &["commit", "-am", "release: v1.2.4", "-q"]);
            run_git(&source, &["tag", "v1.2.4"]);
            let release_commit = run_git(&source, &["rev-parse", "v1.2.4^{commit}"]);

            let artifact_path = home.path().join("fixture.zip");
            write_release_artifact(&artifact_path);
            let artifact = ReleaseArtifact {
                path: artifact_path.display().to_string(),
                durable_path: Some(artifact_path.display().to_string()),
                artifact_type: None,
                platform: None,
            };
            std::fs::create_dir_all(source.join("build")).expect("package build directory");
            std::fs::write(source.join("build/intermediate"), "build").expect("build output");
            std::fs::write(source.join("fixture-1.2.4.tgz"), "package").expect("package output");
            let package_owned_paths = vec!["build".to_string(), "fixture-1.2.4.tgz".to_string()];
            let component = Component {
                id: "fixture".to_string(),
                local_path: source.display().to_string(),
                remote_path: "plugins/fixture".to_string(),
                build_artifact: Some("fixture.zip".to_string()),
                extract_command: Some("unzip -o {{artifact}} && rm {{artifact}}".to_string()),
                version_targets: Some(vec![VersionTarget {
                    file: "VERSION".to_string(),
                    pattern: Some("^([0-9]+\\.[0-9]+\\.[0-9]+)$".to_string()),
                    artifact_path: None,
                }]),
                ..Default::default()
            };

            server::save(&Server {
                id: "local".to_string(),
                host: "localhost".to_string(),
                user: "test".to_string(),
                port: 22,
                identity_file: None,
                aliases: vec![],
                kind: None,
                auth: None,
                env: Default::default(),
                runner: None,
            })
            .expect("save local server");
            for (id, base_path) in [
                ("successful", successful_target.display().to_string()),
                ("retry", "/dev/null".to_string()),
            ] {
                project::save(&Project {
                    id: id.to_string(),
                    server_id: Some("local".to_string()),
                    base_path: Some(base_path),
                    // The configured attachment is intentionally stale. Release must use
                    // its accepted source projection rather than resolving this path.
                    components: vec![ProjectComponentAttachment {
                        id: component.id.clone(),
                        local_path: home
                            .path()
                            .join(format!("missing-{id}-source"))
                            .display()
                            .to_string(),
                        remote_path: Some(format!("plugins/{id}")),
                    }],
                    ..Project::default()
                })
                .expect("save project");
            }

            let first = run_deployment_step(
                &component,
                Some("1.2.4"),
                None,
                &[artifact],
                &package_owned_paths,
            );
            assert_eq!(first.status, ReleaseStepStatus::Failed);
            let first_deployment = first
                .data
                .as_ref()
                .and_then(|data| data.get("deployment"))
                .cloned()
                .and_then(|data| serde_json::from_value::<ReleaseDeploymentResult>(data).ok())
                .expect("initial deployment result");
            assert_eq!(
                first_deployment.summary.succeeded,
                1,
                "initial deployment: {}",
                serde_json::to_string(&first_deployment).expect("serialize deployment")
            );
            assert_eq!(first_deployment.summary.failed, 1);
            assert!(successful_target
                .join("plugins/successful/plugin.txt")
                .exists());
            assert!(super::recovery_path("fixture")
                .expect("recovery path")
                .exists());
            assert!(source.join("build/intermediate").is_file());
            assert!(source.join("fixture-1.2.4.tgz").is_file());

            // A process restart reloads the checkpoint, not an in-memory release plan.
            // The completed target is now invalid: success proves the resumed lifecycle skips it.
            let mut successful = project::load("successful").expect("successful project");
            successful.base_path = Some("/dev/null".to_string());
            project::save(&successful).expect("make completed target invalid");
            let mut retry = project::load("retry").expect("retry project");
            retry.base_path = Some(retry_target.display().to_string());
            project::save(&retry).expect("repair failed target");

            let (recovered, exit_code) = run_command(ReleaseCommandInput {
                component_id: component.id.clone(),
                recover: true,
                pipeline: ReleasePipelineOptions {
                    deploy: true,
                    ..Default::default()
                },
                ..Default::default()
            })
            .expect("recover deployment");
            assert_eq!(exit_code, 0);
            assert_eq!(recovered.status, "released");
            assert!(
                recovered.run.is_none(),
                "recovery must not replay publication"
            );
            assert_eq!(
                recovered
                    .deployment
                    .as_ref()
                    .expect("recovered deployment")
                    .summary
                    .skipped,
                1,
                "completed target is skipped"
            );
            let retried = recovered
                .deployment
                .as_ref()
                .expect("recovered deployment")
                .projects
                .iter()
                .find(|project| project.project_id == "retry")
                .and_then(|project| project.component_result.as_ref())
                .expect("retried component result");
            assert_eq!(retried.requested_ref.as_deref(), Some("v1.2.4"));
            assert_eq!(
                retried.resolved_sha.as_deref(),
                Some(release_commit.as_str())
            );
            assert_eq!(
                retried.source.as_deref(),
                Some(
                    source
                        .canonicalize()
                        .expect("canonical source")
                        .to_string_lossy()
                        .as_ref()
                )
            );
            assert!(retry_target.join("plugins/retry/plugin.txt").exists());
            assert!(!source.join("build").exists());
            assert!(!source.join("fixture-1.2.4.tgz").exists());
            assert_eq!(run_git(&source, &["tag", "--list"]), "v1.2.4");
            assert_eq!(
                run_git(&source, &["rev-parse", "v1.2.4^{commit}"]),
                release_commit
            );
            assert!(
                !super::recovery_path("fixture")
                    .expect("recovery path")
                    .exists(),
                "terminal success clears the durable checkpoint"
            );

            assert!(super::resume_deployment(&component.id)
                .expect("terminal recovery lookup")
                .is_none());
            assert_eq!(run_git(&source, &["tag", "--list"]), "v1.2.4");
        });
    }
}

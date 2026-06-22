use std::path::{Path, PathBuf};

use crate::core::build;
use crate::core::component::Component;
use crate::core::context::RemoteProjectContext;
use crate::core::project::Project;

use super::super::generated_artifacts::GeneratedBuildArtifactCleanupGuard;
use super::super::path_roots::{component_remote_path, resolve_effective_remote_path};
use super::super::policy::{owner_hint_for_path, protected_path_suffixes, validate_deploy_target};
use super::super::types::{ComponentDeployResult, DeployArtifactSource, DeployConfig};
use super::super::version_overrides::is_self_deploy;
use super::preflight::{resolve_preflight_artifact_path, validate_preflight_file_artifact};
use super::release_plan::{
    release_artifact_plan, try_download_release_artifact, ReleaseArtifactPlan,
};
use super::strategies::{execute_artifact_deploy, execute_file_deploy, execute_git_deploy};

pub(crate) struct PreparedComponentDeploy {
    pub component: Component,
    pub config: DeployConfig,
    pub install_dir: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub build_exit_code: Option<i32>,
    pub artifact_path: Option<PathBuf>,
    pub artifact_source: Option<DeployArtifactSource>,
    pub cleanup_local_artifact: bool,
}

#[allow(clippy::result_large_err)]
pub(crate) fn prepare_component_deploy(
    component: &Component,
    config: &DeployConfig,
    base_path: &str,
    project: &Project,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> std::result::Result<PreparedComponentDeploy, ComponentDeployResult> {
    let is_git_deploy = component.deploy_strategy.as_deref() == Some("git");
    let is_file_deploy = component.deploy_strategy.as_deref() == Some("file");

    // Try downloading release artifact from GitHub instead of building locally.
    // This is the preferred path when the component has remote_url set.
    let release_artifact: Option<PathBuf> =
        match release_artifact_plan(component, config, is_git_deploy, is_file_deploy) {
            ReleaseArtifactPlan::Reuse { tag, .. } => {
                match try_download_release_artifact(component, &tag) {
                    Ok(path) => path,
                    Err(error) => {
                        return Err(failed_component_deploy_result(
                            component,
                            base_path,
                            local_version,
                            remote_version,
                            None,
                            error,
                        )
                        .with_artifact_source(DeployArtifactSource::ReleaseAsset));
                    }
                }
            }
            ReleaseArtifactPlan::LocalBuild { reason } => {
                if config.dry_run {
                    log_status!(
                        "deploy",
                        "Local rebuild planned for '{}': {}",
                        component.id,
                        reason
                    );
                }
                None
            }
        };
    let artifact_source = if is_git_deploy || is_file_deploy {
        None
    } else if release_artifact.is_some() {
        Some(DeployArtifactSource::ReleaseAsset)
    } else {
        Some(DeployArtifactSource::LocalBuild)
    };

    let cleanup_generated_artifacts = !is_git_deploy
        && !is_file_deploy
        && !config.skip_build
        && release_artifact.is_none()
        && !is_self_deploy(component);
    let local_path = Path::new(&component.local_path);
    let mut generated_cleanup_guard =
        GeneratedBuildArtifactCleanupGuard::new(local_path, cleanup_generated_artifacts);

    // Build (git-deploy, file-deploy, skip-build, and release-download skip this step)
    let (build_exit_code, build_error) =
        if is_git_deploy || is_file_deploy || config.skip_build || release_artifact.is_some() {
            (Some(0), None)
        } else {
            build::build_component(component)
        };

    if let Some(ref error) = build_error {
        return Err(ComponentDeployResult::failed(
            component,
            base_path,
            local_version,
            remote_version,
            error.clone(),
        )
        .with_build_exit_code(build_exit_code));
    }

    // Auto-resolve remote_path from linked extension deploy policy when not explicitly set.
    // This is a deploy-time safety net; the primary resolution happens in
    // resolve_project_component (#812).
    let effective_remote_path = component_remote_path(component);
    if component.remote_path.trim().is_empty() && !effective_remote_path.trim().is_empty() {
        log_status!(
            "deploy",
            "Auto-resolved remote path: {}",
            effective_remote_path
        );
    }

    if component.remote_owner.is_none() {
        if let Some(suggested_owner) = owner_hint_for_path(component, &effective_remote_path) {
            log_status!(
                "deploy",
                "⚠ Component '{}' deploys to a path that may need remote_owner='{}'. \
             Files may deploy with the SSH user's ownership. \
             Fix: homeboy component set {} --json '{{\"remote_owner\":\"{}\"}}'",
                component.id,
                suggested_owner,
                component.id,
                suggested_owner
            );
        }
    }

    // Resolve and validate install directory before any destructive operation.
    let install_dir =
        match resolve_effective_remote_path(project, component, base_path).and_then(|install_dir| {
            validate_deploy_target(
                &install_dir,
                base_path,
                &component.id,
                &protected_path_suffixes(component),
            )?;
            Ok(install_dir)
        }) {
            Ok(install_dir) => install_dir,
            Err(err) => {
                return Err(failed_component_deploy_result(
                    component,
                    base_path,
                    local_version,
                    remote_version,
                    build_exit_code,
                    err.to_string(),
                ));
            }
        };

    let artifact_path = if is_git_deploy {
        None
    } else if is_file_deploy {
        validate_preflight_file_artifact(
            component,
            base_path,
            build_exit_code,
            local_version.clone(),
            remote_version.clone(),
        )?;
        None
    } else {
        match resolve_preflight_artifact_path(
            component,
            config,
            base_path,
            &install_dir,
            local_version.clone(),
            remote_version.clone(),
            build_exit_code,
            release_artifact.as_ref(),
        ) {
            Ok(path) => Some(path),
            Err(result) => return Err(result),
        }
    };

    generated_cleanup_guard.disarm();

    let cleanup_local_artifact = artifact_path.as_ref().is_some_and(|_| {
        release_artifact.is_none() && !config.skip_build && !is_self_deploy(component)
    });

    Ok(PreparedComponentDeploy {
        component: component.clone(),
        config: config.clone(),
        install_dir,
        local_version,
        remote_version,
        build_exit_code,
        artifact_path,
        artifact_source,
        cleanup_local_artifact,
    })
}

pub(crate) fn execute_preflighted_component_deploy(
    prepared: &PreparedComponentDeploy,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
) -> ComponentDeployResult {
    let component = &prepared.component;

    // Dispatch by deploy strategy
    let strategy = component.deploy_strategy.as_deref().unwrap_or("rsync");

    if strategy == "git" {
        return execute_git_deploy(
            component,
            &prepared.config,
            ctx,
            base_path,
            &prepared.install_dir,
            prepared.local_version.clone(),
            prepared.remote_version.clone(),
        );
    }

    if strategy == "file" {
        return execute_file_deploy(
            component,
            ctx,
            base_path,
            &prepared.install_dir,
            prepared.local_version.clone(),
            prepared.remote_version.clone(),
        );
    }

    execute_artifact_deploy(prepared, ctx, base_path, project)
}

pub(super) fn failed_component_deploy_result(
    component: &Component,
    base_path: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
    build_exit_code: Option<i32>,
    error: String,
) -> ComponentDeployResult {
    ComponentDeployResult::failed(component, base_path, local_version, remote_version, error)
        .with_build_exit_code(build_exit_code)
}

use std::path::PathBuf;

use crate::core::component::Component;
use crate::core::git;

use super::super::orchestration_tag_checkout::deploy_tag_for_version;
use super::super::release_download;
use super::super::types::DeployConfig;

pub(crate) enum ReleaseArtifactPlan {
    Reuse { url: String, tag: String },
    LocalBuild { reason: String },
}

pub(crate) fn release_artifact_plan(
    component: &Component,
    config: &DeployConfig,
    is_git_deploy: bool,
    is_file_deploy: bool,
) -> ReleaseArtifactPlan {
    if is_git_deploy {
        return local_release_artifact_plan("component uses deploy_strategy 'git'");
    }
    if is_file_deploy {
        return local_release_artifact_plan("component uses deploy_strategy 'file'");
    }
    if config.head {
        return local_release_artifact_plan("--head deploys the current checkout");
    }
    if config.tagged {
        return local_release_artifact_plan("--tagged forces a local tag build");
    }
    if config.skip_build {
        return local_release_artifact_plan("build is skipped by caller");
    }
    let Some(remote_url) = component.remote_url.as_ref() else {
        return local_release_artifact_plan("component has no remote_url for release asset lookup");
    };
    let Some(github) = release_download::parse_github_url(remote_url) else {
        return local_release_artifact_plan("component remote_url is not a GitHub repository URL");
    };
    let Some(artifact_name) = release_download::resolve_artifact_name(component) else {
        return local_release_artifact_plan(
            "component has no build_artifact filename for release asset lookup",
        );
    };
    let Some(tag) = deploy_release_tag(component, config) else {
        return local_release_artifact_plan("no version tag found for release asset lookup");
    };

    ReleaseArtifactPlan::Reuse {
        url: github.release_artifact_url(&tag, &artifact_name),
        tag,
    }
}

fn local_release_artifact_plan(reason: impl Into<String>) -> ReleaseArtifactPlan {
    ReleaseArtifactPlan::LocalBuild {
        reason: reason.into(),
    }
}

#[cfg(test)]
pub(super) fn should_try_download_release_artifact(
    component: &Component,
    config: &DeployConfig,
    is_git_deploy: bool,
    is_file_deploy: bool,
) -> bool {
    matches!(
        release_artifact_plan(component, config, is_git_deploy, is_file_deploy),
        ReleaseArtifactPlan::Reuse { .. }
    )
}

/// Try to download a release artifact from GitHub for the selected deploy tag.
///
/// Returns `Ok(Some(path))` if successful and `Ok(None)` for normal download misses
/// that should fall back to local build. Validation failures are returned as deploy
/// errors so invalid artifacts never reach remote install.
pub(super) fn try_download_release_artifact(
    component: &Component,
    tag: &str,
) -> std::result::Result<Option<PathBuf>, String> {
    let Some(remote_url) = component.remote_url.as_ref() else {
        return Ok(None);
    };
    let Some(github) = release_download::parse_github_url(remote_url) else {
        return Ok(None);
    };
    let Some(artifact_name) = release_download::resolve_artifact_name(component) else {
        return Ok(None);
    };

    log_status!(
        "deploy",
        "Attempting to download release artifact for '{}' tag {} from GitHub...",
        component.id,
        tag
    );

    match release_download::download_release_artifact(&github, tag, &artifact_name) {
        Ok(path) => Ok(Some(path)),
        Err(e) => {
            if e.code == crate::core::error::ErrorCode::ValidationInvalidArgument {
                return Err(e.to_string());
            }

            log_status!(
                "deploy",
                "Release download failed for '{}': {} — falling back to local build",
                component.id,
                e
            );
            Ok(None)
        }
    }
}

fn deploy_release_tag(component: &Component, config: &DeployConfig) -> Option<String> {
    if let Some(version) = config.expected_version.as_deref() {
        return Some(deploy_tag_for_version(component, version));
    }

    git::get_latest_tag(&component.local_path).ok().flatten()
}

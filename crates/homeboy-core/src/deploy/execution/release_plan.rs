use crate::component::Component;
use crate::release;

use super::super::orchestration_tag_checkout::deploy_tag_for_version;
use super::super::types::DeployConfig;
use crate::git::release_download;

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
    if config.requested_ref.is_some() {
        return local_release_artifact_plan("--ref deploys an exact materialized commit");
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

pub(crate) fn resolve_planned_release_artifact(
    component: &Component,
    tag: &str,
    store: &mut release_download::ReleaseArtifactStore,
) -> std::result::Result<release_download::ReleaseArtifactLease, String> {
    let remote_url = component
        .remote_url
        .as_deref()
        .ok_or_else(|| "component has no remote_url".to_string())?;
    let github = release_download::parse_github_url(remote_url)
        .ok_or_else(|| "component remote_url is not a GitHub repository URL".to_string())?;
    let artifact_name = release_download::resolve_artifact_name(component)
        .ok_or_else(|| "component has no build_artifact filename".to_string())?;
    store
        .resolve(&github, &component.github, tag, &artifact_name)
        .map_err(|error| release_asset_download_error(component, tag, &artifact_name, error))
}

fn release_asset_download_error(
    component: &Component,
    tag: &str,
    artifact_name: &str,
    error: crate::error::Error,
) -> String {
    let error_details = if error.details.is_null() {
        error.to_string()
    } else {
        format!("{}: {}", error, error.details)
    };

    format!(
        "artifact source release_asset failed for '{}' tag {} artifact '{}': {}. Refusing to fall back to local_build; use --tagged to request an explicit local tag build.",
        component.id, tag, artifact_name, error_details
    )
}

fn deploy_release_tag(component: &Component, config: &DeployConfig) -> Option<String> {
    if let Some(version) = config.expected_version.as_deref() {
        return Some(deploy_tag_for_version(component, version));
    }

    release::latest_component_tag(component).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::release_asset_download_error;
    use crate::component::Component;
    use crate::error::Error;

    #[test]
    fn release_asset_download_error_fails_closed_without_local_build_fallback() {
        let component = Component {
            id: "example".to_string(),
            ..Component::default()
        };

        let message = release_asset_download_error(
            &component,
            "v1.2.3",
            "example.zip",
            Error::internal_io(
                "HTTP 404".to_string(),
                Some("download release artifact".to_string()),
            ),
        );

        assert!(message.contains("artifact source release_asset failed"));
        assert!(message.contains("Refusing to fall back to local_build"));
        assert!(message.contains("use --tagged"));
    }
}

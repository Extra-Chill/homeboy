use homeboy_core::component::Component;
use homeboy_core::error::Error;
use homeboy_version as release;

use super::super::orchestration_tag_checkout::deploy_tag_for_version;
use super::super::types::DeployConfig;
use homeboy_core::git::release_download;

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
    // Checked before `--ref` so an explicit local tag build always wins, and so
    // it stays the documented escape hatch when a release tag has no reusable
    // asset (#12215).
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
    // An exact `--ref` addresses one specific commit, but generated release
    // bytes are not necessarily reproducible from the tagged source alone, so
    // exact Git identity does not imply exact package identity. When the ref
    // names a release, the canonical asset for that release *is* the artifact;
    // rebuilding it locally is what shipped bytes that then read back as
    // `remote_modified` against the release package (#12215).
    //
    // The discrimination is the component's own tag namespace, so this stays a
    // pure, network-free plan: a ref outside that namespace (branch, raw SHA,
    // foreign tag) is not a release ref and still builds locally.
    let tag = match config.requested_ref_for(&component.id) {
        Some(requested_ref) => {
            match release::component_release_tag_version(component, requested_ref) {
                Ok(Some(_)) => requested_ref.to_string(),
                _ => {
                    return local_release_artifact_plan(format!(
                        "--ref '{requested_ref}' is not a release tag for this component"
                    ))
                }
            }
        }
        None => {
            let Some(tag) = deploy_release_tag(component, config) else {
                return local_release_artifact_plan(
                    "no version tag found for release asset lookup",
                );
            };
            tag
        }
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
) -> std::result::Result<release_download::ReleaseArtifactLease, Error> {
    let remote_url = component.remote_url.as_deref().ok_or_else(|| {
        Error::invalid_argument_for(
            "component.remote_url",
            "component has no remote_url",
            component.id.as_str(),
        )
    })?;
    let github = release_download::parse_github_url(remote_url).ok_or_else(|| {
        Error::invalid_argument_for(
            "component.remote_url",
            format!("component remote_url is not a GitHub repository URL: '{remote_url}'"),
            component.id.as_str(),
        )
    })?;
    let artifact_name = release_download::resolve_artifact_name(component).ok_or_else(|| {
        Error::invalid_argument_for(
            "component.build_artifact",
            "component has no build_artifact filename",
            component.id.as_str(),
        )
    })?;
    store
        .resolve(&github, &component.github, tag, &artifact_name)
        .map_err(|error| release_asset_download_error(component, tag, &artifact_name, error))
}

/// Wrap a release-asset download failure without discarding it.
///
/// The download error is already structured — it carries a code, and details
/// such as the HTTP status that explains *why* the asset could not be fetched.
/// Rendering all of that into one sentence (#11135) made release-asset failures
/// unclassifiable by anything downstream, so instead the code and details are
/// carried through verbatim, the original is attached as the source, and the
/// fail-closed policy statement becomes a hint rather than message prose.
fn release_asset_download_error(
    component: &Component,
    tag: &str,
    artifact_name: &str,
    error: Error,
) -> Error {
    let message = format!(
        "artifact source release_asset failed for '{}' tag {} artifact '{}': {}",
        component.id, tag, artifact_name, error.message
    );
    let mut converted = Error::new(error.code, message, error.details.clone());
    converted.hints = error.hints.clone();
    converted.retryable = error.retryable;
    converted.with_source(error).with_hint(
        "Refusing to fall back to local_build; use --tagged to request an explicit local tag build.",
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
    use homeboy_core::component::Component;
    use homeboy_core::error::{Error, ErrorCode};

    #[test]
    fn release_asset_download_error_fails_closed_without_local_build_fallback() {
        let component = Component {
            id: "example".to_string(),
            ..Component::default()
        };

        let error = release_asset_download_error(
            &component,
            "v1.2.3",
            "example.zip",
            Error::internal_io(
                "HTTP 404".to_string(),
                Some("download release artifact".to_string()),
            ),
        );

        assert!(error
            .message
            .contains("artifact source release_asset failed"));
        // The fail-closed policy is a next action, so it is carried as a hint.
        assert!(error.hints.iter().any(|hint| hint
            .message
            .contains("Refusing to fall back to local_build")));
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("use --tagged")));
    }

    /// #11135: the download failure's own classification is what makes a
    /// stranded release-asset deploy machine-readable, so wrapping it must not
    /// collapse the code, the details, or the causal chain into prose.
    #[test]
    fn release_asset_download_error_preserves_the_underlying_classification() {
        let component = Component {
            id: "example".to_string(),
            ..Component::default()
        };

        let error = release_asset_download_error(
            &component,
            "v1.2.3",
            "example.zip",
            Error::internal_io(
                "HTTP 404".to_string(),
                Some("download release artifact".to_string()),
            ),
        );

        assert_eq!(error.code, ErrorCode::InternalIoError);
        assert_eq!(
            error.details.get("error").and_then(|value| value.as_str()),
            Some("HTTP 404")
        );
        assert_eq!(
            error
                .details
                .get("context")
                .and_then(|value| value.as_str()),
            Some("download release artifact")
        );
        assert!(
            std::error::Error::source(&error).is_some(),
            "the originating download error stays reachable as the source"
        );
    }

    /// A full disk during the download must stay a storage-exhaustion failure
    /// after wrapping; the `String` channel turned it into an ordinary sentence.
    #[test]
    fn release_asset_download_error_keeps_storage_exhaustion_distinguishable() {
        let component = Component {
            id: "example".to_string(),
            ..Component::default()
        };

        let error = release_asset_download_error(
            &component,
            "v1.2.3",
            "example.zip",
            Error::storage_exhausted("No space left on device", None),
        );

        assert!(error.is_storage_exhausted(), "{error:?}");
        assert_eq!(error.retryable, Some(false));
    }
}

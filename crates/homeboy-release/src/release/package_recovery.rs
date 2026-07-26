use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use homeboy_core::error::{Error, Result};
use homeboy_core::{git, paths};

use super::context::{load_component, resolve_extensions};
use super::executor::artifacts::{
    PACKAGE_RECOVERY_MANIFEST_SCHEMA, PACKAGE_RECOVERY_MANIFEST_SCHEMA_VERSION,
};
use super::executor::run_package;
use super::types::{ReleaseArtifact, ReleaseOptions, ReleaseState, ReleaseStepResult};

#[derive(Debug, Clone, Serialize)]
pub struct ReleasePackageResult {
    pub schema: &'static str,
    pub schema_version: u32,
    pub component_id: String,
    pub tag: String,
    pub version: String,
    pub commit: String,
    pub artifact_dir: String,
    pub manifest_path: String,
    pub artifacts: Vec<ReleaseArtifact>,
    pub package_step: ReleaseStepResult,
}

pub fn package_existing_tag(
    component_id: &str,
    path_override: Option<String>,
    tag: &str,
    skip_build_validation: bool,
) -> Result<ReleasePackageResult> {
    let component = load_component(
        component_id,
        &ReleaseOptions {
            path_override,
            ..Default::default()
        },
    )?;
    let head_commit = git::get_head_commit(&component.local_path)?;
    validate_existing_tag_at_head(&component.local_path, tag, &head_commit)?;

    let version = super::version::read_component_version(&component)?.version;
    let extensions = resolve_extensions(&component)?;
    let mut state = ReleaseState {
        version: Some(version.clone()),
        tag: Some(tag.to_string()),
        ..Default::default()
    };

    let package_step = run_package(
        &extensions,
        &mut state,
        &component,
        component_id,
        &component.local_path,
        None,
        component.build_artifact.as_deref(),
        skip_build_validation,
    )?;
    if state.artifacts.is_empty() {
        return Err(Error::internal_unexpected(
            "release.package completed without producing any artifacts",
        ));
    }
    super::executor::artifacts::establish_publication_authority(&mut state)?;

    let artifact_dir = release_package_dir(component_id, tag)?;
    fs::create_dir_all(&artifact_dir).map_err(|error| {
        Error::internal_io(
            format!(
                "Failed to create release package artifact directory {}: {}",
                artifact_dir.display(),
                error
            ),
            Some(artifact_dir.display().to_string()),
        )
    })?;

    let artifacts = copy_release_artifacts(&component.local_path, &artifact_dir, &state.artifacts)?;
    let manifest_path = artifact_dir.join("manifest.json");
    let result = ReleasePackageResult {
        schema: PACKAGE_RECOVERY_MANIFEST_SCHEMA,
        schema_version: PACKAGE_RECOVERY_MANIFEST_SCHEMA_VERSION,
        component_id: component_id.to_string(),
        tag: tag.to_string(),
        version,
        commit: head_commit,
        artifact_dir: artifact_dir.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        artifacts,
        package_step,
    };
    let manifest = serde_json::to_string_pretty(&result).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("release package manifest".to_string()),
        )
    })?;
    fs::write(&manifest_path, manifest).map_err(|error| {
        Error::internal_io(
            format!(
                "Failed to write release package manifest {}: {}",
                manifest_path.display(),
                error
            ),
            Some(manifest_path.display().to_string()),
        )
    })?;

    // The durable recovery directory now owns the deliverables. Remove only
    // checkout paths proven to have been created by this package invocation.
    super::executor::run_cleanup(&component, &state)?;

    homeboy_core::log_status!(
        "release",
        "Release package artifacts written to {}",
        artifact_dir.display()
    );
    homeboy_core::log_status!(
        "release",
        "Release package manifest written to {}",
        manifest_path.display()
    );
    for artifact in result
        .artifacts
        .iter()
        .filter(|artifact| artifact.publication_authority)
    {
        homeboy_core::log_status!(
            "release",
            "Authoritative release asset: {} sha256 {} ({})",
            artifact.path,
            artifact.sha256.as_deref().unwrap_or("unavailable"),
            artifact.producer
        );
    }
    homeboy_core::log_status!(
        "release",
        "Resume publication: homeboy release {} --head --from-artifacts {}",
        component_id,
        artifact_dir.display()
    );

    Ok(result)
}

fn validate_existing_tag_at_head(local_path: &str, tag: &str, head_commit: &str) -> Result<()> {
    let local_tag_commit = if git::tag_exists_locally(local_path, tag).unwrap_or(false) {
        Some(git::get_tag_commit(local_path, tag)?)
    } else {
        None
    };
    let remote_tag_commit = git::remote_tag_commit(local_path, tag)?;
    let tag_commit = local_tag_commit
        .clone()
        .or_else(|| remote_tag_commit.clone())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "tag",
                format!("Release tag '{}' does not exist locally or on origin", tag),
                Some(tag.to_string()),
                Some(vec![
                    format!("Fetch tags: git -C {} fetch --tags", local_path),
                    "Then check out the tagged commit before regenerating the package".to_string(),
                ]),
            )
        })?;

    if tag_commit != head_commit {
        return Err(Error::validation_invalid_argument(
            "tag",
            format!(
                "Release tag '{}' points at {}, but HEAD is {}",
                tag,
                short_sha(&tag_commit),
                short_sha(head_commit)
            ),
            Some(tag.to_string()),
            Some(vec![format!(
                "Check out the tagged commit first: git -C {} checkout {}",
                local_path, tag
            )]),
        ));
    }

    Ok(())
}

fn release_package_dir(component_id: &str, tag: &str) -> Result<PathBuf> {
    Ok(paths::artifact_root()?
        .join("release-packages")
        .join(paths::sanitize_path_segment(component_id))
        .join(paths::sanitize_path_segment(tag)))
}

fn copy_release_artifacts(
    component_local_path: &str,
    artifact_dir: &Path,
    artifacts: &[ReleaseArtifact],
) -> Result<Vec<ReleaseArtifact>> {
    let mut copied = Vec::new();
    // The recovery manifest is a publication contract, so retain only the
    // selected asset for each target name. Copying superseded duplicates would
    // overwrite the same destination and make the manifest unverifiable.
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.publication_authority)
    {
        let source = resolve_artifact_path(component_local_path, &artifact.path);
        let file_name = source.file_name().ok_or_else(|| {
            Error::validation_invalid_argument(
                "release.artifacts.path",
                format!("Release artifact path '{}' has no file name", artifact.path),
                Some(artifact.path.clone()),
                None,
            )
        })?;
        let destination = artifact_dir.join(file_name);
        fs::copy(&source, &destination).map_err(|error| {
            Error::internal_io(
                format!(
                    "Failed to copy release artifact {} to {}: {}",
                    source.display(),
                    destination.display(),
                    error
                ),
                Some(source.display().to_string()),
            )
        })?;
        copied.push(ReleaseArtifact {
            path: destination.display().to_string(),
            durable_path: Some(destination.display().to_string()),
            artifact_type: artifact.artifact_type.clone(),
            platform: artifact.platform.clone(),
            phase: artifact.phase.clone(),
            producer: artifact.producer.clone(),
            sha256: artifact.sha256.clone(),
            publication_authority: artifact.publication_authority,
        });
    }
    Ok(copied)
}

fn resolve_artifact_path(component_local_path: &str, artifact_path: &str) -> PathBuf {
    let path = PathBuf::from(artifact_path);
    if path.is_absolute() {
        path
    } else {
        Path::new(component_local_path).join(path)
    }
}

fn short_sha(commit: &str) -> &str {
    &commit[..8.min(commit.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_release_artifacts_copies_relative_artifact_to_durable_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = temp.path().join("component");
        let build = component.join("build");
        let artifact_dir = temp.path().join("artifact-root");
        fs::create_dir_all(&build).expect("build dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(build.join("plugin.zip"), "zip").expect("artifact");

        let copied = copy_release_artifacts(
            &component.display().to_string(),
            &artifact_dir,
            &[ReleaseArtifact {
                path: "build/plugin.zip".to_string(),
                durable_path: None,
                artifact_type: Some("archive".to_string()),
                platform: None,
                phase: "final".to_string(),
                producer: "test".to_string(),
                sha256: None,
                publication_authority: true,
            }],
        )
        .expect("copy artifacts");

        let copied_path = artifact_dir.join("plugin.zip");
        assert_eq!(fs::read_to_string(&copied_path).expect("copied"), "zip");
        assert_eq!(copied[0].path, copied_path.display().to_string());
        assert_eq!(copied[0].artifact_type.as_deref(), Some("archive"));
    }

    #[test]
    fn copy_release_artifacts_omits_superseded_duplicate_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = temp.path().join("component");
        let build = component.join("build");
        let artifact_dir = temp.path().join("artifact-root");
        fs::create_dir_all(&build).expect("build dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(build.join("plugin.zip"), "final").expect("artifact");

        let copied = copy_release_artifacts(
            &component.display().to_string(),
            &artifact_dir,
            &[
                ReleaseArtifact {
                    path: "build/plugin.zip".to_string(),
                    durable_path: None,
                    artifact_type: None,
                    platform: Some("preflight".to_string()),
                    phase: "preflight".to_string(),
                    producer: "preflight".to_string(),
                    sha256: Some("superseded".to_string()),
                    publication_authority: false,
                },
                ReleaseArtifact {
                    path: "build/plugin.zip".to_string(),
                    durable_path: None,
                    artifact_type: None,
                    platform: Some("universal".to_string()),
                    phase: "final".to_string(),
                    producer: "package".to_string(),
                    sha256: Some("authoritative".to_string()),
                    publication_authority: true,
                },
            ],
        )
        .expect("copy authoritative artifact");

        assert_eq!(copied.len(), 1);
        assert_eq!(
            fs::read_to_string(artifact_dir.join("plugin.zip")).unwrap(),
            "final"
        );
        assert_eq!(copied[0].platform.as_deref(), Some("universal"));
        assert!(copied[0].publication_authority);
    }
}

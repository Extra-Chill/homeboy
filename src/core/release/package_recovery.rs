use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::error::{Error, Result};
use crate::core::{git, paths};

use super::context::{load_component, resolve_extensions};
use super::executor::run_package;
use super::types::{ReleaseArtifact, ReleaseOptions, ReleaseState, ReleaseStepResult};

#[derive(Debug, Clone, Serialize)]
pub struct ReleasePackageResult {
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
        component_id,
        &component.local_path,
        skip_build_validation,
    )?;
    if state.artifacts.is_empty() {
        return Err(Error::internal_unexpected(
            "release.package completed without producing any artifacts",
        ));
    }

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

    log_status!(
        "release",
        "Release package artifacts written to {}",
        artifact_dir.display()
    );
    log_status!(
        "release",
        "Release package manifest written to {}",
        manifest_path.display()
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
    for artifact in artifacts {
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
            artifact_type: artifact.artifact_type.clone(),
            platform: artifact.platform.clone(),
            durable_path: None,
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
                artifact_type: Some("archive".to_string()),
                platform: None,
                durable_path: None,
            }],
        )
        .expect("copy artifacts");

        let copied_path = artifact_dir.join("plugin.zip");
        assert_eq!(fs::read_to_string(&copied_path).expect("copied"), "zip");
        assert_eq!(copied[0].path, copied_path.display().to_string());
        assert_eq!(copied[0].artifact_type.as_deref(), Some("archive"));
    }
}

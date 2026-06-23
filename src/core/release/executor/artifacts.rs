use crate::core::error::{Error, Result};

use super::{step_success, ReleaseArtifact, ReleaseState, ReleaseStepResult};

/// Inventory artifacts that were already built by an external release build.
/// This lets `homeboy release --head --from-artifacts <dir>` reuse the normal
/// github.release and publish steps without re-running release.package.
pub(crate) fn run_artifact_inventory(
    state: &mut ReleaseState,
    artifact_dir: &str,
) -> Result<ReleaseStepResult> {
    let dir = std::path::Path::new(artifact_dir);
    if !dir.is_dir() {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!("Artifact directory '{}' does not exist", artifact_dir),
            Some(artifact_dir.to_string()),
            None,
        ));
    }

    let mut artifacts = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to read artifact directory '{}': {}",
                artifact_dir, e
            ),
            Some(artifact_dir.to_string()),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                format!("Failed to read artifact directory entry: {}", e),
                Some(artifact_dir.to_string()),
            )
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let canonical = std::fs::canonicalize(&path).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to resolve artifact path '{}': {}",
                    path.display(),
                    e
                ),
                Some(path.display().to_string()),
            )
        })?;
        artifacts.push(ReleaseArtifact {
            path: canonical.display().to_string(),
            durable_path: None,
            artifact_type: None,
            platform: None,
        });
    }

    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    if artifacts.is_empty() {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!("Artifact directory '{}' contains no files", artifact_dir),
            Some(artifact_dir.to_string()),
            None,
        ));
    }

    let artifact_count = artifacts.len();
    state.artifacts.extend(artifacts.clone());
    let data = serde_json::json!({
        "dir": artifact_dir,
        "artifact_count": artifact_count,
        "artifacts": artifacts,
    });

    Ok(step_success(
        "artifacts.inventory",
        "artifacts.inventory",
        Some(data),
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::run_artifact_inventory;
    use crate::core::release::types::ReleaseState;
    use crate::core::release::ReleaseStepStatus;

    #[test]
    fn test_run_artifact_inventory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("homeboy.tar.gz");
        std::fs::write(&artifact_path, "artifact").expect("write artifact");
        std::fs::create_dir(temp.path().join("nested")).expect("nested dir");

        let mut state = ReleaseState::default();
        let result = run_artifact_inventory(&mut state, &temp.path().to_string_lossy())
            .expect("inventory should succeed");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(
            state.artifacts[0].path,
            std::fs::canonicalize(&artifact_path)
                .expect("canonical artifact")
                .display()
                .to_string()
        );
    }
}

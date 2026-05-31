use std::path::{Path, PathBuf};

use crate::core::error::{Error, Result};
use crate::core::extension::ExtensionManifest;
use crate::core::release::types::{ReleaseState, ReleaseStepResult};

use super::{run_package, step_success};

/// Validate packaging against a temporary component copy before the release
/// pipeline mutates changelog, version files, or git state.
pub(crate) fn run_package_preflight(
    extensions: &[ExtensionManifest],
    component_id: &str,
    component_local_path: &str,
) -> Result<ReleaseStepResult> {
    let temp = create_release_preflight_tempdir()?;
    let temp_component_path = temp.join("component");
    copy_release_preflight_tree(Path::new(component_local_path), &temp_component_path)?;

    let mut state = ReleaseState::default();
    let result = run_package(
        extensions,
        &mut state,
        component_id,
        &temp_component_path.to_string_lossy(),
    );

    let _ = std::fs::remove_dir_all(&temp);
    let result = result?;

    let data = serde_json::json!({
        "component_path": component_local_path,
        "validated_action": "release.package",
        "artifacts": state.artifacts,
        "package_result": result.data,
    });
    Ok(step_success(
        "preflight.package",
        "preflight.package",
        Some(data),
        Vec::new(),
    ))
}

fn create_release_preflight_tempdir() -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "homeboy-release-package-preflight-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));

    std::fs::create_dir_all(&path).map_err(|e| {
        Error::internal_io(
            format!("Failed to create package preflight tempdir: {}", e),
            Some(path.display().to_string()),
        )
    })?;

    Ok(path)
}

fn copy_release_preflight_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination).map_err(|e| {
        Error::internal_io(
            format!("Failed to create package preflight copy: {}", e),
            Some(destination.display().to_string()),
        )
    })?;

    for entry in std::fs::read_dir(source).map_err(|e| {
        Error::internal_io(
            format!("Failed to read package preflight source: {}", e),
            Some(source.display().to_string()),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                format!("Failed to read package preflight source entry: {}", e),
                Some(source.display().to_string()),
            )
        })?;
        let file_name = entry.file_name();
        if file_name == std::ffi::OsStr::new(".git") {
            continue;
        }

        let from = entry.path();
        let to = destination.join(&file_name);
        copy_release_preflight_entry(&from, &to)?;
    }

    Ok(())
}

fn copy_release_preflight_entry(source: &Path, destination: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source).map_err(|e| {
        Error::internal_io(
            format!("Failed to inspect package preflight entry: {}", e),
            Some(source.display().to_string()),
        )
    })?;

    if metadata.file_type().is_symlink() {
        copy_release_preflight_symlink(source, destination)
    } else if metadata.is_dir() {
        copy_release_preflight_tree(source, destination)
    } else if metadata.is_file() {
        std::fs::copy(source, destination).map(|_| ()).map_err(|e| {
            Error::internal_io(
                format!("Failed to copy package preflight file: {}", e),
                Some(source.display().to_string()),
            )
        })
    } else {
        Ok(())
    }
}

fn copy_release_preflight_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = std::fs::read_link(source).map_err(|e| {
        Error::internal_io(
            format!("Failed to read package preflight symlink: {}", e),
            Some(source.display().to_string()),
        )
    })?;

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, destination).map_err(|e| {
            Error::internal_io(
                format!("Failed to copy package preflight symlink: {}", e),
                Some(source.display().to_string()),
            )
        })
    }

    #[cfg(not(unix))]
    {
        let target_path = if target.is_absolute() {
            target
        } else {
            source
                .parent()
                .map(|parent| parent.join(&target))
                .unwrap_or(target)
        };
        copy_release_preflight_entry(&target_path, destination)
    }
}

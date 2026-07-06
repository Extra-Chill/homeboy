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
    skip_build_validation: bool,
) -> Result<ReleaseStepResult> {
    // Inspect the original component checkout (which still has its `.git`) for
    // git-pinned dependencies that lack a committed lockfile. The isolated
    // build copy below excludes `.git`, so this committed-state check must run
    // against the source tree before we mutate or build anything.
    super::lockfile_guard::guard_committed_lockfiles(Path::new(component_local_path))?;
    super::lockfile_guard::guard_local_file_dependencies(Path::new(component_local_path))?;

    let source_component_path = Path::new(component_local_path);
    let source_root = release_preflight_source_root(source_component_path)?;
    let temp = create_release_preflight_tempdir()?;
    let temp_root_path = temp.join("repository");
    copy_release_preflight_tree(&source_root, &temp_root_path)?;
    let temp_component_path =
        release_preflight_component_path(source_component_path, &source_root, &temp_root_path)?;

    let mut state = ReleaseState::default();
    let result = run_package(
        extensions,
        &mut state,
        component_id,
        &temp_component_path.to_string_lossy(),
        Some(component_local_path),
        skip_build_validation,
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

fn release_preflight_source_root(component_path: &Path) -> Result<PathBuf> {
    let component_path = component_path.canonicalize().map_err(|e| {
        Error::internal_io(
            format!("Failed to resolve package preflight component path: {}", e),
            Some(component_path.display().to_string()),
        )
    })?;

    let mut current = component_path.as_path();
    loop {
        if current.join(".git").exists() {
            return Ok(current.to_path_buf());
        }
        let Some(parent) = current.parent() else {
            return Ok(component_path);
        };
        current = parent;
    }
}

fn release_preflight_component_path(
    source_component_path: &Path,
    source_root: &Path,
    temp_root_path: &Path,
) -> Result<PathBuf> {
    let source_component_path = source_component_path.canonicalize().map_err(|e| {
        Error::internal_io(
            format!("Failed to resolve package preflight component path: {}", e),
            Some(source_component_path.display().to_string()),
        )
    })?;

    let relative_component_path = source_component_path
        .strip_prefix(source_root)
        .map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to map package preflight component path into staged repo: {}",
                    e
                ),
                Some(format!(
                    "component: {}; source root: {}",
                    source_component_path.display(),
                    source_root.display()
                )),
            )
        })?;

    Ok(temp_root_path.join(relative_component_path))
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

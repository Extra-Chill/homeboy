//! Detection of Homeboy's own temporary build artifacts.
//!
//! These helpers identify detached temp targets, partial temp checkouts, and
//! full source checkouts that belong to Homeboy itself, so `homeboy cleanup`
//! can reclaim space from its own scratch directories without touching
//! unrelated worktrees. Split out of the cleanup command root to keep the
//! parent module under its structural item threshold.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::{git, Error, Result};

use super::{
    git_safety, has_tracked_changes_under, path_size, ArtifactCleanupCandidate,
    ArtifactCleanupOptions,
};

pub(super) fn homeboy_source_checkout() -> Result<PathBuf> {
    let manifest_dir = option_env!("CARGO_MANIFEST_DIR").ok_or_else(|| {
        Error::validation_invalid_argument(
            "self_artifacts",
            "Homeboy source checkout is unavailable for this binary",
            None,
            None,
        )
    })?;
    validate_homeboy_manifest_dir(Path::new(manifest_dir))
}

pub(super) fn validate_homeboy_manifest_dir(manifest_dir: &Path) -> Result<PathBuf> {
    let cargo_toml = manifest_dir.join("Cargo.toml");
    if !cargo_toml.is_file() {
        return Err(Error::validation_invalid_argument(
            "self_artifacts",
            format!("{} does not contain Cargo.toml", manifest_dir.display()),
            None,
            None,
        ));
    }

    let raw = fs::read_to_string(&cargo_toml).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", cargo_toml.display())),
        )
    })?;
    if !raw.lines().any(|line| line.trim() == "name = \"homeboy\"") {
        return Err(Error::validation_invalid_argument(
            "self_artifacts",
            format!("{} is not the Homeboy crate manifest", cargo_toml.display()),
            None,
            None,
        ));
    }

    Ok(manifest_dir.to_path_buf())
}

pub(super) fn self_temp_artifact_candidates(
    options: &ArtifactCleanupOptions,
) -> Result<Vec<ArtifactCleanupCandidate>> {
    if !options.self_artifacts && options.temp_roots.is_empty() {
        return Ok(Vec::new());
    }

    let roots = if options.temp_roots.is_empty() {
        default_self_temp_roots()
    } else {
        options.temp_roots.clone()
    };
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for root in roots {
        if !root.is_dir() || !seen.insert(root.clone()) {
            continue;
        }
        for entry in fs::read_dir(&root).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read temp root {}", root.display())),
            )
        })? {
            let entry = entry.map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("read temp root entry {}", root.display())),
                )
            })?;
            let path = entry.path();
            if !is_detached_homeboy_temp_artifact(&path) {
                if let Some(candidate) = temp_homeboy_checkout_target_candidate(&path)? {
                    candidates.push(candidate);
                } else if let Some(candidate) = partial_homeboy_temp_target_candidate(&path)? {
                    candidates.push(candidate);
                }
                continue;
            }
            let size_bytes = path_size(&path)?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            candidates.push(ArtifactCleanupCandidate {
                worktree: root.to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                relative_path: name,
                kind: "detached_homeboy_temp_artifact".to_string(),
                declared_by: "self_temp_root".to_string(),
                size_bytes,
                source_dirty: false,
                unpushed_commits: false,
            });
        }
    }

    Ok(candidates)
}

fn temp_homeboy_checkout_target_candidate(
    checkout: &Path,
) -> Result<Option<ArtifactCleanupCandidate>> {
    if !is_homeboy_source_checkout(checkout)? {
        return Ok(None);
    }

    let target = checkout.join("target");
    let Ok(metadata) = fs::symlink_metadata(&target) else {
        return Ok(None);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(None);
    }

    let safety = match git_safety(checkout) {
        Ok(safety) => safety,
        Err(_) => return Ok(None),
    };
    if has_tracked_changes_under(&safety.dirty_paths, "target") {
        return Ok(None);
    }

    let size_bytes = path_size(&target)?;
    Ok(Some(ArtifactCleanupCandidate {
        worktree: checkout.to_string_lossy().to_string(),
        path: target.to_string_lossy().to_string(),
        relative_path: "target".to_string(),
        kind: "temp_homeboy_checkout_target".to_string(),
        declared_by: "self_temp_root".to_string(),
        size_bytes,
        source_dirty: safety.source_dirty,
        unpushed_commits: safety.unpushed_commits,
    }))
}

fn partial_homeboy_temp_target_candidate(
    temp_dir: &Path,
) -> Result<Option<ArtifactCleanupCandidate>> {
    let Ok(metadata) = fs::symlink_metadata(temp_dir) else {
        return Ok(None);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(None);
    }

    let Some(name) = temp_dir.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if !name.starts_with("homeboy-")
        || temp_dir.join(".git").exists()
        || temp_dir.join("Cargo.toml").exists()
    {
        return Ok(None);
    }

    let target = temp_dir.join("target");
    let Ok(target_metadata) = fs::symlink_metadata(&target) else {
        return Ok(None);
    };
    if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
        return Ok(None);
    }
    if !partial_homeboy_temp_skeleton_is_safe(temp_dir)? {
        return Ok(None);
    }

    let size_bytes = path_size(&target)?;
    Ok(Some(ArtifactCleanupCandidate {
        worktree: temp_dir.to_string_lossy().to_string(),
        path: target.to_string_lossy().to_string(),
        relative_path: "target".to_string(),
        kind: "partial_homeboy_temp_target".to_string(),
        declared_by: "self_temp_root".to_string(),
        size_bytes,
        source_dirty: false,
        unpushed_commits: false,
    }))
}

fn partial_homeboy_temp_skeleton_is_safe(temp_dir: &Path) -> Result<bool> {
    let mut saw_target = false;
    for entry in fs::read_dir(temp_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read partial temp dir {}", temp_dir.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read partial temp dir entry {}",
                    temp_dir.display()
                )),
            )
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(false);
        };
        match name {
            "target" => saw_target = true,
            ".github" | "docs" | "src" | "tests" => {
                if !directory_tree_has_no_files(&entry.path())? {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(saw_target)
}

fn directory_tree_has_no_files(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("stat {}", path.display()))))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    for entry in fs::read_dir(path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read directory {}", path.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read directory entry {}", path.display())),
            )
        })?;
        let entry_path = entry.path();
        let entry_metadata = fs::symlink_metadata(&entry_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("stat {}", entry_path.display())),
            )
        })?;
        if !entry_metadata.is_dir() || entry_metadata.file_type().is_symlink() {
            return Ok(false);
        }
        if !directory_tree_has_no_files(&entry_path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_homeboy_source_checkout(path: &Path) -> Result<bool> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(false);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    if !path.join(".git").exists() || !path.join("Cargo.toml").is_file() {
        return Ok(false);
    }
    if !cargo_manifest_package_is_homeboy(&path.join("Cargo.toml"))? {
        return Ok(false);
    }

    let remotes = match git::run_git(path, &["remote", "-v"], "git remote -v") {
        Ok(output) => output,
        Err(_) => return Ok(false),
    };
    Ok(remotes.lines().any(|line| {
        line.contains("Extra-Chill/homeboy.git") || line.contains("Extra-Chill/homeboy ")
    }))
}

fn cargo_manifest_package_is_homeboy(cargo_toml: &Path) -> Result<bool> {
    let raw = fs::read_to_string(cargo_toml).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", cargo_toml.display())),
        )
    })?;

    let mut in_package = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed == "name = \"homeboy\"" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn default_self_temp_roots() -> Vec<PathBuf> {
    let temp_dir = std::env::temp_dir();
    vec![temp_dir.clone(), temp_dir.join("opencode")]
}

fn is_detached_homeboy_temp_artifact(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    if path.join(".git").exists() || path.join("Cargo.toml").exists() {
        return false;
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with("homeboy-")
        && (name.ends_with("-target") || name.contains("-target-") || name.ends_with("-build"))
}

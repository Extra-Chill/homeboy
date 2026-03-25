//! post_release_step — extracted from pipeline.rs.

use crate::git::{self, UncommittedChanges};
use super::super::types::{
    ReleaseOptions, ReleasePlan, ReleasePlanStatus, ReleasePlanStep, ReleaseRun,
    ReleaseSemverCommit, ReleaseSemverRecommendation,
};
use crate::component::{self, Component};
use crate::engine::local_files::FileSystem;
use crate::engine::pipeline::{self, PipelineStep};
use crate::engine::run_dir::{self, RunDir};
use crate::engine::validation::ValidationCollector;
use crate::error::{Error, Result};
use crate::extension::{self, ExtensionManifest};
use super::super::executor::ReleaseStepExecutor;
use super::super::resolver::{resolve_extensions, ReleaseCapabilityResolver};


/// Get list of files allowed to be dirty during release (relative paths).
pub(crate) fn get_release_allowed_files(
    changelog_path: &std::path::Path,
    version_targets: &[String],
    repo_root: &std::path::Path,
) -> Vec<String> {
    let mut allowed = Vec::new();

    // Add changelog (convert to relative path)
    if let Ok(relative) = changelog_path.strip_prefix(repo_root) {
        allowed.push(relative.to_string_lossy().to_string());
    }

    // Add version targets (convert to relative paths)
    for target in version_targets {
        if let Ok(relative) = std::path::Path::new(target).strip_prefix(repo_root) {
            let rel_str = relative.to_string_lossy().to_string();
            allowed.push(rel_str.clone());

            // If a Cargo.toml is a version target, also allow Cargo.lock
            // (version bump regenerates the lockfile to keep it in sync)
            if rel_str.ends_with("Cargo.toml") {
                let lock_path = relative.with_file_name("Cargo.lock");
                allowed.push(lock_path.to_string_lossy().to_string());
            }
        }
    }

    allowed
}

/// Get uncommitted files that are NOT in the allowed list.
pub(crate) fn get_unexpected_uncommitted_files(
    uncommitted: &UncommittedChanges,
    allowed: &[String],
) -> Vec<String> {
    let all_uncommitted: Vec<&String> = uncommitted
        .staged
        .iter()
        .chain(uncommitted.unstaged.iter())
        .chain(uncommitted.untracked.iter())
        .collect();

    all_uncommitted
        .into_iter()
        .filter(|f| !allowed.iter().any(|a| f.ends_with(a) || a.ends_with(*f)))
        .cloned()
        .collect()
}

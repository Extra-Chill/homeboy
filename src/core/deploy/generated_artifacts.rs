use std::path::Path;

use crate::core::defaults::deploy_generated_build_dir;
use crate::core::error::Result;
use crate::core::git;

pub(super) fn is_generated_build_path(rel_path: &str) -> bool {
    let build_dir = deploy_generated_build_dir();
    rel_path == build_dir || rel_path.starts_with(&format!("{build_dir}/"))
}

pub(super) fn unexpected_uncommitted_files_excluding_generated_build(
    local_path: &str,
) -> Result<Vec<String>> {
    let uncommitted = git::get_uncommitted_changes(local_path)?;
    if !uncommitted.has_changes {
        return Ok(Vec::new());
    }

    Ok(uncommitted
        .staged
        .iter()
        .chain(uncommitted.unstaged.iter())
        .chain(uncommitted.untracked.iter())
        .filter(|path| !is_generated_build_path(path))
        .cloned()
        .collect())
}

pub(super) fn cleanup_generated_build_artifacts(local_path: &Path) {
    let build_dir = local_path.join(deploy_generated_build_dir());
    if !build_dir.exists() {
        return;
    }

    if let Err(error) = std::fs::remove_dir_all(&build_dir) {
        log_status!(
            "cleanup",
            "Warning: failed to remove generated deploy artifact directory {}: {}",
            build_dir.display(),
            error
        );
    } else {
        log_status!(
            "cleanup",
            "Removed generated deploy artifact directory {}",
            build_dir.display()
        );
    }
}

pub(super) struct GeneratedBuildArtifactCleanupGuard<'a> {
    local_path: &'a Path,
    enabled: bool,
}

impl<'a> GeneratedBuildArtifactCleanupGuard<'a> {
    pub(super) fn new(local_path: &'a Path, enabled: bool) -> Self {
        Self {
            local_path,
            enabled,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.enabled = false;
    }
}

impl Drop for GeneratedBuildArtifactCleanupGuard<'_> {
    fn drop(&mut self) {
        if self.enabled {
            cleanup_generated_build_artifacts(self.local_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cleanup_generated_build_artifacts, is_generated_build_path,
        unexpected_uncommitted_files_excluding_generated_build,
    };

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(dir, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(dir.join("README.md"), "fixture\n").expect("readme");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "chore: initial"]);
        temp
    }

    #[test]
    fn root_homeboy_build_paths_are_generated() {
        assert!(is_generated_build_path(".homeboy-build"));
        assert!(is_generated_build_path(".homeboy-build/plugin.zip"));
        assert!(!is_generated_build_path("src/.homeboy-build/plugin.zip"));
        assert!(!is_generated_build_path("src/lib.rs"));
    }

    #[test]
    fn uncommitted_filter_ignores_only_generated_build_artifacts() {
        let temp = git_repo();
        let dir = temp.path();
        std::fs::create_dir_all(dir.join(".homeboy-build")).expect("build dir");
        std::fs::write(dir.join(".homeboy-build/plugin.zip"), "artifact").expect("artifact");
        std::fs::write(dir.join("src.rs"), "source\n").expect("source");

        let unexpected =
            unexpected_uncommitted_files_excluding_generated_build(&dir.to_string_lossy())
                .expect("status");

        assert_eq!(unexpected, vec!["src.rs"]);
    }

    #[test]
    fn cleanup_removes_generated_build_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join(".homeboy-build");
        std::fs::create_dir_all(&build_dir).expect("build dir");
        std::fs::write(build_dir.join("plugin.zip"), "artifact").expect("artifact");

        cleanup_generated_build_artifacts(temp.path());

        assert!(!build_dir.exists());
    }
}

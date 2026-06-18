use std::path::Path;

use crate::core::component::Component;
use crate::core::defaults::deploy_generated_build_dir;
use crate::core::error::Result;
use crate::core::git;

pub(super) fn is_generated_build_path(rel_path: &str) -> bool {
    let build_dir = deploy_generated_build_dir();
    rel_path == build_dir || rel_path.starts_with(&format!("{build_dir}/"))
}

pub(super) fn unexpected_uncommitted_files_excluding_generated_build(
    component: &Component,
) -> Result<Vec<String>> {
    let local_path = &component.local_path;
    let uncommitted = git::get_uncommitted_changes(local_path)?;
    if !uncommitted.has_changes {
        return Ok(Vec::new());
    }

    let mut unexpected: Vec<String> = uncommitted
        .staged
        .iter()
        .chain(uncommitted.unstaged.iter())
        .filter(|path| !is_generated_build_path(path))
        .cloned()
        .collect();

    unexpected.extend(
        uncommitted
            .untracked
            .iter()
            .filter(|path| !is_generated_build_path(path))
            .filter(|path| !is_deploy_target_debris_path(component, path))
            .cloned(),
    );

    Ok(unexpected)
}

fn is_deploy_target_debris_path(component: &Component, rel_path: &str) -> bool {
    if !component_uses_archive_deploy(component) {
        return false;
    }

    let remote_path = component
        .remote_path
        .trim()
        .trim_start_matches("./")
        .trim_matches('/');
    if remote_path.is_empty() || remote_path.starts_with('/') {
        return false;
    }

    let rel_path = rel_path.trim().trim_start_matches("./").trim_matches('/');
    if rel_path.is_empty() {
        return false;
    }

    rel_path == remote_path
        || rel_path.starts_with(&format!("{remote_path}/"))
        || remote_path.starts_with(&format!("{rel_path}/"))
}

fn component_uses_archive_deploy(component: &Component) -> bool {
    component.extract_command.is_some()
        || component
            .build_artifact
            .as_deref()
            .and_then(|artifact| Path::new(artifact).extension())
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "zip" | "tar" | "gz" | "tgz"))
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
        cleanup_generated_build_artifacts, is_deploy_target_debris_path, is_generated_build_path,
        unexpected_uncommitted_files_excluding_generated_build,
    };
    use crate::core::component::Component;
    use crate::core::defaults::deploy_generated_build_dir;

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
        run_git(dir, &["config", "user.name", "Fixture Test"]);
        std::fs::write(dir.join("README.md"), "fixture\n").expect("readme");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "chore: initial"]);
        temp
    }

    #[test]
    fn root_homeboy_build_paths_are_generated() {
        let build_dir = deploy_generated_build_dir();
        assert!(is_generated_build_path(&build_dir));
        assert!(is_generated_build_path(&format!("{build_dir}/plugin.zip")));
        assert!(!is_generated_build_path(&format!(
            "src/{build_dir}/plugin.zip"
        )));
        assert!(!is_generated_build_path("src/lib.rs"));
    }

    #[test]
    fn uncommitted_filter_ignores_only_generated_build_artifacts() {
        let temp = git_repo();
        let dir = temp.path();
        let build_dir = deploy_generated_build_dir();
        std::fs::create_dir_all(dir.join(&build_dir)).expect("build dir");
        std::fs::write(dir.join(&build_dir).join("plugin.zip"), "artifact").expect("artifact");
        std::fs::write(dir.join("src.rs"), "source\n").expect("source");

        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Component::default()
        };
        let unexpected =
            unexpected_uncommitted_files_excluding_generated_build(&component).expect("status");

        assert_eq!(unexpected, vec!["src.rs"]);
    }

    #[test]
    fn deploy_target_debris_matches_relative_remote_path_and_collapsed_untracked_dir() {
        let component = Component {
            remote_path: "wp-content/plugins/sample-plugin".to_string(),
            build_artifact: Some("dist/sample-plugin.zip".to_string()),
            ..Component::default()
        };

        assert!(is_deploy_target_debris_path(
            &component,
            "wp-content/plugins/sample-plugin/sample-plugin"
        ));
        assert!(is_deploy_target_debris_path(&component, "wp-content/"));
        assert!(!is_deploy_target_debris_path(&component, "src/lib.rs"));
    }

    #[test]
    fn cleanup_removes_generated_build_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join(deploy_generated_build_dir());
        std::fs::create_dir_all(&build_dir).expect("build dir");
        std::fs::write(build_dir.join("plugin.zip"), "artifact").expect("artifact");

        cleanup_generated_build_artifacts(temp.path());

        assert!(!build_dir.exists());
    }
}

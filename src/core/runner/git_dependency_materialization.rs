use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::core::error::{Error, Result};

use super::{
    workspace::{
        canonical_workspace_path, effective_snapshot_excludes, git_output, local_snapshot_stats,
        materialize_snapshot, snapshot_identity, DEFAULT_EXCLUDES,
    },
    Runner, RunnerWorkspaceSyncMode,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RunnerGitDependencyMaterializationOutput {
    pub local_path: String,
    pub remote_path: String,
    pub remote_url: String,
    pub head: String,
    pub status: String,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct RunnerGitDependencyMaterializationOptions {
    pub local_path: String,
    pub remote_path: String,
    pub remote_url: Option<String>,
    pub required_subpath: Option<String>,
}

pub(crate) fn materialize_git_dependency(
    runner: &Runner,
    options: RunnerGitDependencyMaterializationOptions,
) -> Result<RunnerGitDependencyMaterializationOutput> {
    let local_path = canonical_workspace_path(&options.local_path)?;
    if let Some(subpath) = options
        .required_subpath
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let required_path = local_path.join(subpath);
        if !required_path.is_dir() {
            return Err(Error::validation_invalid_argument(
                "rig_component_dependency",
                "rig dependency snapshot is missing required subpath",
                Some(required_path.display().to_string()),
                None,
            ));
        }
    }

    let remote_url = match options.remote_url {
        Some(remote_url) if !remote_url.trim().is_empty() => remote_url,
        _ => git_output(&local_path, &["config", "--get", "remote.origin.url"]).unwrap_or_default(),
    };
    let update_status = auto_update_clean_git_dependency(&local_path)?;
    let mut excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    for pattern in &runner.policy.snapshot_excludes {
        if !excludes.contains(pattern) {
            excludes.push(pattern.clone());
        }
    }
    let excludes = effective_snapshot_excludes(excludes, &runner.policy.snapshot_includes);
    let snapshot = snapshot_identity(&local_path, &excludes, &runner.policy.snapshot_includes)?;
    let stats = local_snapshot_stats(&local_path, &excludes, &runner.policy.snapshot_includes)?;
    materialize_snapshot(runner, &local_path, &options.remote_path, &excludes)?;

    Ok(RunnerGitDependencyMaterializationOutput {
        local_path: local_path.display().to_string(),
        remote_path: options.remote_path,
        remote_url,
        head: snapshot,
        status: update_status.label().to_string(),
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        files: stats.files,
        bytes: stats.bytes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DependencyUpdateStatus {
    NotGit,
    DirtySkipped,
    NoUpstreamSkipped,
    UpToDate,
    FastForwarded,
    FetchFailedCachedUpToDate,
}

impl DependencyUpdateStatus {
    fn label(self) -> &'static str {
        match self {
            Self::NotGit => "snapshotted",
            Self::DirtySkipped => "snapshotted_dirty_dependency_not_updated",
            Self::NoUpstreamSkipped => "snapshotted_no_upstream_dependency_not_updated",
            Self::UpToDate => "snapshotted_up_to_date",
            Self::FastForwarded => "snapshotted_fast_forwarded",
            Self::FetchFailedCachedUpToDate => "snapshotted_fetch_failed_cached_up_to_date",
        }
    }
}

fn auto_update_clean_git_dependency(local_path: &Path) -> Result<DependencyUpdateStatus> {
    if !local_path.join(".git").exists() {
        return Ok(DependencyUpdateStatus::NotGit);
    }

    let status = git_output(local_path, &["status", "--porcelain=v1"])?;
    if !status.trim().is_empty() {
        return Ok(DependencyUpdateStatus::DirtySkipped);
    }

    let upstream = match git_output(
        local_path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    ) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return Ok(DependencyUpdateStatus::NoUpstreamSkipped),
    };
    let remote = upstream.split('/').next().unwrap_or("").trim();
    if remote.is_empty() || remote == upstream {
        return Ok(DependencyUpdateStatus::NoUpstreamSkipped);
    }

    let before = git_output(local_path, &["rev-parse", "HEAD"])?;
    let upstream_head = git_output(local_path, &["rev-parse", "@{u}"])?;
    if let Err(err) = run_git(local_path, &["fetch", "--prune", remote]) {
        if before == upstream_head {
            return Ok(DependencyUpdateStatus::FetchFailedCachedUpToDate);
        }
        return Err(err);
    }
    run_git(local_path, &["merge", "--ff-only", "@{u}"])?;
    let after = git_output(local_path, &["rev-parse", "HEAD"])?;

    if before == after {
        Ok(DependencyUpdateStatus::UpToDate)
    } else {
        Ok(DependencyUpdateStatus::FastForwarded)
    }
}

fn run_git(local_path: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(local_path)
        .output()
        .map_err(|err| Error::internal_io(err.to_string(), Some("run git".to_string())))?;
    if output.status.success() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "rig_component_dependency",
        format!(
            "git dependency auto-update failed while running git {}",
            args.join(" ")
        ),
        Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        Some(vec![
            "Commit or stash dependency changes before Lab offload.".to_string(),
            "If the dependency branch diverged, update or rebase it manually before rerunning."
                .to_string(),
        ]),
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{auto_update_clean_git_dependency, DependencyUpdateStatus};

    #[test]
    fn auto_update_clean_dependency_fast_forwards_to_upstream() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let before = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        fixture.commit_file("next.txt", "next");
        fixture.push();
        let expected = fixture.head();

        let status = auto_update_clean_git_dependency(checkout.path()).expect("auto update");

        assert_eq!(status, DependencyUpdateStatus::FastForwarded);
        assert_ne!(before, expected);
        assert_eq!(
            git_output(checkout.path(), &["rev-parse", "HEAD"]),
            expected
        );
    }

    #[test]
    fn auto_update_dirty_dependency_leaves_checkout_unchanged() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let before = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        fs::write(checkout.path().join("dirty.txt"), "dirty").expect("write dirty file");
        fixture.commit_file("next.txt", "next");
        fixture.push();

        let status = auto_update_clean_git_dependency(checkout.path()).expect("auto update");

        assert_eq!(status, DependencyUpdateStatus::DirtySkipped);
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn auto_update_fetch_failure_uses_cached_upstream_when_checkout_matches() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let before = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        let missing_remote = checkout.path().join("missing-remote.git");
        run_git(
            checkout.path(),
            &[
                "remote",
                "set-url",
                "origin",
                missing_remote.to_str().expect("missing remote path"),
            ],
        );

        let status = auto_update_clean_git_dependency(checkout.path()).expect("auto update");

        assert_eq!(status, DependencyUpdateStatus::FetchFailedCachedUpToDate);
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), before);
    }

    struct GitFixture {
        remote: tempfile::TempDir,
        work: tempfile::TempDir,
    }

    impl GitFixture {
        fn new() -> Self {
            let remote = tempfile::tempdir().expect("remote");
            run_git(remote.path(), &["init", "--bare"]);

            let work = tempfile::tempdir().expect("work");
            run_git(work.path(), &["init"]);
            run_git(
                work.path(),
                &["config", "user.email", "homeboy@example.test"],
            );
            run_git(work.path(), &["config", "user.name", "Homeboy Test"]);
            run_git(
                work.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    remote.path().to_str().expect("remote path"),
                ],
            );

            Self { remote, work }
        }

        fn commit_file(&self, name: &str, contents: &str) {
            fs::write(self.work.path().join(name), contents).expect("write file");
            run_git(self.work.path(), &["add", name]);
            run_git(self.work.path(), &["commit", "-m", name]);
        }

        fn push(&self) {
            run_git(self.work.path(), &["push", "-u", "origin", "HEAD:main"]);
        }

        fn head(&self) -> String {
            git_output(self.work.path(), &["rev-parse", "HEAD"])
        }

        fn clone_checkout(&self) -> tempfile::TempDir {
            let checkout = tempfile::tempdir().expect("checkout");
            run_git(
                Path::new("/"),
                &[
                    "clone",
                    self.remote.path().to_str().expect("remote path"),
                    checkout.path().to_str().expect("checkout path"),
                ],
            );
            run_git(checkout.path(), &["checkout", "main"]);
            checkout
        }
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

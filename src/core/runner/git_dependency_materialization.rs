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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_subpath: Option<String>,
    pub used_pinned_ref: bool,
    /// True when the snapshot includes dirty tracked and/or untracked working
    /// tree changes (explicit `--allow-dirty-lab-workspace` overlay) rather than
    /// a clean checkout at HEAD. Makes bench artifact provenance explicit.
    pub dirty_overlay: bool,
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
    pub pinned_ref: Option<String>,
    /// When true, a dirty/untracked working tree is snapshotted as-is (overlay)
    /// instead of being refused. The snapshot already tars the working tree, so
    /// the dirty overlay travels to the runner. Defaults to false (clean-HEAD).
    pub allow_dirty: bool,
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
    let freshness = ensure_git_dependency_fresh(
        &local_path,
        options.pinned_ref.as_deref(),
        options.allow_dirty,
    )?;
    let dirty_overlay = freshness.status == DependencyUpdateStatus::DirtyOverlayAllowed;
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
        status: freshness.status.label().to_string(),
        branch: freshness.branch,
        before_sha: freshness.before_sha,
        after_sha: freshness.after_sha,
        upstream_sha: freshness.upstream_sha,
        upstream: freshness.upstream,
        pinned_ref: freshness.pinned_ref,
        required_subpath: options.required_subpath,
        used_pinned_ref: freshness.used_pinned_ref,
        dirty_overlay,
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        files: stats.files,
        bytes: stats.bytes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DependencyUpdateStatus {
    NotGit,
    DirtyNotUpdated,
    DirtyOverlayAllowed,
    NoUpstream,
    DetachedUnpinned,
    UpToDate,
    FastForwarded,
    PinnedRef,
    FetchFailedCachedUpToDate,
    FetchFailed,
    BehindAfterFetch,
}

impl DependencyUpdateStatus {
    fn label(self) -> &'static str {
        match self {
            Self::NotGit => "snapshotted",
            Self::DirtyNotUpdated => "dirty_not_updated",
            Self::DirtyOverlayAllowed => "snapshotted_dirty_overlay",
            Self::NoUpstream => "no_upstream",
            Self::DetachedUnpinned => "detached_unpinned",
            Self::UpToDate => "snapshotted_up_to_date",
            Self::FastForwarded => "snapshotted_fast_forwarded",
            Self::PinnedRef => "snapshotted_pinned_ref",
            Self::FetchFailedCachedUpToDate => "snapshotted_fetch_failed_cached_up_to_date",
            Self::FetchFailed => "fetch_failed_cached",
            Self::BehindAfterFetch => "behind_after_fetch",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::DirtyNotUpdated
                | Self::NoUpstream
                | Self::DetachedUnpinned
                | Self::FetchFailed
                | Self::BehindAfterFetch
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DependencyFreshness {
    status: DependencyUpdateStatus,
    branch: Option<String>,
    before_sha: Option<String>,
    after_sha: Option<String>,
    upstream_sha: Option<String>,
    upstream: Option<String>,
    pinned_ref: Option<String>,
    used_pinned_ref: bool,
}

fn ensure_git_dependency_fresh(
    local_path: &Path,
    pinned_ref: Option<&str>,
    allow_dirty: bool,
) -> Result<DependencyFreshness> {
    if !local_path.join(".git").exists() {
        return Ok(DependencyFreshness {
            status: DependencyUpdateStatus::NotGit,
            branch: None,
            before_sha: None,
            after_sha: None,
            upstream_sha: None,
            upstream: None,
            pinned_ref: pinned_ref.map(str::to_string),
            used_pinned_ref: pinned_ref.is_some(),
        });
    }

    let before = git_output(local_path, &["rev-parse", "HEAD"])?;
    let branch = git_output(local_path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok();
    if branch.as_deref() == Some("HEAD") && pinned_ref.is_none() {
        let freshness = DependencyFreshness {
            status: DependencyUpdateStatus::DetachedUnpinned,
            branch,
            before_sha: Some(before.clone()),
            after_sha: Some(before),
            upstream_sha: None,
            upstream: None,
            pinned_ref: None,
            used_pinned_ref: false,
        };
        return Err(terminal_dependency_error(local_path, &freshness, None));
    }

    if let Some(pinned_ref) = pinned_ref.filter(|value| !value.trim().is_empty()) {
        return Ok(DependencyFreshness {
            status: DependencyUpdateStatus::PinnedRef,
            branch,
            before_sha: Some(before.clone()),
            after_sha: Some(before),
            upstream_sha: None,
            upstream: None,
            pinned_ref: Some(pinned_ref.to_string()),
            used_pinned_ref: true,
        });
    }

    let upstream = match git_output(
        local_path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    ) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            let freshness = DependencyFreshness {
                status: DependencyUpdateStatus::NoUpstream,
                branch,
                before_sha: Some(before.clone()),
                after_sha: Some(before),
                upstream_sha: None,
                upstream: None,
                pinned_ref: None,
                used_pinned_ref: false,
            };
            return Err(terminal_dependency_error(local_path, &freshness, None));
        }
    };
    let remote = upstream.split('/').next().unwrap_or("").trim();
    if remote.is_empty() || remote == upstream {
        let freshness = DependencyFreshness {
            status: DependencyUpdateStatus::NoUpstream,
            branch,
            before_sha: Some(before.clone()),
            after_sha: Some(before),
            upstream_sha: None,
            upstream: Some(upstream),
            pinned_ref: None,
            used_pinned_ref: false,
        };
        return Err(terminal_dependency_error(local_path, &freshness, None));
    }

    let fetch_error = run_git(local_path, &["fetch", "--prune", remote]).err();
    let upstream_head = git_output(local_path, &["rev-parse", "@{u}"]).ok();
    let status = git_output(local_path, &["status", "--porcelain=v1"])?;
    if !status.trim().is_empty() {
        // A dirty working tree (tracked changes and/or untracked files) is
        // refused by default so bench results are reproducible from clean git.
        // With an explicit override the dirty working tree is snapshotted as an
        // overlay: `materialize_snapshot` tars the working directory, so the
        // dirty files travel to the runner verbatim.
        if allow_dirty {
            return Ok(DependencyFreshness {
                status: DependencyUpdateStatus::DirtyOverlayAllowed,
                branch,
                before_sha: Some(before.clone()),
                after_sha: Some(before),
                upstream_sha: upstream_head,
                upstream: Some(upstream),
                pinned_ref: None,
                used_pinned_ref: false,
            });
        }
        let freshness = DependencyFreshness {
            status: DependencyUpdateStatus::DirtyNotUpdated,
            branch,
            before_sha: Some(before.clone()),
            after_sha: Some(before),
            upstream_sha: upstream_head,
            upstream: Some(upstream),
            pinned_ref: None,
            used_pinned_ref: false,
        };
        return Err(terminal_dependency_error(
            local_path,
            &freshness,
            fetch_error,
        ));
    }

    if fetch_error.is_some() && upstream_head.as_deref() == Some(before.as_str()) {
        return Ok(DependencyFreshness {
            status: DependencyUpdateStatus::FetchFailedCachedUpToDate,
            branch,
            before_sha: Some(before.clone()),
            after_sha: Some(before),
            upstream_sha: upstream_head,
            upstream: Some(upstream),
            pinned_ref: None,
            used_pinned_ref: false,
        });
    }

    if fetch_error.is_some() {
        let freshness = DependencyFreshness {
            status: DependencyUpdateStatus::FetchFailed,
            branch,
            before_sha: Some(before.clone()),
            after_sha: Some(before),
            upstream_sha: upstream_head,
            upstream: Some(upstream),
            pinned_ref: None,
            used_pinned_ref: false,
        };
        return Err(terminal_dependency_error(
            local_path,
            &freshness,
            fetch_error,
        ));
    }

    if upstream_head.as_deref().is_some_and(|head| head != before) {
        run_git(local_path, &["merge", "--ff-only", "@{u}"])?;
    }
    let after = git_output(local_path, &["rev-parse", "HEAD"])?;
    let upstream_head = git_output(local_path, &["rev-parse", "@{u}"]).ok();
    let status = if upstream_head.as_deref().is_some_and(|head| head != after) {
        DependencyUpdateStatus::BehindAfterFetch
    } else if before == after {
        DependencyUpdateStatus::UpToDate
    } else {
        DependencyUpdateStatus::FastForwarded
    };
    let freshness = DependencyFreshness {
        status,
        branch,
        before_sha: Some(before),
        after_sha: Some(after),
        upstream_sha: upstream_head,
        upstream: Some(upstream),
        pinned_ref: None,
        used_pinned_ref: false,
    };

    if freshness.status.is_terminal() {
        return Err(terminal_dependency_error(local_path, &freshness, None));
    }

    Ok(freshness)
}

fn terminal_dependency_error(
    local_path: &Path,
    freshness: &DependencyFreshness,
    source_error: Option<Error>,
) -> Error {
    let mut hints = vec![
        "Update, rebase, or clean the dependency checkout before rerunning the Lab proof.".to_string(),
        "Use an explicit pinned ref in the rig/component dependency only when the stale checkout is intentional.".to_string(),
    ];
    if freshness.status == DependencyUpdateStatus::DirtyNotUpdated {
        hints.push(
            "Pass --allow-dirty-lab-workspace to snapshot the dirty working tree (tracked changes and untracked files) as an explicit overlay.".to_string(),
        );
        hints.push(
            "Or make the dirty checkout the primary bench workspace with --path so its working tree is snapshotted directly instead of as a clean git-only rig dependency.".to_string(),
        );
    }
    if let Some(error) = &source_error {
        hints.push(format!("Fetch failure: {}", error.message));
    }
    Error::validation_invalid_argument(
        "rig_component_dependency",
        format!(
            "Lab offload refused stale or ambiguous git dependency `{}` with status `{}`",
            local_path.display(),
            freshness.status.label()
        ),
        Some(
            serde_json::json!({
                "local_path": local_path.display().to_string(),
                "status": freshness.status.label(),
                "branch": freshness.branch.as_deref(),
                "before_sha": freshness.before_sha.as_deref(),
                "after_sha": freshness.after_sha.as_deref(),
                "upstream": freshness.upstream.as_deref(),
                "upstream_sha": freshness.upstream_sha.as_deref(),
                "pinned_ref": freshness.pinned_ref.as_deref(),
                "used_pinned_ref": freshness.used_pinned_ref,
            })
            .to_string(),
        ),
        Some(hints),
    )
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

    use super::{ensure_git_dependency_fresh, DependencyUpdateStatus};

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

        let freshness =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect("auto update");

        assert_eq!(freshness.status, DependencyUpdateStatus::FastForwarded);
        assert_ne!(before, expected);
        assert_eq!(freshness.before_sha.as_deref(), Some(before.as_str()));
        assert_eq!(freshness.after_sha.as_deref(), Some(expected.as_str()));
        assert_eq!(freshness.upstream_sha.as_deref(), Some(expected.as_str()));
        assert_eq!(
            git_output(checkout.path(), &["rev-parse", "HEAD"]),
            expected
        );
    }

    #[test]
    fn dirty_dependency_fails_before_snapshotting() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let before = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        fs::write(checkout.path().join("dirty.txt"), "dirty").expect("write dirty file");
        fixture.commit_file("next.txt", "next");
        fixture.push();

        let err =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect_err("dirty fails");

        assert!(err.message.contains("dirty_not_updated"));
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn detached_dependency_without_pinned_ref_fails() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let head = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        run_git(checkout.path(), &["checkout", "--detach", &head]);

        let err =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect_err("detached fails");

        assert!(err.message.contains("detached_unpinned"));
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn dependency_without_upstream_fails_before_snapshotting() {
        let repo = tempfile::tempdir().expect("repo");
        run_git(repo.path(), &["init", "-b", "main"]);
        run_git(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        run_git(repo.path(), &["config", "user.name", "Homeboy Test"]);
        fs::write(repo.path().join("initial.txt"), "initial").expect("write file");
        run_git(repo.path(), &["add", "initial.txt"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        let err =
            ensure_git_dependency_fresh(repo.path(), None, false).expect_err("no upstream fails");

        assert!(err.message.contains("no_upstream"));
    }

    #[test]
    fn explicit_pinned_ref_allows_detached_dependency() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let head = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        run_git(checkout.path(), &["checkout", "--detach", &head]);

        let freshness =
            ensure_git_dependency_fresh(checkout.path(), Some(&head), false).expect("pinned");

        assert_eq!(freshness.status, DependencyUpdateStatus::PinnedRef);
        assert!(freshness.used_pinned_ref);
        assert_eq!(freshness.pinned_ref.as_deref(), Some(head.as_str()));
    }

    #[test]
    fn fetch_failure_uses_cached_upstream_when_checkout_matches() {
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

        let freshness =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect("cached fallback");

        assert_eq!(
            freshness.status,
            DependencyUpdateStatus::FetchFailedCachedUpToDate
        );
        assert_eq!(freshness.before_sha.as_deref(), Some(before.as_str()));
        assert_eq!(freshness.after_sha.as_deref(), Some(before.as_str()));
        assert_eq!(freshness.upstream_sha.as_deref(), Some(before.as_str()));
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn fetch_failure_is_terminal_when_cached_upstream_differs() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();
        fixture.commit_file("next.txt", "next");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let upstream = git_output(checkout.path(), &["rev-parse", "@{u}"]);
        run_git(checkout.path(), &["reset", "--hard", "HEAD~1"]);
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

        let err =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect_err("fetch fails");

        assert_ne!(before, upstream);
        assert!(err.message.contains("fetch_failed_cached"));
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

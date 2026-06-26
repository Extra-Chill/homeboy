use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::core::error::{Error, Result};

use super::{
    workspace::{
        canonical_workspace_path, effective_snapshot_excludes, git_output, local_snapshot_stats,
        materialize_snapshot, materialize_snapshot_git, snapshot_identity, ByteFileCounts,
        DEFAULT_EXCLUDES,
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
    #[serde(flatten)]
    pub counts: ByteFileCounts,
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
    // The default snapshot excludes strip `.git`/`.git/**`, so a plain
    // `materialize_snapshot` lands a runner-side component path with NO git
    // provenance (no HEAD, no refs). Canonical trace preflight probes the
    // materialized component path for git provenance and rejects it as
    // `not-git` before the workload starts (#4314). When the source checkout is
    // a real git worktree, seed a synthetic git checkout on the runner so the
    // materialized path carries canonical provenance (a committed HEAD at the
    // snapshot identity, with the source commit recorded), letting trace
    // preflight accept it. A non-git source has no provenance to preserve, so it
    // keeps the plain snapshot.
    if freshness.status == DependencyUpdateStatus::NotGit {
        materialize_snapshot(runner, &local_path, &options.remote_path, &excludes)?;
    } else {
        materialize_snapshot_git(
            runner,
            &local_path,
            &options.remote_path,
            &excludes,
            &snapshot,
        )?;
    }

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
        counts: stats,
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

impl DependencyFreshness {
    /// Build a freshness record for the common (non-pinned) case where both the
    /// before/after SHA derive from the same `HEAD` snapshot and no pinned ref is
    /// in play. Centralizes the repeated struct literal so each call site only
    /// names the parts that actually vary (status, branch, upstream metadata).
    fn at_head(
        status: DependencyUpdateStatus,
        branch: Option<String>,
        before: &str,
        upstream_sha: Option<String>,
        upstream: Option<String>,
    ) -> Self {
        DependencyFreshness {
            status,
            branch,
            before_sha: Some(before.to_string()),
            after_sha: Some(before.to_string()),
            upstream_sha,
            upstream,
            pinned_ref: None,
            used_pinned_ref: false,
        }
    }
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
        let freshness = DependencyFreshness::at_head(
            DependencyUpdateStatus::DetachedUnpinned,
            branch,
            &before,
            None,
            None,
        );
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
            let freshness = DependencyFreshness::at_head(
                DependencyUpdateStatus::NoUpstream,
                branch,
                &before,
                None,
                None,
            );
            return Err(terminal_dependency_error(local_path, &freshness, None));
        }
    };
    let remote = upstream.split('/').next().unwrap_or("").trim();
    if remote.is_empty() || remote == upstream {
        let freshness = DependencyFreshness::at_head(
            DependencyUpdateStatus::NoUpstream,
            branch,
            &before,
            None,
            Some(upstream),
        );
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
            return Ok(DependencyFreshness::at_head(
                DependencyUpdateStatus::DirtyOverlayAllowed,
                branch,
                &before,
                upstream_head,
                Some(upstream),
            ));
        }
        let freshness = DependencyFreshness::at_head(
            DependencyUpdateStatus::DirtyNotUpdated,
            branch,
            &before,
            upstream_head,
            Some(upstream),
        );
        return Err(terminal_dependency_error(
            local_path,
            &freshness,
            fetch_error,
        ));
    }

    if fetch_error.is_some() && upstream_head.as_deref() == Some(before.as_str()) {
        return Ok(DependencyFreshness::at_head(
            DependencyUpdateStatus::FetchFailedCachedUpToDate,
            branch,
            &before,
            upstream_head,
            Some(upstream),
        ));
    }

    if fetch_error.is_some() {
        let freshness = DependencyFreshness::at_head(
            DependencyUpdateStatus::FetchFailed,
            branch,
            &before,
            upstream_head,
            Some(upstream),
        );
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
    if freshness.status == DependencyUpdateStatus::DetachedUnpinned {
        hints.push(format!(
            "Use a branch-backed dependency checkout before Lab offload: git -C {} switch <branch>",
            shell_arg(&local_path.display().to_string())
        ));
        hints.push(
            "If this detached checkout is the component under test, create/select a branch-backed worktree and rerun the rig proof with --path <component-path>.".to_string(),
        );
        hints.push(
            "If the detached commit is intentional and reviewable, pin the rig component dependency with an explicit ref.".to_string(),
        );
    }
    if freshness.status == DependencyUpdateStatus::NoUpstream {
        hints.push(format!(
            "Set an upstream for the dependency branch before Lab offload: git -C {} branch --set-upstream-to=<remote>/<branch>",
            shell_arg(&local_path.display().to_string())
        ));
        hints.push(
            "Or use a branch-backed worktree with an upstream and pass it as the rig component --path when it is the component under test.".to_string(),
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

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
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

    use super::{
        ensure_git_dependency_fresh, materialize_git_dependency, DependencyUpdateStatus,
        RunnerGitDependencyMaterializationOptions,
    };

    #[test]
    fn materialized_git_dependency_preserves_canonical_git_provenance() {
        // Regression for #4314: the default snapshot excludes strip `.git`, so a
        // plain snapshot lands a runner-side component path with no git
        // provenance and canonical trace preflight rejects it as `not-git`
        // before the workload starts. Materializing a real git checkout must
        // seed a synthetic git checkout on the runner so the materialized path
        // is a valid git work tree with a committed HEAD.
        crate::test_support::with_isolated_home(|_| {
            let fixture = GitFixture::new();
            fixture.commit_file("initial.txt", "initial");
            fixture.push();
            let checkout = fixture.clone_checkout();

            let runner_root = tempfile::tempdir().expect("runner root");
            crate::core::runner::create(
                &format!(
                    r#"{{"id":"lab-local-git-dependency","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");
            let runner =
                crate::core::runner::load("lab-local-git-dependency").expect("load runner");

            let remote_path = runner_root
                .path()
                .join("materialized-dependency")
                .display()
                .to_string();
            let output = materialize_git_dependency(
                &runner,
                RunnerGitDependencyMaterializationOptions {
                    local_path: checkout.path().display().to_string(),
                    remote_path: remote_path.clone(),
                    remote_url: None,
                    required_subpath: None,
                    pinned_ref: None,
                    allow_dirty: false,
                },
            )
            .expect("materialize git dependency");

            assert_eq!(output.remote_path, remote_path);
            let remote = Path::new(&remote_path);
            // Canonical provenance preserved: the materialized path is a real git
            // work tree with a resolvable HEAD, so trace preflight no longer
            // rejects it as `not-git`.
            assert_eq!(
                git_output(remote, &["rev-parse", "--is-inside-work-tree"]),
                "true"
            );
            assert!(!git_output(remote, &["rev-parse", "HEAD"]).is_empty());
            // Working tree is a clean committed snapshot, not a dirty checkout.
            assert!(git_output(remote, &["status", "--porcelain=v1"]).is_empty());
        });
    }

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
        let hints = err.details["tried"]
            .as_array()
            .expect("tried hints")
            .iter()
            .filter_map(|hint| hint.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(hints.contains("git -C"));
        assert!(hints.contains("switch <branch>"));
        assert!(hints.contains("--path <component-path>"));
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

//! The `git.push` release step: push the release branch and tags, with
//! automatic recovery from a non-fast-forward rejection when the remote branch
//! advanced after the release commit/tag were created (issue #3611).
//!
//! Split out of `executor.rs` to keep the step module focused.

use crate::core::component::Component;
use crate::core::error::{Error, Result};

use super::super::types::ReleaseStepResult;
use super::{step_failed, step_success};

pub(crate) fn run_git_push(
    component: &Component,
    component_id: &str,
    release_tag: Option<&str>,
) -> Result<ReleaseStepResult> {
    let branch = crate::core::git::current_branch(std::path::Path::new(&component.local_path))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "branch",
                "Release push requires a checked-out branch",
                Some(component.local_path.clone()),
                Some(vec![
                    "Check out the release branch before running `homeboy release`.".to_string(),
                ]),
            )
        })?;
    let output = git_push_release_branch(component, component_id, &branch)?;
    let data = serde_json::to_value(&output)
        .map_err(|e| Error::internal_json(e.to_string(), Some("git push output".to_string())))?;

    if output.success {
        return Ok(step_success("git.push", "git.push", Some(data), Vec::new()));
    }

    // The branch push was rejected. When the remote branch advanced after the
    // release commit + tag were created (issue #3611), git rejects the branch
    // ref as non-fast-forward — typically leaving the tag pushed but the branch
    // behind. Attempt a clean, non-force recovery: fetch, rebase the release
    // commit onto the advanced remote head, and re-push the branch.
    if is_non_fast_forward_rejection(&output.stderr) {
        match recover_advanced_remote_push(component, component_id, &branch, release_tag) {
            Ok(Some(recovered)) => {
                let recovered_data = serde_json::to_value(&recovered).map_err(|e| {
                    Error::internal_json(e.to_string(), Some("git push output".to_string()))
                })?;
                log_status!(
                    "release",
                    "Remote {} advanced during release — rebased the release commit onto the new head and re-pushed.",
                    branch
                );
                return Ok(step_success(
                    "git.push",
                    "git.push",
                    Some(serde_json::json!({
                        "success": true,
                        "recovered": "advanced-remote-rebased",
                        "branch": branch,
                        "push": recovered_data,
                    })),
                    Vec::new(),
                ));
            }
            Ok(None) => {
                // Recovery was not safe to perform automatically; fall through
                // to the failure path with explicit recovery guidance.
            }
            Err(recover_err) => {
                log_status!(
                    "release",
                    "⚠ Automatic recovery from advanced remote failed: {}",
                    recover_err
                );
            }
        }

        let error = push_error_message(&output);
        return Ok(step_failed(
            "git.push",
            "git.push",
            Some(data),
            Some(error),
            non_fast_forward_recovery_hints(component_id, &branch),
        ));
    }

    let error = push_error_message(&output);
    Ok(step_failed(
        "git.push",
        "git.push",
        Some(data),
        Some(error),
        Vec::new(),
    ))
}

/// Push the release branch (and tags) to `origin`.
fn git_push_release_branch(
    component: &Component,
    component_id: &str,
    branch: &str,
) -> Result<crate::core::git::GitOutput> {
    crate::core::git::push_at(
        Some(component_id),
        crate::core::git::PushOptions {
            tags: true,
            force_with_lease: false,
            refspec: Some(format!("HEAD:refs/heads/{branch}")),
            ..Default::default()
        },
        Some(&component.local_path),
    )
}

/// Recover from a non-fast-forward branch rejection caused by the remote
/// advancing after the release commit/tag were created (issue #3611, #5502).
///
/// Fetches `origin`, confirms the local branch is strictly ahead of a common
/// ancestor (so a rebase is the right reconciliation, not a force-push over
/// divergent history), rebases HEAD onto the advanced remote head, and re-pushes
/// the branch.
///
/// Rebasing replays the release commit onto the new remote head, producing a
/// *new* release commit object. The annotated release tag was created in the
/// earlier `git.tag` step pointing at the original (pre-rebase) release commit,
/// which is now orphaned off the branch line. If left untouched, the tag points
/// at a commit that is NOT an ancestor of the pushed branch, and the next
/// release sees a stranded duplicate-version commit (issue #5502). So after a
/// successful rebase this re-creates the tag at the new HEAD and force-pushes
/// it, keeping exactly one release commit on the branch and the tag always an
/// ancestor of the pushed branch.
///
/// Returns:
/// - `Ok(Some(push_output))` when the rebase + re-push succeeded,
/// - `Ok(None)` when automatic recovery is unsafe (e.g. rebase conflict, or the
///   remote branch is unexpectedly gone) — the caller emits manual guidance,
/// - `Err(_)` on an unexpected git failure.
fn recover_advanced_remote_push(
    component: &Component,
    component_id: &str,
    branch: &str,
    release_tag: Option<&str>,
) -> Result<Option<crate::core::git::GitOutput>> {
    let path = &component.local_path;
    crate::core::git::fetch_origin(path)?;

    let Some(remote_commit) = crate::core::git::remote_branch_commit(path, branch)? else {
        // The branch is not on the remote at all — non-fast-forward against a
        // missing branch is unexpected; don't guess, defer to manual recovery.
        return Ok(None);
    };
    let head_commit = crate::core::git::get_head_commit(path)?;

    // Already reconciled (e.g. a retry after a manual fix): nothing to do.
    if remote_commit == head_commit {
        return git_push_release_branch(component, component_id, branch).map(Some);
    }

    // Only rebase when the remote head is NOT already contained in HEAD — if it
    // were, the push would have fast-forwarded. Confirm the histories share an
    // ancestor before rebasing so we never replay onto unrelated history.
    if crate::core::git::is_ancestor(path, &remote_commit, &head_commit)? {
        // Remote head is an ancestor of HEAD; the rejection was spurious or
        // already resolved. Re-push directly.
        return git_push_release_branch(component, component_id, branch).map(Some);
    }

    log_status!(
        "release",
        "Rebasing release commit onto advanced remote {} ({})...",
        branch,
        &remote_commit[..remote_commit.len().min(8)]
    );
    let rebase = crate::core::git::rebase_at(
        Some(component_id),
        crate::core::git::RebaseOptions {
            onto: Some(remote_commit.clone()),
            ..Default::default()
        },
        Some(path),
    )?;
    if !rebase.success {
        // Conflicting rebase — abort to leave the tree clean and defer to the
        // operator. Recovery is not safe to automate here.
        let _ = crate::core::git::rebase_at(
            Some(component_id),
            crate::core::git::RebaseOptions {
                abort: true,
                ..Default::default()
            },
            Some(path),
        );
        return Ok(None);
    }

    // The rebase moved the release commit to a new SHA on the branch line. Move
    // the release tag onto the rebased release commit so it stays an ancestor of
    // the pushed branch (issue #5502). Do this BEFORE pushing the branch so a
    // successful branch push is never left with a stranded, off-branch tag.
    if let Some(tag_name) = release_tag {
        retag_rebased_release(component, component_id, branch, tag_name)?;
    }

    git_push_release_branch(component, component_id, branch).map(Some)
}

/// Re-point the release tag at the post-rebase HEAD (the release commit that is
/// now on the branch line) and force-push the tag.
///
/// After [`recover_advanced_remote_push`] rebases the release commit onto the
/// advanced remote head, the original tagged commit is orphaned off-branch. This
/// deletes the stale local tag, recreates the annotated tag at HEAD, and
/// force-pushes only the tag ref — guaranteeing the tag is an ancestor of the
/// pushed branch and that no second release commit with the same version is left
/// stranded (issue #5502). The branch itself is never force-pushed.
fn retag_rebased_release(
    component: &Component,
    component_id: &str,
    branch: &str,
    tag_name: &str,
) -> Result<()> {
    let path = &component.local_path;
    let head_commit = crate::core::git::get_head_commit(path)?;

    // If the tag already points at the rebased HEAD there is nothing to move.
    if crate::core::git::tag_exists_locally(path, tag_name).unwrap_or(false) {
        let current = crate::core::git::get_tag_commit(path, tag_name)?;
        if current == head_commit {
            return Ok(());
        }
        crate::core::git::delete_local_tag(path, tag_name)?;
    }

    log_status!(
        "release",
        "Moving release tag {} onto the rebased release commit on {} ({})...",
        tag_name,
        branch,
        &head_commit[..head_commit.len().min(8)]
    );

    let message = format!("Release {}", tag_name);
    let tag_output = crate::core::git::tag_at(
        Some(component_id),
        Some(tag_name),
        Some(&message),
        Some(path),
    )?;
    if !tag_output.success {
        return Err(Error::git_command_failed(format!(
            "Failed to recreate release tag {} at the rebased HEAD: {}",
            tag_name,
            tag_output.stderr.trim()
        )));
    }

    // Re-publish the tag ref only (never the branch). If the orphaned tag was
    // already pushed (the initial `--follow-tags` push lands the tag even when
    // the branch is rejected), delete it on the remote first so the fresh tag
    // publishes as a clean, non-forced update. Tags are deliberately moved here:
    // the rebased commit supersedes the orphaned one within the same release.
    if crate::core::git::tag_exists_on_remote(path, tag_name).unwrap_or(false) {
        let delete = crate::core::git::delete_remote_tag(path, tag_name)?;
        if !delete.success {
            return Err(Error::git_command_failed(format!(
                "Failed to delete the orphaned remote release tag {} before retagging: {}",
                tag_name,
                delete.stderr.trim()
            )));
        }
    }

    let push = crate::core::git::push_at(
        Some(component_id),
        crate::core::git::PushOptions {
            refspec: Some(format!("refs/tags/{tag_name}:refs/tags/{tag_name}")),
            ..Default::default()
        },
        Some(path),
    )?;
    if !push.success {
        return Err(Error::git_command_failed(format!(
            "Failed to push the moved release tag {}: {}",
            tag_name,
            push.stderr.trim()
        )));
    }

    Ok(())
}

/// True when git's stderr indicates a non-fast-forward / stale-remote branch
/// rejection — the signature of the advanced-remote race in issue #3611.
pub(crate) fn is_non_fast_forward_rejection(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("[rejected]")
        || lower.contains("non-fast-forward")
        || lower.contains("fetch first")
        || lower.contains("tip of your current branch is behind")
        || lower.contains("updates were rejected")
}

fn push_error_message(output: &crate::core::git::GitOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    "git push failed".to_string()
}

/// Hints emitted when the branch push was rejected as non-fast-forward and
/// automatic recovery did not complete. They give the operator a deterministic,
/// non-force recovery path (issue #3611).
fn non_fast_forward_recovery_hints(
    component_id: &str,
    branch: &str,
) -> Vec<crate::core::error::Hint> {
    vec![
        crate::core::error::Hint {
            message: format!(
                "Remote '{}' advanced after the release commit/tag were created. The tag may already be pushed; the branch was rejected as non-fast-forward.",
                branch
            ),
        },
        crate::core::error::Hint {
            message: format!(
                "Reconcile and finish the release without re-tagging or force-pushing: homeboy release {} --recover",
                component_id
            ),
        },
        crate::core::error::Hint {
            message: format!(
                "Or resolve manually: git fetch origin && git rebase origin/{branch} && git push origin HEAD:{branch}",
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{is_non_fast_forward_rejection, run_git_push};
    use crate::core::component::Component;
    use crate::core::release::types::ReleaseStepStatus;
    use std::process::Command;

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn git_push_step_fails_when_git_push_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let init = Command::new("git")
            .arg("init")
            .current_dir(temp.path())
            .output()
            .expect("git init");
        assert!(init.status.success());

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        let result =
            run_git_push(&component, "fixture", None).expect("push step should return result");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(!result.error.unwrap().trim().is_empty());
        assert_eq!(
            result
                .data
                .and_then(|data| data.get("success").and_then(serde_json::Value::as_bool)),
            Some(false)
        );
    }

    #[test]
    fn test_run_git_push_without_upstream() {
        let local = tempfile::tempdir().expect("local tempdir");
        let remote = tempfile::tempdir().expect("remote tempdir");
        git(remote.path(), &["init", "--bare"]);
        git(local.path(), &["init"]);
        git(local.path(), &["checkout", "-b", "main"]);
        git(local.path(), &["config", "user.name", "Homeboy Test"]);
        git(
            local.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        git(
            local.path(),
            &[
                "remote",
                "add",
                "origin",
                remote.path().to_str().expect("remote path"),
            ],
        );
        std::fs::write(local.path().join("release.txt"), "release").expect("write fixture");
        git(local.path(), &["add", "release.txt"]);
        git(local.path(), &["commit", "-m", "release: v1.0.0"]);
        git(
            local.path(),
            &["tag", "-a", "v1.0.0", "-m", "Release v1.0.0"],
        );

        let upstream = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
            .current_dir(local.path())
            .output()
            .expect("check upstream");
        assert!(
            !upstream.status.success(),
            "fixture should not have upstream"
        );

        let component = Component {
            id: "fixture".to_string(),
            local_path: local.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        let result = run_git_push(&component, "fixture", Some("v1.0.0"))
            .expect("push step should return result");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        git(remote.path(), &["show-ref", "--verify", "refs/heads/main"]);
        git(remote.path(), &["show-ref", "--verify", "refs/tags/v1.0.0"]);
    }

    #[test]
    fn test_is_non_fast_forward_rejection() {
        // The exact shape of git's stderr from issue #3611's failed push.
        let stderr = " ! [rejected]        HEAD -> main (fetch first)\n\
            error: failed to push some refs to 'https://github.com/owner/repo.git'\n\
            hint: Updates were rejected because the remote contains work that you do not\n\
            hint: have locally.";
        assert!(is_non_fast_forward_rejection(stderr));
        assert!(is_non_fast_forward_rejection("hint: non-fast-forward"));
        assert!(is_non_fast_forward_rejection(
            "Updates were rejected because the tip of your current branch is behind"
        ));
        // Unrelated failures must not trigger the rebase-recovery path.
        assert!(!is_non_fast_forward_rejection(
            "fatal: Authentication failed"
        ));
        assert!(!is_non_fast_forward_rejection(""));
    }

    /// Issue #3611: when the remote branch advances after the release commit and
    /// tag are created, `run_git_push` must rebase the release commit onto the
    /// advanced remote head and re-push — without force-pushing or re-tagging.
    #[test]
    fn run_git_push_recovers_when_remote_advanced() {
        let remote = tempfile::tempdir().expect("remote tempdir");
        let other = tempfile::tempdir().expect("other clone tempdir");
        let local = tempfile::tempdir().expect("local tempdir");
        git(remote.path(), &["init", "--bare", "-b", "main"]);

        let setup_identity = |dir: &std::path::Path| {
            git(dir, &["config", "user.name", "Homeboy Test"]);
            git(dir, &["config", "user.email", "homeboy@example.test"]);
            git(dir, &["config", "commit.gpgsign", "false"]);
        };

        // Seed the remote with an initial commit via the "other" clone.
        git(
            other.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        setup_identity(other.path());
        std::fs::write(other.path().join("base.txt"), "base").unwrap();
        git(other.path(), &["add", "."]);
        git(other.path(), &["commit", "-m", "base"]);
        git(other.path(), &["push", "origin", "main"]);

        // The release clone starts from that base.
        git(
            local.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        setup_identity(local.path());

        // The remote advances AFTER the release clone was made.
        std::fs::write(other.path().join("advance.txt"), "advance").unwrap();
        git(other.path(), &["add", "."]);
        git(other.path(), &["commit", "-m", "remote advance"]);
        git(other.path(), &["push", "origin", "main"]);

        // The release commit + tag are created locally (mirroring the release
        // pipeline state right before the racing push).
        std::fs::write(local.path().join("release.txt"), "release").unwrap();
        git(local.path(), &["add", "."]);
        git(local.path(), &["commit", "-m", "release: v1.0.0"]);
        git(
            local.path(),
            &["tag", "-a", "v1.0.0", "-m", "Release v1.0.0"],
        );

        let component = Component {
            id: "fixture".to_string(),
            local_path: local.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        let result = run_git_push(&component, "fixture", Some("v1.0.0"))
            .expect("push step returns a result");

        assert_eq!(
            result.status,
            ReleaseStepStatus::Success,
            "push should recover from the advanced remote: {:?}",
            result.error
        );
        assert_eq!(
            result
                .data
                .as_ref()
                .and_then(|d| d.get("recovered").and_then(serde_json::Value::as_str)),
            Some("advanced-remote-rebased")
        );

        // The remote main now contains BOTH the remote advance and the release
        // commit (the release commit was rebased on top), and the tag is pushed.
        git(remote.path(), &["show-ref", "--verify", "refs/tags/v1.0.0"]);
        // Refresh remote-tracking ref first.
        git(local.path(), &["fetch", "origin"]);
        let log = Command::new("git")
            .args(["log", "--format=%s", "origin/main"])
            .current_dir(local.path())
            .output()
            .expect("git log");
        let subjects = String::from_utf8_lossy(&log.stdout);
        assert!(
            subjects.contains("release: v1.0.0"),
            "remote main must contain the release commit, got: {}",
            subjects
        );
        assert!(
            subjects.contains("remote advance"),
            "remote main must retain the advance commit (no force-push), got: {}",
            subjects
        );

        // Issue #5502: the tag must follow the rebased release commit so it is an
        // ancestor of the pushed branch — not stranded on the orphaned original.
        let head = rev(local.path(), "origin/main");
        let tag_commit = rev(local.path(), "v1.0.0^{commit}");
        assert_eq!(
            tag_commit, head,
            "tag v1.0.0 must point at the rebased release commit on origin/main"
        );
        // And the same on the remote itself (the moved tag was force-pushed).
        let remote_tag = rev(remote.path(), "v1.0.0^{commit}");
        let remote_head = rev(remote.path(), "main");
        assert_eq!(
            remote_tag, remote_head,
            "remote tag v1.0.0 must point at remote main's HEAD (the release commit)"
        );

        // The ancestry invariant deploy relies on must hold.
        let is_ancestor = Command::new("git")
            .args(["merge-base", "--is-ancestor", "v1.0.0", "origin/main"])
            .current_dir(local.path())
            .status()
            .expect("merge-base");
        assert!(
            is_ancestor.success(),
            "tag v1.0.0 must be an ancestor of origin/main (deploy ancestry invariant)"
        );

        // Exactly one release commit exists on the branch line (no duplicate).
        let release_commits = subjects
            .lines()
            .filter(|s| s.trim() == "release: v1.0.0")
            .count();
        assert_eq!(
            release_commits, 1,
            "exactly one release commit must exist on the branch, got: {}",
            subjects
        );
    }

    fn rev(dir: &std::path::Path, refname: &str) -> String {
        let output = Command::new("git")
            .args(["rev-parse", refname])
            .current_dir(dir)
            .output()
            .expect("git rev-parse");
        assert!(
            output.status.success(),
            "git rev-parse {} failed: {}",
            refname,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

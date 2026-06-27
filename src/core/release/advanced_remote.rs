//! Shared advanced-remote reconciliation for the release push step and
//! `release --recover` (issues #3611, #5502, #6141).
//!
//! When the remote release branch advances after the release commit/tag were
//! created, git rejects the branch push as non-fast-forward. Two callers must
//! reconcile that state the same way:
//!
//! - the reactive push step ([`super::executor`]'s `git_push`), which hits the
//!   rejection live, and
//! - the proactive `--recover` flow ([`super::workflow_recover`]), which finds
//!   the same partial state after the fact.
//!
//! Both replay the local release commit onto the advanced remote head and
//! re-push the branch ref — never a force-push over divergent history. This
//! module owns that shared "rebase onto the advanced remote → optionally move
//! the tag → push the branch" mechanic so the two callers cannot drift. Each
//! caller keeps its own pre-checks (how it discovers the divergence) and its own
//! result contract (the push step reports a recovery payload; recover returns a
//! human action string), and maps a rebase conflict ([`Ok(None)`]) onto its own
//! failure path.

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;

/// Outcome of recovering a release push from an advanced remote branch.
///
/// Carries the final push output plus the newly-included commit range that
/// landed on the remote during the release window. The range is surfaced both as
/// a prominent operator warning and in structured step output so downstream
/// summaries can state exactly what changed since the dry-run plan (issue #6141).
pub(crate) struct AdvancedRemoteRecovery {
    pub(crate) push: git::GitOutput,
    pub(crate) advance: ConcurrentAdvance,
}

impl AdvancedRemoteRecovery {
    /// Recovery that did not actually rebase over an advance (already reconciled
    /// or a spurious rejection): there is no newly-included range to report.
    pub(crate) fn without_advance(push: git::GitOutput) -> Self {
        Self {
            push,
            advance: ConcurrentAdvance::default(),
        }
    }
}

/// Describes the commits that landed on the remote branch between the reviewed
/// dry-run/preflight plan and the final tag/notes creation (issue #6141).
#[derive(Default)]
pub(crate) struct ConcurrentAdvance {
    /// The pre-rebase release HEAD — the tip the dry-run plan was built against.
    base: String,
    /// The advanced remote head the release commit was rebased onto.
    remote_head: String,
    /// Commits newly included by the advance (`base..remote_head`), newest first.
    commits: Vec<git::CommitInfo>,
}

impl ConcurrentAdvance {
    fn short(rev: &str) -> &str {
        &rev[..rev.len().min(8)]
    }

    /// True when no actual advance was recorded (e.g. an already-reconciled retry
    /// or a spurious non-fast-forward rejection) — nothing new was auto-included.
    pub(crate) fn is_noop(&self) -> bool {
        self.base.is_empty() && self.remote_head.is_empty()
    }

    /// Structured representation for the step `data` payload so downstream
    /// summaries can report exactly which commits/PRs were auto-included.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "base": self.base,
            "remote_head": self.remote_head,
            "range": format!("{}..{}", Self::short(&self.base), Self::short(&self.remote_head)),
            "count": self.commits.len(),
            "commits": self.commits
                .iter()
                .map(|c| serde_json::json!({ "hash": c.hash, "subject": c.subject }))
                .collect::<Vec<_>>(),
        })
    }

    /// Emit a prominent warning listing the newly-included commits so an operator
    /// reviewing the release sees that the published contents differ from the
    /// reviewed dry-run plan (issue #6141).
    pub(crate) fn warn(&self, branch: &str) {
        log_status!(
            "release",
            "⚠ CONCURRENT MAIN ADVANCE: remote '{}' advanced during the release. \
             The final published release content CHANGED since the dry-run/preflight plan.",
            branch
        );
        log_status!(
            "release",
            "⚠ Auto-included {} commit(s) ({}..{}) not present in the reviewed dry-run plan:",
            self.commits.len(),
            Self::short(&self.base),
            Self::short(&self.remote_head)
        );
        if self.commits.is_empty() {
            log_status!(
                "release",
                "⚠   (remote advanced but no new commits were resolved in the recovered range)"
            );
        } else {
            for commit in &self.commits {
                log_status!("release", "⚠   {} {}", commit.hash, commit.subject);
            }
        }
    }
}

/// Push the release branch (HEAD) and tags to `origin`.
///
/// Shared by both reconciliation paths and the initial release push so the
/// branch refspec (`HEAD:refs/heads/<branch>`) and tag-follow behavior cannot
/// drift between them. The branch is never force-pushed.
pub(crate) fn push_release_branch(
    component: &Component,
    component_id: &str,
    branch: &str,
) -> Result<git::GitOutput> {
    git::push_at(
        Some(component_id),
        git::PushOptions {
            tags: true,
            force_with_lease: false,
            refspec: Some(format!("HEAD:refs/heads/{branch}")),
            ..Default::default()
        },
        Some(&component.local_path),
    )
}

/// Rebase the local release commit onto an already-known advanced remote head,
/// optionally move the release tag onto the rebased commit, and push the branch.
///
/// Callers invoke this only after they have established that the histories
/// diverged (the remote advanced past a shared ancestor); the caller-side
/// ancestor checks are intentionally not duplicated here. The commits the remote
/// gained during the release window are captured for issue #6141 reporting,
/// HEAD is rebased onto `remote_commit`, and — when `retag` names the release
/// tag — the tag is moved onto the rebased commit before the branch is pushed so
/// a successful branch push is never left with a stranded, off-branch tag
/// (issue #5502).
///
/// Returns `Ok(None)` when the rebase conflicts (it is aborted to leave a clean
/// tree) so each caller can map the un-automatable state onto its own failure
/// contract. The returned [`AdvancedRemoteRecovery`] carries the push output;
/// callers that must surface a failed post-rebase push inspect `push.success`.
pub(crate) fn rebase_onto_advanced_remote_and_push(
    component: &Component,
    component_id: &str,
    branch: &str,
    head_commit: &str,
    remote_commit: &str,
    retag: Option<&str>,
) -> Result<Option<AdvancedRemoteRecovery>> {
    let path = &component.local_path;

    // Capture the commits that landed on the remote between the pre-rebase
    // release HEAD (what the dry-run plan was reviewed against) and the advanced
    // remote head. These are auto-included in the final release after the rebase
    // (issue #6141).
    let advance = resolve_concurrent_advance(path, head_commit, remote_commit);

    let rebase = git::rebase_at(
        Some(component_id),
        git::RebaseOptions {
            onto: Some(remote_commit.to_string()),
            ..Default::default()
        },
        Some(path),
    )?;
    if !rebase.success {
        // Conflicting rebase — abort to leave the tree clean and defer to the
        // operator. Recovery is not safe to automate here.
        let _ = git::rebase_at(
            Some(component_id),
            git::RebaseOptions {
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
    if let Some(tag_name) = retag {
        retag_rebased_release(component, component_id, branch, tag_name)?;
    }

    push_release_branch(component, component_id, branch)
        .map(|push| Some(AdvancedRemoteRecovery { push, advance }))
}

/// Resolve the commits that landed on the remote between the pre-rebase release
/// HEAD and the advanced remote head (`base..remote_head`).
///
/// Best-effort: a git failure listing the range must never block the recovery —
/// the advance still gets reported (with an empty commit list) so the operator
/// at least sees the base/head SHAs that changed (issue #6141).
fn resolve_concurrent_advance(path: &str, base: &str, remote_head: &str) -> ConcurrentAdvance {
    let commits = git::get_commits_in_range(path, base, remote_head).unwrap_or_default();
    ConcurrentAdvance {
        base: base.to_string(),
        remote_head: remote_head.to_string(),
        commits,
    }
}

/// Re-point the release tag at the post-rebase HEAD (the release commit that is
/// now on the branch line) and force-push the tag.
///
/// After [`rebase_onto_advanced_remote_and_push`] rebases the release commit
/// onto the advanced remote head, the original tagged commit is orphaned
/// off-branch. This deletes the stale local tag, recreates the annotated tag at
/// HEAD, and force-pushes only the tag ref — guaranteeing the tag is an ancestor
/// of the pushed branch and that no second release commit with the same version
/// is left stranded (issue #5502). The branch itself is never force-pushed.
fn retag_rebased_release(
    component: &Component,
    component_id: &str,
    branch: &str,
    tag_name: &str,
) -> Result<()> {
    let path = &component.local_path;
    let head_commit = git::get_head_commit(path)?;

    // If the tag already points at the rebased HEAD there is nothing to move.
    if git::tag_exists_locally(path, tag_name).unwrap_or(false) {
        let current = git::get_tag_commit(path, tag_name)?;
        if current == head_commit {
            return Ok(());
        }
        git::delete_local_tag(path, tag_name)?;
    }

    log_status!(
        "release",
        "Moving release tag {} onto the rebased release commit on {} ({})...",
        tag_name,
        branch,
        &head_commit[..head_commit.len().min(8)]
    );

    let message = format!("Release {}", tag_name);
    let tag_output = git::tag_at(
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
    if git::tag_exists_on_remote(path, tag_name).unwrap_or(false) {
        let delete = git::delete_remote_tag(path, tag_name)?;
        if !delete.success {
            return Err(Error::git_command_failed(format!(
                "Failed to delete the orphaned remote release tag {} before retagging: {}",
                tag_name,
                delete.stderr.trim()
            )));
        }
    }

    let push = git::push_at(
        Some(component_id),
        git::PushOptions {
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

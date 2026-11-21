//! Git worktrees that exist on disk with no task-worktree record (#16310).
//!
//! A local-placement Cook, an older Homeboy, or an operator can leave a linked
//! worktree that this host's data root never recorded. Before this module,
//! every lifecycle command answered "No task-worktree record exists" for such a
//! path, so a merged worktree could only be reclaimed with a manual
//! `git worktree remove`. Here an unrecorded worktree is classified as an
//! orphan and becomes removable once Git itself proves nothing would be lost:
//!
//! - the tree is clean (no tracked changes and no untracked, non-ignored files);
//! - `HEAD` is reachable from a remote-tracking ref, or its content is already
//!   contained in the remote default branch (a squash or rebase merge that
//!   deleted the branch leaves no remote ref to compare against);
//! - no stash entry was recorded on its branch or at its `HEAD`.
//!
//! `--force` never relaxes these checks: with no record there is no lifecycle
//! owner whose authority could vouch for discarding work.

use super::reaping;
use super::*;
use homeboy_engine_primitives::shell::shell_arg;

/// Liveness classification reported for an unrecorded worktree.
pub const UNRECORDED_WORKTREE_LIVENESS: &str = "orphan_unrecorded_worktree";

/// Facts Git reports about one unrecorded linked worktree.
#[derive(Debug, Clone)]
pub(super) struct UnrecordedWorktree {
    pub worktree: PathBuf,
    pub primary_checkout: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    /// Remote branch used for squash/rebase containment, when one resolves.
    pub default_remote_ref: Option<String>,
}

/// Resolve `id` as a path to an existing linked Git worktree root. Returns
/// `None` when `id` is not an existing directory or not a worktree root, so
/// callers keep answering with the ordinary "no record" error.
pub(super) fn discover(
    id: &str,
    deadline: Option<std::time::Instant>,
) -> Result<Option<UnrecordedWorktree>> {
    let path = Path::new(id);
    if !path.is_dir() {
        return Ok(None);
    }
    let Ok(worktree) = path.canonicalize() else {
        return Ok(None);
    };
    let Ok(rev_parse) = run_inventory_git_until(
        &worktree,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ],
        "git rev-parse unrecorded worktree",
        deadline,
    ) else {
        return Ok(None);
    };
    let mut lines = rev_parse.lines().map(str::trim);
    let (Some(toplevel), Some(git_dir), Some(common_dir)) =
        (lines.next(), lines.next(), lines.next())
    else {
        return Ok(None);
    };
    let toplevel = canonical_or_raw(Path::new(toplevel));
    if toplevel != worktree {
        // A subdirectory of a checkout is not a worktree handle.
        return Ok(None);
    }
    let Some(primary_checkout) = primary_checkout_until(&worktree, deadline)? else {
        return Ok(None);
    };
    if canonical_or_raw(Path::new(git_dir)) == canonical_or_raw(Path::new(common_dir))
        || primary_checkout == worktree
    {
        // A primary checkout is never a lifecycle candidate; keep answering
        // with the ordinary "no record" error for it.
        return Ok(None);
    }
    let branch = run_inventory_git_until(
        &worktree,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        "git symbolic-ref",
        deadline,
    )
    .ok()
    .map(|branch| branch.trim().to_string())
    .filter(|branch| !branch.is_empty());
    let head = run_inventory_git_until(
        &worktree,
        &["rev-parse", "--verify", "HEAD"],
        "git rev-parse HEAD",
        deadline,
    )
    .map(|head| head.trim().to_string())
    .unwrap_or_default();
    let default_remote_ref = default_remote_ref_until(&worktree, deadline);
    Ok(Some(UnrecordedWorktree {
        worktree,
        primary_checkout,
        branch,
        head,
        default_remote_ref,
    }))
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The first `worktree` entry of `git worktree list --porcelain` is always the
/// primary checkout.
fn primary_checkout_until(
    worktree: &Path,
    deadline: Option<std::time::Instant>,
) -> Result<Option<PathBuf>> {
    let Ok(listing) = run_inventory_git_until(
        worktree,
        &["worktree", "list", "--porcelain"],
        "git worktree list",
        deadline,
    ) else {
        return Ok(None);
    };
    Ok(listing
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .map(|path| canonical_or_raw(Path::new(path.trim()))))
}

fn default_remote_ref_until(
    worktree: &Path,
    deadline: Option<std::time::Instant>,
) -> Option<String> {
    if let Ok(symbolic) = run_inventory_git_until(
        worktree,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        "git symbolic-ref origin/HEAD",
        deadline,
    ) {
        let symbolic = symbolic.trim();
        if !symbolic.is_empty() {
            return Some(symbolic.to_string());
        }
    }
    ["main", "master", "trunk"].iter().find_map(|name| {
        let candidate = format!("refs/remotes/origin/{name}");
        run_inventory_git_until(
            worktree,
            &["rev-parse", "--verify", "--quiet", &candidate],
            "git rev-parse default remote branch",
            deadline,
        )
        .ok()
        .map(|_| candidate)
    })
}

impl UnrecordedWorktree {
    fn is_primary(&self) -> bool {
        self.worktree == self.primary_checkout
    }

    /// A synthetic record describing the unrecorded worktree, so status and
    /// removal keep their existing output contracts. It is never persisted.
    pub(super) fn synthetic_record(&self, state: TaskWorktreeState) -> TaskWorktreeRecord {
        let component_id = self
            .primary_checkout
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        TaskWorktreeRecord {
            id: self.worktree.display().to_string(),
            component_id,
            source_checkout: self.primary_checkout.display().to_string(),
            worktree_path: self.worktree.display().to_string(),
            branch: self.branch.clone().unwrap_or_else(|| "HEAD".to_string()),
            base_ref: self
                .default_remote_ref
                .as_deref()
                .map(|reference| {
                    reference
                        .strip_prefix("refs/remotes/")
                        .unwrap_or(reference)
                        .to_string()
                })
                .unwrap_or_default(),
            workspace_identity: None,
            task_url: None,
            run_id: None,
            cleanup_policy: CleanupPolicy::RemoveWhenSafe,
            terminal_disposition: Some(UNRECORDED_WORKTREE_LIVENESS.to_string()),
            branch_cleanup_intent: BranchCleanupIntent::Preserve,
            created_at: chrono::Utc::now().to_rfc3339(),
            state,
            lifecycle_revision: 0,
            terminal_workspace_authority: None,
        }
    }

    /// Safety report for an unrecorded worktree. `safe` means every check in
    /// the module docs passed.
    pub(super) fn safety_report(
        &self,
        deadline: Option<std::time::Instant>,
    ) -> Result<WorktreeSafetyReport> {
        let mut reasons = vec![format!(
            "{UNRECORDED_WORKTREE_LIVENESS}: no task-worktree record exists for {}",
            self.worktree.display()
        )];
        let primary_checkout = self.is_primary();
        let path_contained = !primary_checkout
            && self
                .primary_checkout
                .parent()
                .is_some_and(|parent| self.worktree.starts_with(parent));
        if primary_checkout {
            reasons.push("refuses to remove primary checkout".to_string());
            return Ok(WorktreeSafetyReport {
                dirty: false,
                unpushed_commits: 0,
                primary_checkout,
                path_contained,
                worktree_missing: false,
                source_checkout_missing: false,
                safe: false,
                reasons,
            });
        }
        let mut blockers = Vec::new();
        if crate::cleanup::cargo_build_lock_is_held(&self.worktree) {
            blockers.push("worktree has an active Cargo build".to_string());
        }
        if !path_contained {
            blockers.push("worktree path is outside the component checkout parent".to_string());
        }
        let status = run_inventory_git_until(
            &self.worktree,
            &["status", "--porcelain=v1"],
            "git status",
            deadline,
        )?;
        let dirty = !status.trim().is_empty();
        if dirty {
            blockers.push("dirty worktree".to_string());
        }
        let unpushed_commits = if self.head.is_empty() {
            blockers.push("worktree HEAD does not resolve to a commit".to_string());
            0
        } else {
            self.unpushed_commits(deadline)?
        };
        if unpushed_commits > 0 {
            blockers.push(format!(
                "{unpushed_commits} commit(s) not reachable from any remote ref and not contained in {}",
                self.default_remote_ref
                    .as_deref()
                    .unwrap_or("a remote default branch")
            ));
        }
        if let Some(stash) = self.matching_stash(deadline)? {
            blockers.push(format!("stash entry belongs to this worktree: {stash}"));
        }
        let caller_cwd =
            std::env::current_dir().map_err(|error| Error::internal_io(error.to_string(), None))?;
        let caller_cwd = canonical_or_raw(&caller_cwd);
        if caller_cwd.starts_with(&self.worktree) {
            blockers.push(LIVE_CWD_REASON.to_string());
        }
        let safe = blockers.is_empty();
        reasons.extend(blockers);
        if safe {
            reasons.push(format!(
                "reclaimable: clean, nothing stashed, and HEAD is contained upstream; remove with `homeboy worktree remove {}`",
                shell_arg(&self.worktree.display().to_string())
            ));
        }
        Ok(WorktreeSafetyReport {
            dirty,
            unpushed_commits,
            primary_checkout,
            path_contained,
            worktree_missing: false,
            source_checkout_missing: false,
            safe,
            reasons,
        })
    }

    /// Commits on `HEAD` that no remote-tracking ref reaches, or zero when the
    /// remote default branch already contains `HEAD`'s content.
    fn unpushed_commits(&self, deadline: Option<std::time::Instant>) -> Result<u32> {
        let count = run_inventory_git_until(
            &self.worktree,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
            "git rev-list unpushed",
            deadline,
        )?;
        let count = count.trim().parse::<u32>().unwrap_or(u32::MAX);
        if count == 0 {
            return Ok(0);
        }
        if let Some(target) = self.default_remote_ref.as_deref() {
            if branch_content_merged_until(&self.worktree, "HEAD", target, deadline)
                .unwrap_or(false)
            {
                return Ok(0);
            }
        }
        if self.merged_pr_head_matches(deadline) {
            return Ok(0);
        }
        Ok(count)
    }

    /// A deleted remote branch after a squash merge cannot establish ancestry.
    /// GitHub's immutable merged PR head is an independent preservation proof.
    fn merged_pr_head_matches(&self, deadline: Option<std::time::Instant>) -> bool {
        let probe = || -> Result<bool> {
            let remote = run_inventory_git_until(
                &self.worktree,
                &["remote", "get-url", "origin"],
                "git remote get-url",
                deadline,
            )?;
            let Some(repo) = git::release_download::parse_github_url(remote.trim()) else {
                return Ok(false);
            };
            let Some(branch) = self.branch.as_deref() else {
                return Ok(false);
            };
            let head = format!("{}:{branch}", repo.owner);
            let head: String = head.bytes().map(|byte| format!("%{byte:02X}")).collect();
            let query = format!("state=closed&head={head}&per_page=100");
            let endpoint = format!("repos/{}/{}/pulls?{query}", repo.owner, repo.repo);
            let client = git::GhClient::for_repo(&repo);
            let deadline = deadline
                .unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(5));
            let pulls: Vec<Value> = client.api_json_until(&endpoint, deadline)?;
            Ok(pulls
                .iter()
                .any(|pr| merged_pr_preserves_head(pr, &self.head)))
        };
        probe().unwrap_or(false)
    }

    /// The first stash entry recorded on this worktree's branch or at its
    /// `HEAD`. Stashes live in the shared repository, so only entries this
    /// worktree produced block its removal.
    fn matching_stash(&self, deadline: Option<std::time::Instant>) -> Result<Option<String>> {
        let listing = run_inventory_git_until(
            &self.worktree,
            &["stash", "list", "--format=%gd%x09%P%x09%gs"],
            "git stash list",
            deadline,
        )?;
        Ok(listing.lines().find_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let (Some(selector), Some(parents), Some(subject)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return None;
            };
            let at_head = !self.head.is_empty()
                && parents.split_whitespace().next() == Some(self.head.as_str());
            let on_branch = self.branch.as_deref().is_some_and(|branch| {
                subject.starts_with(&format!("WIP on {branch}:"))
                    || subject.starts_with(&format!("On {branch}:"))
            });
            (at_head || on_branch).then(|| format!("{selector} ({subject})"))
        }))
    }
}

fn merged_pr_preserves_head(pr: &Value, head: &str) -> bool {
    !head.is_empty()
        && pr["merged_at"]
            .as_str()
            .is_some_and(|date| !date.is_empty())
        && pr["head"]["sha"].as_str() == Some(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_pr_requires_exact_head_not_just_closed_state() {
        let merged = serde_json::json!({"merged_at":"2026-11-20T00:00:00Z", "head":{"sha":"abc"}});
        assert!(merged_pr_preserves_head(&merged, "abc"));
        assert!(!merged_pr_preserves_head(&merged, "different"));
        assert!(!merged_pr_preserves_head(&merged, ""));
        let closed = serde_json::json!({"merged_at":null, "state":"closed", "head":{"sha":"abc"}});
        assert!(!merged_pr_preserves_head(&closed, "abc"));
        assert!(!merged_pr_preserves_head(&Value::Null, "abc"));
    }
}

/// Status for an unrecorded worktree path, reported as an orphan with the
/// evidence that decides whether it is reclaimable.
pub(super) fn status(id: &str) -> Result<Option<WorktreeStatusOutput>> {
    let deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
    let Some(unrecorded) = discover(id, deadline)? else {
        return Ok(None);
    };
    let safety = unrecorded.safety_report(deadline)?;
    Ok(Some(WorktreeStatusOutput {
        record: unrecorded.synthetic_record(TaskWorktreeState::Active),
        safety: Some(safety),
    }))
}

/// Remove an unrecorded worktree that passes every safety check.
pub(super) fn remove(
    options: &WorktreeRemoveOptions,
    store_dir: &Path,
    deadline: Option<std::time::Instant>,
) -> Result<Option<WorktreeRemoveOutput>> {
    let deadline = Some(
        deadline.unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(10)),
    );
    let Some(unrecorded) = discover(&options.id, deadline)? else {
        return Ok(None);
    };
    let safety = unrecorded.safety_report(deadline)?;
    if safety.primary_checkout || !safety.path_contained {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "Unrecorded worktree failed hard removal safety gates",
            Some(options.id.clone()),
            Some(safety.reasons.clone()),
        ));
    }
    if !safety.safe {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "Unrecorded worktree is not safe to remove; --force does not apply without a task-worktree record",
            Some(options.id.clone()),
            Some(safety.reasons.clone()),
        ));
    }
    let worktree = unrecorded.worktree.display().to_string();
    run_inventory_git_until(
        &unrecorded.primary_checkout,
        &["worktree", "remove", &worktree],
        "git worktree remove",
        deadline,
    )?;
    let record = unrecorded.synthetic_record(TaskWorktreeState::Removed);
    if let Some(data_root) = store_dir.parent() {
        reaping::append_reaping_audit_in_root(
            data_root,
            &record,
            now_ms(),
            options.reason.as_deref(),
            options.reaper.as_deref(),
            options.force,
            false,
        )?;
    }
    let branch_cleanup = WorktreeBranchCleanupReport {
        branch: record.branch.clone(),
        base_ref: record.base_ref.clone(),
        intent: record.branch_cleanup_intent.clone(),
        status: BranchCleanupStatus::Preserved,
        safe_delete: false,
        deleted: false,
        reason: Some(
            "unrecorded worktree: no recorded branch intent, so the local branch was preserved"
                .to_string(),
        ),
        cleanup_command: match unrecorded.branch.as_deref() {
            Some(branch) => format!(
                "git -C {} branch -d {}",
                shell_arg(&unrecorded.primary_checkout.display().to_string()),
                shell_arg(branch)
            ),
            None => String::new(),
        },
    };
    Ok(Some(WorktreeRemoveOutput {
        record,
        safety,
        branch_cleanup,
        removed: true,
    }))
}

pub(super) fn append_cleanup(
    output: &mut WorktreeCleanupOutput,
    options: &WorktreeCleanupOptions,
    store: &Path,
    deadline: Option<std::time::Instant>,
) -> Result<()> {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let deadline = Some(deadline.map_or(end, |deadline| deadline.min(end)));
    for path in cleanup_candidates(store, deadline)? {
        if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
            output.continuation = Some("homeboy worktree cleanup".to_string());
            break;
        }
        eprintln!("Inspecting orphan worktree {}", path.display());
        let id = path.display().to_string();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let probe = Some(deadline.map_or(end, |deadline| deadline.min(end)));
        let Some(orphan) = discover(&id, probe)? else {
            continue;
        };
        let record = orphan.synthetic_record(TaskWorktreeState::Active);
        let safety = match orphan.safety_report(probe) {
            Ok(safety) => safety,
            Err(error) => {
                output.skipped.push(WorktreeCleanupSkipped {
                    record,
                    safety: None,
                    reasons: vec![error.message],
                });
                continue;
            }
        };
        if !safety.safe {
            output.skipped.push(WorktreeCleanupSkipped {
                record,
                reasons: safety.reasons.clone(),
                safety: Some(safety),
            });
            continue;
        }
        output.candidates.push(WorktreeCleanupCandidate {
            branch_cleanup: branch_cleanup_unknown(
                &record,
                "unrecorded worktree: branch preserved".to_string(),
            ),
            record: record.clone(),
            safety: safety.clone(),
        });
        if !options.dry_run {
            match remove_with_store_until(
                WorktreeRemoveOptions {
                    id,
                    force: false,
                    cleanup_branch: false,
                    allow_unmerged_branch: false,
                    reason: Some("cleanup orphan worktree".to_string()),
                    reaper: options.reaper.clone(),
                },
                store,
                deadline,
            ) {
                Ok(removed) => output.removed.push(removed),
                Err(error) => output.skipped.push(WorktreeCleanupSkipped {
                    record,
                    safety: Some(safety),
                    reasons: vec![error.message],
                }),
            }
        }
    }
    output.counts.candidates = output.candidates.len();
    output.counts.removed = output.removed.len();
    output.counts.skipped = output.skipped.len();
    Ok(())
}

/// Discover unrecorded linked worktrees from registered and recorded source
/// checkouts. Git is the inventory authority, not directory-name heuristics.
pub(super) fn cleanup_candidates(
    store: &Path,
    deadline: Option<std::time::Instant>,
) -> Result<Vec<PathBuf>> {
    let (records, diagnostics) = scan_registry(store)?;
    if !diagnostics.is_empty() {
        return Err(Error::internal_unexpected(
            "orphan inventory requires a readable registry",
        ));
    }
    let recorded: std::collections::HashSet<_> = records
        .iter()
        .map(|record| canonical_or_raw(Path::new(&record.worktree_path)))
        .collect();
    let mut roots: std::collections::HashSet<_> = records
        .iter()
        .map(|record| PathBuf::from(&record.source_checkout))
        .collect();
    for component in component::registered_base()? {
        roots.insert(PathBuf::from(component.local_path));
    }
    let mut candidates = std::collections::BTreeSet::new();
    for root in roots {
        if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
            break;
        }
        if !root.is_dir() {
            continue;
        }
        let probe = Some(
            deadline
                .unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(2)),
        );
        let Ok(listing) = run_inventory_git_until(
            &root,
            &["worktree", "list", "--porcelain"],
            "inventory orphan worktrees",
            probe,
        ) else {
            continue;
        };
        for path in listing
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .skip(1)
        {
            let path = canonical_or_raw(Path::new(path));
            if path.is_dir() && !recorded.contains(&path) {
                candidates.insert(path);
            }
        }
    }
    Ok(candidates.into_iter().collect())
}

/// When `id` is not a record handle but names an existing directory, return
/// the handle of the active record whose worktree lives at that path. This
/// lets `status`/`remove` accept the worktree path a recorded Cook printed.
pub(super) fn recorded_handle_for_path(store_dir: &Path, id: &str) -> Result<Option<String>> {
    let path = Path::new(id);
    if !path.is_dir() {
        return Ok(None);
    }
    let Ok(target) = path.canonicalize() else {
        return Ok(None);
    };
    let (records, diagnostics) = scan_registry(store_dir)?;
    if !diagnostics.is_empty() {
        return Err(Error::internal_unexpected(
            "cannot prove worktree is unrecorded while registry manifests are unreadable",
        ));
    }
    Ok(records
        .into_iter()
        .filter(|record| record.state == TaskWorktreeState::Active)
        .find(|record| canonical_or_raw(Path::new(&record.worktree_path)) == target)
        .map(|record| record.id))
}

/// The record handle `id` refers to, or `None` when no record exists for it
/// (neither as a handle nor as a recorded worktree path).
pub(super) fn resolve_record_handle(store_dir: &Path, id: &str) -> Result<Option<String>> {
    if record_path(store_dir, id).exists() {
        return Ok(Some(id.to_string()));
    }
    recorded_handle_for_path(store_dir, id)
}

//! Classify an existing Cook destination before a Cook builds on it (#16217).
//!
//! A Cook derives its destination (`<repo>@fix-issue-<n>-<component>` and the
//! branch `fix/issue-<n>-<component>`) from the issue alone, so a later Cook
//! for the same issue finds whatever an earlier one left behind: a closed PR's
//! commits, an operator's unpushed work, or a different branch checked out in
//! the derived worktree. Building on any of that silently produces a candidate
//! on top of stale history, and resetting it orphans work Homeboy never owned.
//!
//! Classification is read-only. A destination is *fresh* only when it is clean,
//! on the expected branch, and has no commits ahead of `origin/<base>`.
//! [`refuse_stale_derived_destination`] refuses an issue-derived destination
//! that holds history this Cook did not create, with a typed error naming the
//! worktree, the checked-out branch, the commits ahead of the base, and the
//! commands to discard it or to choose another destination. Nothing is ever
//! reset or rebased. An explicit `--to-worktree`/`--head` is never classified.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::component::{self, TargetSpec};
use crate::error::{Error, Result};
use crate::git;

pub const STALE_DESTINATION_SCHEMA: &str = "homeboy/cook-stale-destination/v1";
const MAX_REPORTED_COMMITS: usize = 20;
const MAX_REPORTED_PATHS: usize = 20;

/// One commit a stale destination holds beyond the base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AheadCommit {
    pub sha: String,
    pub subject: String,
}

/// Why a destination is not safe to build on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleDestinationReason {
    CommitsAheadOfBase,
    UncommittedChanges,
    DifferentBranchCheckedOut,
    DetachedHead,
}

/// Evidence for a destination a Cook must not build on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaleDestination {
    pub schema: &'static str,
    pub handle: String,
    /// The worktree path, or the source checkout when only a local branch
    /// exists for the destination.
    pub path: PathBuf,
    /// Whether `path` is the destination worktree (`true`) or the source
    /// checkout holding a leftover local branch (`false`).
    pub worktree: bool,
    pub expected_branch: String,
    pub checked_out_branch: Option<String>,
    pub base_ref: String,
    pub ahead_count: usize,
    pub ahead_commits: Vec<AheadCommit>,
    pub dirty_path_count: usize,
    pub dirty_paths: Vec<String>,
    pub reasons: Vec<StaleDestinationReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationFreshness {
    /// No `origin/<base>` tracking ref or no Git checkout: nothing can be
    /// classified, so existing admission stays authoritative.
    Unclassified,
    /// Clean, on its expected branch, and nothing ahead of the base.
    Fresh,
    Stale(StaleDestination),
}

/// Classify an existing destination worktree against `origin/<base>`.
pub fn classify_worktree_destination(
    path: &Path,
    handle: &str,
    expected_branch: &str,
    base: &str,
) -> Result<DestinationFreshness> {
    if !path.is_dir() {
        return Ok(DestinationFreshness::Unclassified);
    }
    let Some((base_ref, base_sha)) = remote_base(path, base) else {
        return Ok(DestinationFreshness::Unclassified);
    };
    if git::output_optional(path, &["rev-parse", "--verify", "HEAD^{commit}"]).is_none() {
        return Ok(DestinationFreshness::Unclassified);
    }
    let checked_out_branch = git::output_optional(path, &["symbolic-ref", "--short", "-q", "HEAD"]);
    let status = git::run_git(
        path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        "inspect Cook destination for uncommitted changes",
    )?;
    let dirty = status
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .map(|entry| entry[3..].to_string())
        .collect::<Vec<_>>();
    let (ahead_count, ahead_commits) = ahead_of_base(path, &base_sha, "HEAD")?;
    let mut reasons = Vec::new();
    match checked_out_branch.as_deref() {
        None => reasons.push(StaleDestinationReason::DetachedHead),
        Some(branch) if branch != expected_branch => {
            reasons.push(StaleDestinationReason::DifferentBranchCheckedOut)
        }
        Some(_) => {}
    }
    if ahead_count > 0 {
        reasons.push(StaleDestinationReason::CommitsAheadOfBase);
    }
    if !dirty.is_empty() {
        reasons.push(StaleDestinationReason::UncommittedChanges);
    }
    if !reasons.is_empty() {
        return Ok(DestinationFreshness::Stale(StaleDestination {
            schema: STALE_DESTINATION_SCHEMA,
            handle: handle.to_string(),
            path: path.to_path_buf(),
            worktree: true,
            expected_branch: expected_branch.to_string(),
            checked_out_branch,
            base_ref,
            ahead_count,
            ahead_commits,
            dirty_path_count: dirty.len(),
            dirty_paths: dirty.into_iter().take(MAX_REPORTED_PATHS).collect(),
            reasons,
        }));
    }
    Ok(DestinationFreshness::Fresh)
}

/// Classify a local branch that exists without a destination worktree. A
/// missing branch is unclassified: creation starts it from the base.
pub fn classify_local_branch(
    source: &Path,
    handle: &str,
    branch: &str,
    base: &str,
) -> Result<DestinationFreshness> {
    let reference = format!("refs/heads/{branch}");
    if git::output_optional(
        source,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
    )
    .is_none()
    {
        return Ok(DestinationFreshness::Unclassified);
    }
    let Some((base_ref, base_sha)) = remote_base(source, base) else {
        return Ok(DestinationFreshness::Unclassified);
    };
    let (ahead_count, ahead_commits) = ahead_of_base(source, &base_sha, &reference)?;
    if ahead_count > 0 {
        return Ok(DestinationFreshness::Stale(StaleDestination {
            schema: STALE_DESTINATION_SCHEMA,
            handle: handle.to_string(),
            path: source.to_path_buf(),
            worktree: false,
            expected_branch: branch.to_string(),
            checked_out_branch: None,
            base_ref,
            ahead_count,
            ahead_commits,
            dirty_path_count: 0,
            dirty_paths: Vec::new(),
            reasons: vec![StaleDestinationReason::CommitsAheadOfBase],
        }));
    }
    Ok(DestinationFreshness::Fresh)
}

/// Classify the leftover local `branch` of a component's source checkout, as
/// worktree creation would adopt it. Unresolvable components are unclassified
/// so creation reports its own, more specific error.
pub fn classify_component_branch(
    component_id: &str,
    handle: &str,
    branch: &str,
    base: &str,
) -> Result<DestinationFreshness> {
    let Ok(target) = component::resolve_target(TargetSpec {
        component_id: Some(component_id),
        path_override: None,
        project: None,
        capability: None,
        allow_synthetic: true,
        accept_bare_directory: true,
        ..TargetSpec::default()
    }) else {
        return Ok(DestinationFreshness::Unclassified);
    };
    let Ok(source) = super::queue_ops::source_checkout_for_worktree(&target) else {
        return Ok(DestinationFreshness::Unclassified);
    };
    classify_local_branch(&source, handle, branch, base)
}

/// Refuse an issue-derived Cook destination that holds work this Cook did not
/// create. Only issue-derived destinations reach this: an explicit
/// `--to-worktree` or `--head` stays authoritative. Read-only, so it is safe
/// during preview. Uncommitted changes alone are left to Cook admission, which
/// owns reuse of a Cook's own in-progress candidate.
pub fn refuse_stale_derived_destination(
    component_id: &str,
    handle: &str,
    branch: &str,
    base: &str,
) -> Result<()> {
    let classification = match super::resolve_workspace_ref_if_present(handle)? {
        Some(super::WorkspaceRefRecord::Task(record))
            if record.state == super::TaskWorktreeState::Active =>
        {
            classify_worktree_destination(Path::new(&record.worktree_path), handle, branch, base)?
        }
        Some(_) => DestinationFreshness::Unclassified,
        None => classify_component_branch(component_id, handle, branch, base)?,
    };
    refuse_foreign_history(classification)
}

/// A destination is refused for history this Cook did not create: commits
/// ahead of the base, another branch, or a detached HEAD.
fn refuse_foreign_history(classification: DestinationFreshness) -> Result<()> {
    match classification {
        DestinationFreshness::Stale(stale)
            if stale
                .reasons
                .iter()
                .any(|reason| *reason != StaleDestinationReason::UncommittedChanges) =>
        {
            Err(stale.into_error())
        }
        _ => Ok(()),
    }
}

impl StaleDestination {
    /// The commands that discard the stale destination, or pick another one.
    pub fn recovery_commands(&self) -> Vec<String> {
        let path = quote(&self.path.display().to_string());
        let mut discard = Vec::new();
        if self.worktree {
            discard.push(format!(
                "homeboy worktree remove {} --cleanup-branch --allow-unmerged-branch --force",
                quote(&self.handle)
            ));
            discard.push(format!(
                "or with Git: git -C {path} worktree remove --force {path} && git branch -D {}",
                quote(&self.expected_branch)
            ));
            if let Some(branch) = self
                .checked_out_branch
                .as_deref()
                .filter(|branch| *branch != self.expected_branch)
            {
                discard.push(format!(
                    "the checked-out branch {} is kept by `worktree remove`; delete it with `git branch -D {}` only if its commits are not needed",
                    quote(branch),
                    quote(branch)
                ));
            }
        } else {
            discard.push(format!(
                "git -C {path} branch -D {}",
                quote(&self.expected_branch)
            ));
        }
        let mut commands = vec![format!(
            "Discard the stale destination, then rerun this Cook: {}",
            discard.join("; ")
        )];
        commands.push(
            "Or keep it and choose another destination: rerun this Cook with `--head <new-branch>` (and `--to-worktree <repo>@<new-branch-slug>`)."
                .to_string(),
        );
        commands
    }

    pub fn into_error(self) -> Error {
        let mut problems = Vec::new();
        for reason in &self.reasons {
            problems.push(match reason {
                StaleDestinationReason::CommitsAheadOfBase => format!(
                    "{} commit(s) ahead of {}: {}",
                    self.ahead_count,
                    self.base_ref,
                    self.ahead_commits
                        .iter()
                        .map(|commit| format!("{} {}", commit.sha, commit.subject))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
                StaleDestinationReason::UncommittedChanges => {
                    format!("{} uncommitted path(s)", self.dirty_path_count)
                }
                StaleDestinationReason::DifferentBranchCheckedOut => format!(
                    "branch `{}` is checked out instead of `{}`",
                    self.checked_out_branch.as_deref().unwrap_or_default(),
                    self.expected_branch
                ),
                StaleDestinationReason::DetachedHead => {
                    format!("HEAD is detached instead of on `{}`", self.expected_branch)
                }
            });
        }
        let subject = if self.worktree {
            format!(
                "destination `{}` at {} (checked out: {})",
                self.handle,
                self.path.display(),
                self.checked_out_branch
                    .as_deref()
                    .unwrap_or("detached HEAD")
            )
        } else {
            format!(
                "destination `{}` would adopt existing local branch `{}` in {}",
                self.handle,
                self.expected_branch,
                self.path.display()
            )
        };
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "Cook refuses to build on stale {subject}: {}; it was not reset and no provider ran",
                problems.join(", ")
            ),
            Some(self.handle.clone()),
            Some(self.recovery_commands()),
        );
        error.details["stale_destination"] =
            serde_json::to_value(&self).unwrap_or(serde_json::Value::Null);
        error.details["stale_destination"]["recovery_commands"] =
            serde_json::json!(self.recovery_commands());
        error
    }
}

fn remote_base(path: &Path, base: &str) -> Option<(String, String)> {
    let base = base.trim().trim_start_matches("refs/heads/");
    let base = base.strip_prefix("origin/").unwrap_or(base);
    if base.is_empty() {
        return None;
    }
    let sha = git::output_optional(
        path,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{base}^{{commit}}"),
        ],
    )?;
    Some((format!("origin/{base}"), sha))
}

fn ahead_of_base(path: &Path, base_sha: &str, head: &str) -> Result<(usize, Vec<AheadCommit>)> {
    let range = format!("{base_sha}..{head}");
    let total = count(path, &range)?;
    if total == 0 {
        return Ok((0, Vec::new()));
    }
    let log = git::run_git(
        path,
        &[
            "log",
            "--format=%h%x09%s",
            &format!("--max-count={MAX_REPORTED_COMMITS}"),
            &range,
        ],
        "list Cook destination commits ahead of base",
    )?;
    let commits = log
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (sha, subject) = line.split_once('\t').unwrap_or((line, ""));
            AheadCommit {
                sha: sha.to_string(),
                subject: subject.to_string(),
            }
        })
        .collect();
    Ok((total, commits))
}

fn count(path: &Path, range: &str) -> Result<usize> {
    let output = git::run_git(
        path,
        &["rev-list", "--count", range],
        "count Cook destination commits",
    )?;
    output.trim().parse::<usize>().map_err(|error| {
        Error::git_command_failed(format!(
            "could not count Cook destination commits for {range}: {error}"
        ))
    })
}

fn quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// An origin with `main`, a clone of it, and a linked derived worktree on
    /// `fix/issue-1-fixture` started from `origin/main`.
    struct Fixture {
        _root: tempfile::TempDir,
        origin: PathBuf,
        clone: PathBuf,
        worktree: PathBuf,
    }

    const BRANCH: &str = "fix/issue-1-fixture";
    const HANDLE: &str = "fixture@fix-issue-1-fixture";

    fn commit(cwd: &Path, file: &str, contents: &str, message: &str) {
        std::fs::write(cwd.join(file), contents).expect("write file");
        git(cwd, &["add", file]);
        git(cwd, &["commit", "-qm", message]);
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("root");
        let origin = root.path().join("origin");
        std::fs::create_dir(&origin).expect("origin dir");
        git(&origin, &["init", "-q", "--initial-branch=main"]);
        git(&origin, &["config", "user.email", "t@example.test"]);
        git(&origin, &["config", "user.name", "T"]);
        commit(&origin, "tracked.txt", "base\n", "base");
        let clone = root.path().join("clone");
        git(
            root.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        git(&clone, &["config", "user.email", "t@example.test"]);
        git(&clone, &["config", "user.name", "T"]);
        let worktree = root.path().join(HANDLE);
        git(
            &clone,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                BRANCH,
                worktree.to_str().unwrap(),
                "origin/main",
            ],
        );
        Fixture {
            _root: root,
            origin,
            clone,
            worktree,
        }
    }

    fn advance_origin(fixture: &Fixture) -> String {
        commit(&fixture.origin, "release.txt", "release\n", "release");
        git(&fixture.clone, &["fetch", "-q", "origin"]);
        git(&fixture.clone, &["rev-parse", "origin/main"])
    }

    #[test]
    fn stale_destination_with_commits_ahead_and_no_remote_branch_is_refused_intact() {
        let fixture = fixture();
        // A closed PR's two commits; its remote branch was deleted (never pushed here).
        commit(&fixture.worktree, "a.txt", "a\n", "closed pr first");
        commit(&fixture.worktree, "b.txt", "b\n", "closed pr second");
        advance_origin(&fixture);
        let head_before = git(&fixture.worktree, &["rev-parse", "HEAD"]);
        assert!(git(&fixture.clone, &["ls-remote", "--heads", "origin", BRANCH]).is_empty());

        let DestinationFreshness::Stale(stale) =
            classify_worktree_destination(&fixture.worktree, HANDLE, BRANCH, "main")
                .expect("classify")
        else {
            panic!("a destination with commits ahead must be stale");
        };
        assert_eq!(stale.ahead_count, 2);
        assert_eq!(stale.checked_out_branch.as_deref(), Some(BRANCH));
        assert_eq!(
            stale.reasons,
            vec![StaleDestinationReason::CommitsAheadOfBase]
        );
        let subjects = stale
            .ahead_commits
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect::<Vec<_>>();
        assert_eq!(subjects, vec!["closed pr second", "closed pr first"]);

        let error = stale.into_error();
        let message = error.message.clone();
        assert!(message.contains(HANDLE), "{message}");
        assert!(message.contains("closed pr first"), "{message}");
        assert!(message.contains(BRANCH), "{message}");
        assert!(error.details["stale_destination"]["recovery_commands"]
            .as_array()
            .is_some_and(|commands| commands.len() == 2));
        assert_eq!(
            git(&fixture.worktree, &["rev-parse", "HEAD"]),
            head_before,
            "classification never moves the stale branch"
        );
        assert_eq!(
            git(
                &fixture.clone,
                &["rev-parse", &format!("refs/heads/{BRANCH}")]
            ),
            head_before
        );
    }

    #[test]
    fn stale_destination_leftover_local_branch_without_worktree_is_refused() {
        let fixture = fixture();
        commit(&fixture.worktree, "a.txt", "a\n", "closed pr commit");
        let head = git(&fixture.worktree, &["rev-parse", "HEAD"]);
        git(
            &fixture.clone,
            &["worktree", "remove", fixture.worktree.to_str().unwrap()],
        );
        let DestinationFreshness::Stale(stale) =
            classify_local_branch(&fixture.clone, HANDLE, BRANCH, "main").expect("classify")
        else {
            panic!("a leftover branch with commits ahead must be stale");
        };
        assert!(!stale.worktree);
        assert_eq!(stale.ahead_count, 1);
        assert_eq!(
            git(
                &fixture.clone,
                &["rev-parse", &format!("refs/heads/{BRANCH}")]
            ),
            head
        );
        assert_eq!(
            classify_local_branch(&fixture.clone, HANDLE, "fix/absent", "main")
                .expect("classify absent"),
            DestinationFreshness::Unclassified
        );
    }

    #[test]
    fn stale_destination_with_a_different_branch_checked_out_is_never_reset() {
        let fixture = fixture();
        git(
            &fixture.worktree,
            &["checkout", "-q", "-b", "fix/issue-1-runner-cache"],
        );
        commit(&fixture.worktree, "c.txt", "c\n", "unpushed earlier work");
        let head_before = git(&fixture.worktree, &["rev-parse", "HEAD"]);
        advance_origin(&fixture);

        let DestinationFreshness::Stale(stale) =
            classify_worktree_destination(&fixture.worktree, HANDLE, BRANCH, "main")
                .expect("classify")
        else {
            panic!("a different branch checked out must be stale");
        };
        assert_eq!(
            stale.checked_out_branch.as_deref(),
            Some("fix/issue-1-runner-cache")
        );
        assert!(stale
            .reasons
            .contains(&StaleDestinationReason::DifferentBranchCheckedOut));
        assert!(stale
            .into_error()
            .message
            .contains("fix/issue-1-runner-cache"));
        assert_eq!(git(&fixture.worktree, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            git(&fixture.worktree, &["symbolic-ref", "--short", "HEAD"]),
            "fix/issue-1-runner-cache"
        );

        // Even a different branch with nothing ahead is refused, not reset.
        git(
            &fixture.worktree,
            &["checkout", "-q", "-b", "other", "origin/main"],
        );
        assert!(matches!(
            classify_worktree_destination(&fixture.worktree, HANDLE, BRANCH, "main")
                .expect("classify"),
            DestinationFreshness::Stale(_)
        ));
    }

    #[test]
    fn stale_destination_uncommitted_changes_alone_are_left_to_cook_admission() {
        let fixture = fixture();
        std::fs::write(fixture.worktree.join("tracked.txt"), "edit\n").expect("edit");
        let classification =
            classify_worktree_destination(&fixture.worktree, HANDLE, BRANCH, "main")
                .expect("classify");
        let DestinationFreshness::Stale(stale) = classification.clone() else {
            panic!("a dirty destination must be stale");
        };
        assert_eq!(
            stale.reasons,
            vec![StaleDestinationReason::UncommittedChanges]
        );
        assert_eq!(stale.dirty_paths, vec!["tracked.txt".to_string()]);
        // A Cook's own in-progress candidate looks exactly like this; Cook
        // admission owns its reuse, so the derived-destination check admits it.
        refuse_foreign_history(classification).expect("uncommitted changes alone are admitted");
    }

    #[test]
    fn stale_destination_clean_with_nothing_ahead_is_fresh_even_when_behind() {
        let fixture = fixture();
        advance_origin(&fixture);
        let classification =
            classify_worktree_destination(&fixture.worktree, HANDLE, BRANCH, "main")
                .expect("classify");
        assert_eq!(classification, DestinationFreshness::Fresh);
        refuse_foreign_history(classification).expect("a fresh destination is admitted");
    }

    #[test]
    fn stale_destination_without_tracking_base_is_unclassified() {
        let fixture = fixture();
        assert_eq!(
            classify_worktree_destination(&fixture.worktree, HANDLE, BRANCH, "trunk")
                .expect("classify"),
            DestinationFreshness::Unclassified
        );
    }
}

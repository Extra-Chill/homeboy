//! Core-owned CI autofix transaction.
//!
//! The end-to-end CI autofix transaction — branch preparation, drift-only
//! filtering, push-target resolution, and commit/push — historically lived in
//! [Extra-Chill/homeboy-action] shell (`prepare-autofix-branch.sh`,
//! `apply-autofix-commit.sh`, `lib.sh`). That left the bot identity, the
//! autofix commit prefix, drift classification, and push-target logic
//! duplicated between core and the action, with the action parsing sidecars
//! via `jq` to infer what core already knows.
//!
//! This module owns that transaction so the action becomes a thin caller: it
//! provides CI context (repo, branch, token) and a description of the staged
//! changes, and core decides how to branch, filter, commit, and push using the
//! primitives it already owns:
//!
//! - Bot identity and commit prefix: [`crate::core::git`] / [`super::guard`].
//! - Drift-file classification: [`crate::core::component::drift`].
//! - Git primitives: [`crate::core::git`] (`stage_all`, `commit_*`, `run_git`).
//!
//! The transaction is intentionally agnostic: it knows nothing about Rust, PHP,
//! JS, or any particular ecosystem. It operates purely on git state, the
//! component's declared drift files, and the supplied CI context.
//!
//! [Extra-Chill/homeboy-action]: https://github.com/Extra-Chill/homeboy-action

use std::path::Path;

use serde::Serialize;

use crate::core::component::Component;
use crate::core::error::Result;
use crate::core::git::{
    commit_staged_with_author, configure_identity, has_staged_changes, parse_git_identity, run_git,
    run_git_output, stage_all, GitIdentity,
};

/// Prefix used for all autofix commits. Single source of truth shared with the
/// guard checks ([`super::guard`]) and the action.
pub const AUTOFIX_COMMIT_PREFIX: &str = "chore(ci): homeboy autofix";

/// Commit prefix used for drift-only (baseline / generated-metadata) pushes.
///
/// Distinct from [`AUTOFIX_COMMIT_PREFIX`] so drift commits do not count toward
/// the autofix cap enforced by [`super::guard`].
pub const DRIFT_COMMIT_PREFIX: &str = "chore(ci): update audit baseline";

/// How autofix changes should be routed once committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PushRoute {
    /// Changes are review-worthless bookkeeping (drift only) — push directly to
    /// the base branch.
    DirectDrift,
    /// Changes contain authored fixes — push to the autofix branch (PR flow).
    AutofixBranch,
}

/// CI context the action supplies to the transaction.
///
/// All fields are optional so the same struct works for PR autofix, non-PR
/// (scheduled) autofix, and local dry runs. Resolution falls back to `origin`
/// / the current branch when context is absent.
#[derive(Debug, Clone, Default)]
pub struct CiContext {
    /// Target repository (`owner/repo`). When equal to the current `origin`
    /// repo and no token is set, the push target collapses to `origin`.
    pub target_repo: Option<String>,
    /// Repository backing the current `origin` remote (`owner/repo`). Used to
    /// decide whether the target repo is the same remote.
    pub origin_repo: Option<String>,
    /// Branch to push to (the PR head branch, or the autofix branch name).
    pub target_branch: Option<String>,
    /// GitHub App / access token for cross-repo or re-run-triggering pushes.
    pub token: Option<String>,
}

impl CiContext {
    /// Resolve the git remote to push to.
    ///
    /// - With a token: an authenticated `https://x-access-token:…` URL so the
    ///   push originates from a GitHub App (which re-triggers workflows).
    /// - Same repo as `origin`, no token: the literal `origin` remote.
    /// - Different repo, no token: an anonymous `https://github.com/…` URL.
    /// - No target repo at all: `origin`.
    pub fn resolve_push_target(&self) -> String {
        let Some(repo) = self.target_repo.as_deref() else {
            return "origin".to_string();
        };

        if let Some(token) = self.token.as_deref() {
            return build_github_remote_url(repo, Some(token));
        }

        if self.origin_repo.as_deref() == Some(repo) {
            return "origin".to_string();
        }

        build_github_remote_url(repo, None)
    }
}

/// Build a GitHub HTTPS remote URL, embedding a token when supplied.
pub fn build_github_remote_url(repo: &str, token: Option<&str>) -> String {
    match token {
        Some(token) => format!("https://x-access-token:{token}@github.com/{repo}.git"),
        None => format!("https://github.com/{repo}.git"),
    }
}

/// Outcome of an autofix transaction.
#[derive(Debug, Clone, Serialize)]
pub struct TransactionOutcome {
    /// Whether a commit was created and pushed.
    pub committed: bool,
    /// Machine-readable status (e.g. `pushed`, `no-changes`, `push-failed`).
    pub status: String,
    /// How the change was routed once committed.
    pub route: Option<PushRoute>,
    /// Repo-relative paths included in the commit (sorted).
    pub changed_files: Vec<String>,
    /// Branch the commit was pushed to.
    pub target_branch: Option<String>,
}

impl TransactionOutcome {
    fn skipped(status: impl Into<String>) -> Self {
        Self {
            committed: false,
            status: status.into(),
            route: None,
            changed_files: Vec::new(),
            target_branch: None,
        }
    }
}

/// Inputs to an autofix transaction.
pub struct TransactionRequest<'a> {
    /// Repository working tree to operate on.
    pub repo_path: &'a Path,
    /// Component whose declared drift files classify the staged changes.
    pub component: &'a Component,
    /// CI context for push-target resolution.
    pub ci: CiContext,
    /// Git identity to commit as (`None` → CI bot identity).
    pub git_identity: Option<&'a str>,
    /// Commit message subject/body for authored (non-drift) fixes. When the
    /// staged changes are drift-only, a fixed drift message is used instead so
    /// the commit does not count toward the autofix cap.
    pub fix_commit_message: String,
    /// When `true`, stage, commit, and push are skipped — only classification
    /// and target resolution run. Useful for local inspection.
    pub dry_run: bool,
}

/// Run the end-to-end CI autofix transaction.
///
/// Stages all working-tree changes, classifies them (drift-only vs authored),
/// builds the appropriate commit, and pushes to the resolved target. Returns a
/// [`TransactionOutcome`] describing what happened. Returns early (without
/// error) when there is nothing to commit.
pub fn run_autofix_transaction(request: TransactionRequest<'_>) -> Result<TransactionOutcome> {
    let repo = request.repo_path;

    stage_all(repo)?;

    if !has_staged_changes(repo)? {
        return Ok(TransactionOutcome::skipped("no-changes"));
    }

    let changed_files = staged_files(repo)?;
    if changed_files.is_empty() {
        return Ok(TransactionOutcome::skipped("no-changes"));
    }

    let drift_files = crate::core::component::drift::drift_file_paths(request.component);
    let route = if changes_are_only_drift(&changed_files, &drift_files) {
        PushRoute::DirectDrift
    } else {
        PushRoute::AutofixBranch
    };

    let target_branch = request.ci.target_branch.clone();

    if request.dry_run {
        return Ok(TransactionOutcome {
            committed: false,
            status: "dry-run".to_string(),
            route: Some(route),
            changed_files,
            target_branch,
        });
    }

    let identity = parse_git_identity(request.git_identity);
    configure_identity(&repo.to_string_lossy(), &identity)?;

    let message = match route {
        PushRoute::DirectDrift => drift_commit_message(&changed_files),
        PushRoute::AutofixBranch => request.fix_commit_message.clone(),
    };
    commit(repo, &message, &identity)?;

    let push_target = request.ci.resolve_push_target();
    let pushed = push_to_target(repo, &push_target, target_branch.as_deref())?;

    let status = if pushed { "pushed" } else { "push-failed" };
    Ok(TransactionOutcome {
        committed: pushed,
        status: status.to_string(),
        route: Some(route),
        changed_files,
        target_branch,
    })
}

/// Return the sorted list of staged repo-relative paths.
fn staged_files(repo: &Path) -> Result<Vec<String>> {
    let stdout = run_git(
        repo,
        &["diff", "--cached", "--name-only"],
        "git diff --cached --name-only",
    )?;
    let mut files: Vec<String> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// Whether every changed file is a declared drift file. An empty change set is
/// not considered drift-only (there is nothing to push).
pub fn changes_are_only_drift(changed_files: &[String], drift_files: &[String]) -> bool {
    if changed_files.is_empty() {
        return false;
    }
    changed_files
        .iter()
        .all(|file| drift_files.iter().any(|drift| drift == file))
}

/// Build the drift-only commit message (distinct prefix, lists files).
fn drift_commit_message(changed_files: &[String]) -> String {
    let mut message = format!("{DRIFT_COMMIT_PREFIX}\n\nDrift-only update (no authored fixes).");
    for file in changed_files {
        message.push('\n');
        message.push_str(file);
    }
    message
}

fn commit(repo: &Path, message: &str, identity: &GitIdentity) -> Result<()> {
    let author = format!("{} <{}>", identity.name, identity.email);
    commit_staged_with_author(repo, message, &author)
}

/// Push `HEAD` to the resolved target. A `None` branch pushes the current
/// branch as-is; otherwise pushes `HEAD:refs/heads/<branch>`. Returns whether
/// the push succeeded.
fn push_to_target(repo: &Path, target: &str, branch: Option<&str>) -> Result<bool> {
    let refspec = branch.map(|b| format!("HEAD:refs/heads/{b}"));
    let mut args: Vec<&str> = vec!["push", target];
    if let Some(refspec) = refspec.as_deref() {
        args.push(refspec);
    }
    let output = run_git_output(repo, &args, "git push")?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_remote_url_with_and_without_token() {
        assert_eq!(
            build_github_remote_url("owner/repo", None),
            "https://github.com/owner/repo.git"
        );
        assert_eq!(
            build_github_remote_url("owner/repo", Some("abc")),
            "https://x-access-token:abc@github.com/owner/repo.git"
        );
    }

    #[test]
    fn resolve_push_target_no_repo_is_origin() {
        let ctx = CiContext::default();
        assert_eq!(ctx.resolve_push_target(), "origin");
    }

    #[test]
    fn resolve_push_target_same_repo_no_token_is_origin() {
        let ctx = CiContext {
            target_repo: Some("owner/repo".to_string()),
            origin_repo: Some("owner/repo".to_string()),
            target_branch: None,
            token: None,
        };
        assert_eq!(ctx.resolve_push_target(), "origin");
    }

    #[test]
    fn resolve_push_target_cross_repo_no_token_is_anon_url() {
        let ctx = CiContext {
            target_repo: Some("fork/repo".to_string()),
            origin_repo: Some("owner/repo".to_string()),
            target_branch: None,
            token: None,
        };
        assert_eq!(
            ctx.resolve_push_target(),
            "https://github.com/fork/repo.git"
        );
    }

    #[test]
    fn resolve_push_target_with_token_is_authenticated_url() {
        let ctx = CiContext {
            target_repo: Some("owner/repo".to_string()),
            origin_repo: Some("owner/repo".to_string()),
            target_branch: None,
            token: Some("tok".to_string()),
        };
        assert_eq!(
            ctx.resolve_push_target(),
            "https://x-access-token:tok@github.com/owner/repo.git"
        );
    }

    #[test]
    fn changes_are_only_drift_true_when_subset() {
        let changed = vec!["homeboy.json".to_string()];
        let drift = vec!["homeboy.json".to_string(), "Cargo.lock".to_string()];
        assert!(changes_are_only_drift(&changed, &drift));
    }

    #[test]
    fn changes_are_only_drift_false_when_authored_file_present() {
        let changed = vec!["homeboy.json".to_string(), "src/main.rs".to_string()];
        let drift = vec!["homeboy.json".to_string()];
        assert!(!changes_are_only_drift(&changed, &drift));
    }

    #[test]
    fn changes_are_only_drift_false_when_empty() {
        let changed: Vec<String> = Vec::new();
        let drift = vec!["homeboy.json".to_string()];
        assert!(!changes_are_only_drift(&changed, &drift));
    }

    #[test]
    fn drift_commit_message_uses_drift_prefix_and_lists_files() {
        let msg = drift_commit_message(&["homeboy.json".to_string()]);
        assert!(msg.starts_with(DRIFT_COMMIT_PREFIX));
        assert!(msg.contains("homeboy.json"));
    }
}

//! Homeboy-owned destination residue (#18888).
//!
//! A promotion leaves its candidate staged on the destination base, and a
//! superseded attempt's alignment can leave an earlier candidate staged too.
//! Before a newer candidate is materialized, any non-clean destination state
//! is snapshotted to `refs/homeboy/superseded-worktree/<run>`. State Homeboy
//! can attribute to itself (an index that is exactly a recorded Homeboy
//! candidate tree, with no unstaged or untracked edits) is then reset away.
//! Anything else stops promotion, and the error names the snapshot ref.
use std::path::Path;

use homeboy_core::{Error, Result};
use serde_json::{json, Value};

pub(crate) const SUPERSEDED_WORKTREE_REF_PREFIX: &str = "refs/homeboy/superseded-worktree";

/// Ref namespaces whose commits are candidates Homeboy itself produced or
/// preserved. Their trees identify staged state as Homeboy's own.
const HOMEBOY_CANDIDATE_REF_PREFIXES: &[&str] = &[
    "refs/homeboy/candidates/",
    "refs/homeboy/preserved-candidates/",
    "refs/homeboy/superseded/",
    "refs/homeboy/superseded-worktree/",
];

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    git_with_index(repo, args, None)
}

fn git_with_index(repo: &Path, args: &[&str], index: Option<&Path>) -> Result<String> {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(repo).args(args);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), None))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            String::from_utf8_lossy(&output.stderr).trim(),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Snapshot and release non-clean destination state before a candidate is
/// materialized. Returns `None` for a clean destination, the snapshot
/// evidence after Homeboy-owned residue was reset to `HEAD`, and an error
/// naming the snapshot ref for edits Homeboy cannot attribute to itself.
/// A dry run reports what it would do and mutates nothing.
pub(crate) fn release_homeboy_residue(
    repo: &Path,
    run_id: &str,
    dry_run: bool,
) -> Result<Option<Value>> {
    let dirty_paths = git(repo, &["status", "--porcelain", "--untracked-files=all"])?;
    if dirty_paths.is_empty() {
        return Ok(None);
    }
    let head = git(repo, &["rev-parse", "HEAD"])?;
    let index_tree = git(repo, &["write-tree"])?;
    let attributed = homeboy_owned_index(repo, &index_tree)?;
    let base_reference = format!(
        "{SUPERSEDED_WORKTREE_REF_PREFIX}/{}",
        homeboy_core::paths::sanitize_path_segment(run_id)
    );
    if dry_run {
        if attributed.is_none() {
            return Err(unattributed_error(&base_reference, &dirty_paths, true));
        }
        return Ok(Some(json!({
            "schema": "homeboy/promotion-destination-residue/v1",
            "head": head,
            "index_tree": index_tree,
            "attributed_to": attributed,
            "ref": base_reference,
            "dry_run": true,
        })));
    }
    let (reference, snapshot) = snapshot(repo, &head, &base_reference)?;
    let Some(attributed) = attributed else {
        return Err(unattributed_error(&reference, &dirty_paths, false));
    };
    git(repo, &["reset", "--hard", "HEAD"])?;
    Ok(Some(json!({
        "schema": "homeboy/promotion-destination-residue/v1",
        "head": head,
        "index_tree": index_tree,
        "attributed_to": attributed,
        "ref": reference,
        "snapshot": snapshot,
    })))
}

/// The Homeboy ref whose tree the index exactly is, provided the working tree
/// holds nothing beyond the index (no unstaged or untracked edits).
fn homeboy_owned_index(repo: &Path, index_tree: &str) -> Result<Option<String>> {
    let unstaged = git(repo, &["diff", "--name-only"])?;
    let untracked = git(repo, &["ls-files", "--others", "--exclude-standard"])?;
    if !unstaged.is_empty() || !untracked.is_empty() {
        return Ok(None);
    }
    let refs = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/homeboy/",
        ],
    )?;
    for line in refs.lines() {
        let Some((name, object)) = line.split_once(' ') else {
            continue;
        };
        if !HOMEBOY_CANDIDATE_REF_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        if git(repo, &["rev-parse", &format!("{object}^{{tree}}")])
            .ok()
            .as_deref()
            == Some(index_tree)
        {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// Record index, working tree and untracked files as one commit on `head`,
/// through a private index so the destination index is untouched. Never
/// overwrites an earlier snapshot: a taken ref gets a numbered sibling.
fn snapshot(repo: &Path, head: &str, base_reference: &str) -> Result<(String, String)> {
    let index_file = homeboy_core::git::private_index(repo)?;
    let index = index_file.path();
    git_with_index(repo, &["read-tree", head], Some(index))?;
    git_with_index(repo, &["add", "--all"], Some(index))?;
    let tree = git_with_index(repo, &["write-tree"], Some(index))?;
    let commit = git(
        repo,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit-tree",
            &tree,
            "-p",
            head,
            "-m",
            "homeboy: destination state before candidate promotion",
        ],
    )?;
    for suffix in 1.. {
        let reference = if suffix == 1 {
            base_reference.to_string()
        } else {
            format!("{base_reference}-{suffix}")
        };
        match git(repo, &["rev-parse", "--verify", "--quiet", &reference]) {
            Ok(existing) if existing == commit => return Ok((reference, commit)),
            Ok(_) => continue,
            Err(_) => {
                git(repo, &["update-ref", &reference, &commit, ""])?;
                return Ok((reference, commit));
            }
        }
    }
    unreachable!("an unbounded suffix search always returns")
}

fn unattributed_error(reference: &str, dirty_paths: &str, dry_run: bool) -> Error {
    let preserved = if dry_run {
        format!("promotion would preserve them at {reference}")
    } else {
        format!("preserved at {reference}")
    };
    let mut error = Error::validation_invalid_argument(
        "to_worktree",
        format!(
            "candidate destination must be clean: edits Homeboy cannot attribute to its own candidates are {preserved}"
        ),
        None,
        None,
    );
    error.details["destination_snapshot_ref"] = json!(reference);
    error.details["destination_paths"] = json!(dirty_paths.lines().collect::<Vec<_>>());
    error
}

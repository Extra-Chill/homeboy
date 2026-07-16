//! Tests for `core::stack::status` — read-only stack status.
//!
//! The full `status()` entry point hits `gh` for upstream PR metadata, so
//! tests here focus on the deterministic git-side helpers that build the
//! report's local-state columns: ahead/behind counts, ref existence, and
//! commit reachability. End-to-end reporting is verified out-of-band via
//! the live-verify fixture spec described in the PR body.

use crate::stack::status::{commit_reachable, count_revs, git_ref_exists, patch_in_base};
use std::fs;

mod support;
use support::{commit_file, git, init_repo};

// ---------------------------------------------------------------------------
// git_ref_exists
// ---------------------------------------------------------------------------

#[test]
fn git_ref_exists_true_for_existing_branch() {
    let (_dir, path) = init_repo();
    assert!(git_ref_exists(&path, "main"));
    assert!(git_ref_exists(&path, "HEAD"));
}

#[test]
fn git_ref_exists_false_for_missing_branch() {
    let (_dir, path) = init_repo();
    assert!(!git_ref_exists(&path, "no-such-branch"));
    assert!(!git_ref_exists(&path, "origin/never-fetched"));
}

// ---------------------------------------------------------------------------
// count_revs
// ---------------------------------------------------------------------------

#[test]
fn count_revs_zero_when_branches_at_same_commit() {
    let (_dir, path) = init_repo();
    git(&path, &["branch", "twin"]);
    assert_eq!(count_revs(&path, "main", "twin"), Some(0));
    assert_eq!(count_revs(&path, "twin", "main"), Some(0));
}

#[test]
fn count_revs_returns_ahead_count() {
    let (dir, path) = init_repo();
    git(&path, &["branch", "base"]);
    commit_file(&dir, &path, "a.txt", "a\n", "a");
    commit_file(&dir, &path, "b.txt", "b\n", "b");
    commit_file(&dir, &path, "c.txt", "c\n", "c");
    assert_eq!(count_revs(&path, "base", "main"), Some(3));
    // Reverse direction: base has 0 commits ahead of main.
    assert_eq!(count_revs(&path, "main", "base"), Some(0));
}

#[test]
fn count_revs_none_for_invalid_ref() {
    let (_dir, path) = init_repo();
    // Unknown ref → git rev-list errors → None.
    assert_eq!(count_revs(&path, "main", "nope"), None);
}

// ---------------------------------------------------------------------------
// commit_reachable
// ---------------------------------------------------------------------------

#[test]
fn commit_reachable_true_when_sha_in_branch_history() {
    let (dir, path) = init_repo();
    let sha = commit_file(&dir, &path, "a.txt", "a\n", "a");
    assert_eq!(commit_reachable(&path, &sha, "main"), Some(true));
}

#[test]
fn commit_reachable_false_when_sha_on_different_branch() {
    let (dir, path) = init_repo();
    git(&path, &["checkout", "-q", "-b", "feature"]);
    let sha = commit_file(&dir, &path, "a.txt", "a\n", "feature only");
    git(&path, &["checkout", "-q", "main"]);
    assert_eq!(commit_reachable(&path, &sha, "main"), Some(false));
    // But still reachable from the branch that owns it.
    assert_eq!(commit_reachable(&path, &sha, "feature"), Some(true));
}

#[test]
fn commit_reachable_none_for_unknown_sha() {
    let (_dir, path) = init_repo();
    let bogus = "0000000000000000000000000000000000000000";
    assert!(commit_reachable(&path, bogus, "main").is_none());
}

#[test]
fn commit_reachable_none_for_empty_sha() {
    let (_dir, path) = init_repo();
    assert!(commit_reachable(&path, "", "main").is_none());
}

// ---------------------------------------------------------------------------
// patch_in_base — squash-merge detection
// ---------------------------------------------------------------------------

#[test]
fn patch_in_base_detects_squash_merged_content() {
    let (dir, path) = init_repo();

    // pr-feature: the PR's "head SHA" before merge.
    git(&path, &["checkout", "-q", "-b", "pr-feature"]);
    let pr_head_sha = commit_file(&dir, &path, "feature.txt", "feature\n", "PR feature commit");

    // Back to base branch (still "main"); apply the SAME tree as a
    // different commit (this is what squash-merge does upstream).
    git(&path, &["checkout", "-q", "main"]);
    fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "-q", "-m", "Squash-merge PR feature"]);

    // pr_head_sha is on pr-feature but NOT main; main has its own commit
    // with the same tree. patch_in_base should detect equivalence.
    assert_eq!(
        commit_reachable(&path, &pr_head_sha, "main"),
        Some(false),
        "head SHA must not be reachable from squash-merged main"
    );
    assert_eq!(
        patch_in_base(&path, &pr_head_sha, "main"),
        Some(true),
        "patch-id should match the squash on main"
    );
}

#[test]
fn patch_in_base_returns_false_when_patch_absent() {
    let (dir, path) = init_repo();
    git(&path, &["checkout", "-q", "-b", "pr-feature"]);
    let pr_head_sha = commit_file(&dir, &path, "feature.txt", "feature\n", "PR feature commit");

    // main has no equivalent commit.
    git(&path, &["checkout", "-q", "main"]);

    assert_eq!(
        patch_in_base(&path, &pr_head_sha, "main"),
        Some(false),
        "patch should not be in base when no equivalent commit exists"
    );
}

#[test]
fn patch_in_base_unknown_when_sha_not_local() {
    let (_dir, path) = init_repo();
    // SHA shape is valid hex but no such object exists.
    assert_eq!(
        patch_in_base(&path, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef", "main"),
        None,
        "absent SHA must surface as None, not Some(false)"
    );
    assert_eq!(
        patch_in_base(&path, "", "main"),
        None,
        "empty SHA must surface as None"
    );
}

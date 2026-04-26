//! Tests for `core::stack::status` — read-only stack status.
//!
//! The full `status()` entry point hits `gh` for upstream PR metadata, so
//! tests here focus on the deterministic git-side helpers that build the
//! report's local-state columns: ahead/behind counts, ref existence, and
//! commit reachability. End-to-end reporting is verified out-of-band via
//! the live-verify fixture spec described in the PR body.

use crate::stack::status::{commit_reachable, count_revs, git_ref_exists};
use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn init_repo() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().to_string_lossy().to_string();
    run(&path, &["init", "-q", "-b", "main"]);
    run(&path, &["config", "user.email", "test@test.com"]);
    run(&path, &["config", "user.name", "Test"]);
    fs::write(dir.path().join("README.md"), "initial\n").unwrap();
    run(&path, &["add", "."]);
    run(&path, &["commit", "-q", "-m", "initial"]);
    (dir, path)
}

fn run(path: &str, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap_or_else(|e| panic!("git {:?}: {}", args, e));
    assert!(
        out.status.success(),
        "git {:?} failed: {} / {}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_and_commit(dir: &TempDir, path: &str, file: &str, body: &str, msg: &str) -> String {
    fs::write(dir.path().join(file), body).unwrap();
    run(path, &["add", "."]);
    run(path, &["commit", "-q", "-m", msg]);
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

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
    run(&path, &["branch", "twin"]);
    assert_eq!(count_revs(&path, "main", "twin"), Some(0));
    assert_eq!(count_revs(&path, "twin", "main"), Some(0));
}

#[test]
fn count_revs_returns_ahead_count() {
    let (dir, path) = init_repo();
    run(&path, &["branch", "base"]);
    write_and_commit(&dir, &path, "a.txt", "a\n", "a");
    write_and_commit(&dir, &path, "b.txt", "b\n", "b");
    write_and_commit(&dir, &path, "c.txt", "c\n", "c");
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
    let sha = write_and_commit(&dir, &path, "a.txt", "a\n", "a");
    assert_eq!(commit_reachable(&path, &sha, "main"), Some(true));
}

#[test]
fn commit_reachable_false_when_sha_on_different_branch() {
    let (dir, path) = init_repo();
    run(&path, &["checkout", "-q", "-b", "feature"]);
    let sha = write_and_commit(&dir, &path, "a.txt", "a\n", "feature only");
    run(&path, &["checkout", "-q", "main"]);
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

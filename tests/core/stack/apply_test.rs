//! Tests for `core::stack::apply` — cherry-pick orchestration.
//!
//! The full `apply()` entry point reaches out to `gh` to resolve PR head
//! SHAs, so it can't be exercised hermetically without mocking the network.
//! Instead, these tests cover the pure git-side helpers that drive the
//! interesting behaviour: cherry-pick outcome detection (picked / empty /
//! conflict), URL matching, and force-checkout from a base ref.
//!
//! End-to-end correctness is verified out-of-band via the live-verify
//! fixture spec described in the PR body.

use crate::stack::apply::{checkout_force, cherry_pick, url_matches, CherryPickResult};
use std::fs;
use std::process::Command;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Init an empty git repo with one initial commit on `main`.
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
// cherry_pick
// ---------------------------------------------------------------------------

#[test]
fn cherry_pick_succeeds_picked() {
    let (dir, path) = init_repo();
    // Create a feature branch with a non-conflicting commit, then go back
    // to main and cherry-pick it cleanly.
    run(&path, &["checkout", "-q", "-b", "feature"]);
    let sha = write_and_commit(&dir, &path, "a.txt", "feature change\n", "feature commit");
    run(&path, &["checkout", "-q", "main"]);

    let result = cherry_pick(&path, &sha).expect("cherry_pick");
    assert!(
        matches!(result, CherryPickResult::Picked),
        "expected Picked, got {:?}",
        result
    );

    // Working tree must be clean — no in-progress cherry-pick.
    let status = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(status.stdout.is_empty(), "working tree should be clean");
}

#[test]
fn cherry_pick_skips_empty_when_change_already_in_base() {
    let (dir, path) = init_repo();
    // Make a commit on main, branch off, attempt to cherry-pick it back —
    // the change is already in base, so the pick should be empty.
    let sha = write_and_commit(&dir, &path, "a.txt", "shared change\n", "shared commit");
    run(&path, &["checkout", "-q", "-b", "feature"]);

    let result = cherry_pick(&path, &sha).expect("cherry_pick");
    assert!(
        matches!(result, CherryPickResult::Empty),
        "expected Empty (already-applied), got {:?}",
        result
    );

    // Empty pick path uses `cherry-pick --skip` for cleanup, so the working
    // tree must be clean afterward.
    let status = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(
        status.stdout.is_empty(),
        "working tree should be clean after empty-pick skip; got: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn cherry_pick_returns_conflict_with_message() {
    let (dir, path) = init_repo();
    // Both branches modify the same line of the same file → guaranteed
    // conflict on cherry-pick.
    write_and_commit(&dir, &path, "f.txt", "main version\n", "main edit");
    run(&path, &["checkout", "-q", "-b", "feature", "HEAD~1"]);
    let conflict_sha = write_and_commit(&dir, &path, "f.txt", "feature version\n", "feature edit");
    run(&path, &["checkout", "-q", "main"]);

    let result = cherry_pick(&path, &conflict_sha).expect("cherry_pick");
    match result {
        CherryPickResult::Conflict(msg) => {
            assert!(!msg.is_empty(), "conflict message should not be empty");
        }
        other => panic!("expected Conflict, got {:?}", other),
    }

    // Caller (the `apply` layer) is responsible for `cherry-pick --abort`.
    // Tests should clean up so the tempdir is healthy.
    let _ = Command::new("git")
        .args(["cherry-pick", "--abort"])
        .current_dir(&path)
        .output();
}

// ---------------------------------------------------------------------------
// checkout_force
// ---------------------------------------------------------------------------

#[test]
fn checkout_force_recreates_branch_from_base() {
    let (dir, path) = init_repo();
    // Add commits to main so HEAD ≠ initial.
    write_and_commit(&dir, &path, "x.txt", "x\n", "x");
    write_and_commit(&dir, &path, "y.txt", "y\n", "y");

    // Tag main HEAD as our "base remote ref" stand-in.
    run(&path, &["tag", "base"]);

    // Create a divergent target branch with a stale commit.
    run(&path, &["checkout", "-q", "-b", "target"]);
    write_and_commit(&dir, &path, "stale.txt", "stale\n", "stale on target");

    // Now force-recreate target from base — stale commit must vanish.
    checkout_force(&path, "target", "base").expect("checkout_force");

    // HEAD should be at base (not the stale commit).
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&path)
        .output()
        .unwrap();
    let base_sha = Command::new("git")
        .args(["rev-parse", "base"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert_eq!(
        head.stdout, base_sha.stdout,
        "after force-checkout, HEAD must match base"
    );

    // The stale file must be gone.
    assert!(
        !dir.path().join("stale.txt").exists(),
        "stale file should be removed by force-checkout"
    );
}

// ---------------------------------------------------------------------------
// url_matches
// ---------------------------------------------------------------------------

#[test]
fn url_matches_https_with_and_without_dot_git() {
    assert!(url_matches(
        "https://github.com/Automattic/studio.git",
        "https://github.com/Automattic/studio"
    ));
    assert!(url_matches(
        "https://github.com/Automattic/studio",
        "https://github.com/Automattic/studio.git"
    ));
}

#[test]
fn url_matches_https_vs_ssh() {
    assert!(url_matches(
        "https://github.com/Automattic/studio.git",
        "git@github.com:Automattic/studio.git"
    ));
}

#[test]
fn url_matches_case_insensitive() {
    assert!(url_matches(
        "https://github.com/automattic/STUDIO.git",
        "https://github.com/Automattic/studio"
    ));
}

#[test]
fn url_matches_rejects_different_repos() {
    assert!(!url_matches(
        "https://github.com/Automattic/studio",
        "https://github.com/Automattic/playground"
    ));
}

#[test]
fn url_matches_rejects_non_github_urls() {
    // Non-github URLs aren't keyed and conservatively return false.
    assert!(!url_matches(
        "https://gitlab.com/foo/bar",
        "https://gitlab.com/foo/bar"
    ));
}

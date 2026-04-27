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

use crate::stack::apply::{checkout_force, cherry_pick, rebase, url_matches, CherryPickResult};
use crate::stack::{save, GitRef, StackSpec};
use crate::test_support::with_isolated_home;
use std::fs;
use std::process::Command;

mod support;
use support::{commit_file, git, init_repo, rev_parse};

// ---------------------------------------------------------------------------
// cherry_pick
// ---------------------------------------------------------------------------

#[test]
fn cherry_pick_succeeds_picked() {
    let (dir, path) = init_repo();
    // Create a feature branch with a non-conflicting commit, then go back
    // to main and cherry-pick it cleanly.
    git(&path, &["checkout", "-q", "-b", "feature"]);
    let sha = commit_file(&dir, &path, "a.txt", "feature change\n", "feature commit");
    git(&path, &["checkout", "-q", "main"]);

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
    let sha = commit_file(&dir, &path, "a.txt", "shared change\n", "shared commit");
    git(&path, &["checkout", "-q", "-b", "feature"]);

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
    commit_file(&dir, &path, "f.txt", "main version\n", "main edit");
    git(&path, &["checkout", "-q", "-b", "feature", "HEAD~1"]);
    let conflict_sha = commit_file(&dir, &path, "f.txt", "feature version\n", "feature edit");
    git(&path, &["checkout", "-q", "main"]);

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
    commit_file(&dir, &path, "x.txt", "x\n", "x");
    commit_file(&dir, &path, "y.txt", "y\n", "y");

    // Tag main HEAD as our "base remote ref" stand-in.
    git(&path, &["tag", "base"]);

    // Create a divergent target branch with a stale commit.
    git(&path, &["checkout", "-q", "-b", "target"]);
    commit_file(&dir, &path, "stale.txt", "stale\n", "stale on target");

    // Now force-recreate target from base — stale commit must vanish.
    checkout_force(&path, "target", "base").expect("checkout_force");

    // HEAD should be at base (not the stale commit).
    assert_eq!(rev_parse(&path, "HEAD"), rev_parse(&path, "base"));

    // The stale file must be gone.
    assert!(
        !dir.path().join("stale.txt").exists(),
        "stale file should be removed by force-checkout"
    );
}

// ---------------------------------------------------------------------------
// rebase
// ---------------------------------------------------------------------------

#[test]
fn rebase_rebuilds_target_without_editing_spec() {
    with_isolated_home(|home| {
        let (dir, path) = init_repo();
        git(&path, &["remote", "add", "origin", &path]);
        commit_file(&dir, &path, "base.txt", "base\n", "base commit");

        // Target starts stale and must be rebuilt from origin/main.
        git(&path, &["checkout", "-q", "-b", "stack-target"]);
        commit_file(&dir, &path, "stale.txt", "stale\n", "stale target commit");
        git(&path, &["checkout", "-q", "main"]);

        let spec = StackSpec {
            id: "rebase-no-edit".to_string(),
            description: "prove rebase does not mutate specs".to_string(),
            component: "homeboy".to_string(),
            component_path: path.clone(),
            base: GitRef {
                remote: "origin".to_string(),
                branch: "main".to_string(),
            },
            target: GitRef {
                remote: "origin".to_string(),
                branch: "stack-target".to_string(),
            },
            prs: Vec::new(),
        };
        save(&spec).expect("save stack spec");
        let spec_path = home
            .path()
            .join(".config/homeboy/stacks/rebase-no-edit.json");
        let before = fs::read_to_string(&spec_path).expect("read spec before rebase");

        let output = rebase(&spec).expect("rebase stack");
        assert!(output.success);
        assert_eq!(output.picked_count, 0);
        assert_eq!(output.skipped_count, 0);

        let after = fs::read_to_string(&spec_path).expect("read spec after rebase");
        assert_eq!(after, before, "stack rebase must not edit the spec file");

        assert_eq!(
            rev_parse(&path, "stack-target"),
            rev_parse(&path, "origin/main")
        );
        assert!(
            !dir.path().join("stale.txt").exists(),
            "rebase should recreate target from base and remove stale files"
        );
    });
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

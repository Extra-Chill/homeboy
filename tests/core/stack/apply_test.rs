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

use crate::stack::apply::{
    checkout_force, cherry_pick, cherry_pick_in_progress, conflict_error, conflict_guidance,
    ensure_no_cherry_pick_in_progress, rebase, url_matches, CherryPickResult, ConflictContext,
    ConflictPolicy,
};
use crate::stack::{save, GitRef, StackPrEntry, StackSpec};
use homeboy_core::test_support::with_isolated_home;
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
// conflict handling — policy + guidance
// ---------------------------------------------------------------------------

/// Drive a real cherry-pick conflict in a throwaway repo and hand back the
/// checkout plus the SHA that failed to apply.
fn repo_with_live_conflict() -> (tempfile::TempDir, String, String) {
    let (dir, path) = init_repo();
    commit_file(&dir, &path, "f.txt", "main version\n", "main edit");
    git(&path, &["checkout", "-q", "-b", "feature", "HEAD~1"]);
    let conflict_sha = commit_file(&dir, &path, "f.txt", "feature version\n", "feature edit");
    git(&path, &["checkout", "-q", "main"]);

    let result = cherry_pick(&path, &conflict_sha).expect("cherry_pick");
    assert!(
        matches!(result, CherryPickResult::Conflict(_)),
        "fixture should produce a conflict, got {:?}",
        result
    );
    assert!(
        cherry_pick_in_progress(&path),
        "fixture should leave a paused cherry-pick"
    );
    (dir, path, conflict_sha)
}

fn pr_entry() -> StackPrEntry {
    StackPrEntry {
        repo: "example-org/studio".to_string(),
        number: 3120,
        note: None,
    }
}

#[test]
fn conflict_error_preserves_state_by_default() {
    let (dir, path, sha) = repo_with_live_conflict();
    let pr = pr_entry();

    let error = conflict_error(
        ConflictContext {
            path: &path,
            stack_id: "demo",
            pr: &pr,
            sha: &sha,
            rerun_command: "homeboy stack apply demo",
            policy: ConflictPolicy::default(),
        },
        "CONFLICT (content): Merge conflict in f.txt",
    );

    // The conflict the message points at must still exist.
    assert!(
        cherry_pick_in_progress(&path),
        "default policy must not abort the in-progress cherry-pick"
    );
    assert!(
        dir.path().join(".git/CHERRY_PICK_HEAD").exists(),
        "CHERRY_PICK_HEAD must survive so `cherry-pick --continue` works"
    );
    let conflicted = fs::read_to_string(dir.path().join("f.txt")).expect("read conflicted file");
    assert!(
        conflicted.contains("<<<<<<<"),
        "conflict markers must survive; got: {conflicted}"
    );

    let rendered = error.to_string();
    assert!(
        rendered.contains("git -C") && rendered.contains("cherry-pick --continue"),
        "message must name the resolve command; got: {rendered}"
    );
    assert!(
        rendered.contains("cherry-pick --abort"),
        "message must name the bail-out command; got: {rendered}"
    );
    assert!(
        rendered.contains(&path),
        "message must name the repo path; got: {rendered}"
    );

    let _ = Command::new("git")
        .args(["cherry-pick", "--abort"])
        .current_dir(&path)
        .output();
}

#[test]
fn conflict_error_aborts_when_opted_in() {
    let (dir, path, sha) = repo_with_live_conflict();
    let pr = pr_entry();

    let error = conflict_error(
        ConflictContext {
            path: &path,
            stack_id: "demo",
            pr: &pr,
            sha: &sha,
            rerun_command: "homeboy stack apply demo",
            policy: ConflictPolicy::Abort,
        },
        "CONFLICT (content): Merge conflict in f.txt",
    );

    assert!(
        !cherry_pick_in_progress(&path),
        "--abort-on-conflict must abort the in-progress cherry-pick"
    );
    assert!(!dir.path().join(".git/CHERRY_PICK_HEAD").exists());
    let status = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(
        status.stdout.is_empty(),
        "abort should restore a clean tree; got: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    let rendered = error.to_string();
    assert!(
        rendered.contains("--abort-on-conflict"),
        "abort message must say the pick was aborted; got: {rendered}"
    );
    assert!(
        !rendered.contains("cherry-pick --continue"),
        "abort message must not point at state it just destroyed; got: {rendered}"
    );
}

#[test]
fn conflict_error_carries_pr_coordinates() {
    let (_dir, path) = init_repo();
    let pr = pr_entry();

    let error = conflict_error(
        ConflictContext {
            path: &path,
            stack_id: "demo",
            pr: &pr,
            sha: "deadbeef",
            rerun_command: "homeboy stack sync demo",
            policy: ConflictPolicy::Preserve,
        },
        "CONFLICT (content): Merge conflict in f.txt",
    );

    assert_eq!(
        error.code,
        homeboy_core::error::ErrorCode::StackApplyConflict
    );
    let rendered = error.to_string();
    assert!(rendered.contains("example-org/studio#3120"), "{rendered}");
    assert!(rendered.contains("deadbeef"), "{rendered}");
    assert!(rendered.contains("homeboy stack sync demo"), "{rendered}");
}

#[test]
fn conflict_guidance_preserve_and_abort_differ() {
    let pr = pr_entry();
    fn ctx<'a>(pr: &'a StackPrEntry, policy: ConflictPolicy) -> ConflictContext<'a> {
        ConflictContext {
            path: "/tmp/checkout",
            stack_id: "demo",
            pr,
            sha: "abc1234",
            rerun_command: "homeboy stack apply demo",
            policy,
        }
    }

    let preserve = conflict_guidance(&ctx(&pr, ConflictPolicy::Preserve), "CONFLICT in f.txt");
    assert!(preserve.contains("still in progress in /tmp/checkout"));
    assert!(preserve.contains("git -C /tmp/checkout cherry-pick --continue"));
    assert!(preserve.contains("git -C /tmp/checkout cherry-pick --abort"));

    let abort = conflict_guidance(&ctx(&pr, ConflictPolicy::Abort), "CONFLICT in f.txt");
    assert!(abort.contains("git cherry-pick --abort"));
    assert!(abort.contains("/tmp/checkout"));
    assert!(!abort.contains("cherry-pick --continue"));
}

#[test]
fn conflict_policy_maps_the_cli_flag() {
    assert_eq!(
        ConflictPolicy::from_abort_flag(false),
        ConflictPolicy::Preserve
    );
    assert_eq!(ConflictPolicy::from_abort_flag(true), ConflictPolicy::Abort);
    assert_eq!(ConflictPolicy::default(), ConflictPolicy::Preserve);
}

// ---------------------------------------------------------------------------
// ensure_no_cherry_pick_in_progress
// ---------------------------------------------------------------------------

#[test]
fn rebuild_preflight_allows_a_clean_checkout() {
    let (_dir, path) = init_repo();
    assert!(!cherry_pick_in_progress(&path));
    ensure_no_cherry_pick_in_progress(&path, "homeboy stack apply demo")
        .expect("clean checkout should pass the preflight");
}

#[test]
fn rebuild_preflight_refuses_to_clobber_a_preserved_conflict() {
    let (_dir, path, _sha) = repo_with_live_conflict();

    let error = ensure_no_cherry_pick_in_progress(&path, "homeboy stack apply demo")
        .expect_err("paused cherry-pick must block a rebuild");
    let rendered = error.to_string();
    assert!(rendered.contains(&path), "{rendered}");
    assert!(rendered.contains("cherry-pick --continue"), "{rendered}");
    assert!(rendered.contains("homeboy stack apply demo"), "{rendered}");

    // The preflight is read-only — the conflict is still there afterwards.
    assert!(cherry_pick_in_progress(&path));

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

        let output = rebase(&spec, ConflictPolicy::default()).expect("rebase stack");
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
        "https://github.com/example-org/studio.git",
        "https://github.com/example-org/studio"
    ));
    assert!(url_matches(
        "https://github.com/example-org/studio",
        "https://github.com/example-org/studio.git"
    ));
}

#[test]
fn url_matches_https_vs_ssh() {
    assert!(url_matches(
        "https://github.com/example-org/studio.git",
        "git@github.com:example-org/studio.git"
    ));
}

#[test]
fn url_matches_case_insensitive() {
    assert!(url_matches(
        "https://github.com/EXAMPLE-ORG/STUDIO.git",
        "https://github.com/example-org/studio"
    ));
}

#[test]
fn url_matches_rejects_different_repos() {
    assert!(!url_matches(
        "https://github.com/example-org/studio",
        "https://github.com/example-org/playground"
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

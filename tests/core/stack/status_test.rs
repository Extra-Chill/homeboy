//! Tests for `core::stack::status` — read-only stack status.
//!
//! Includes focused end-to-end status coverage with a fake `gh`, plus the
//! deterministic git-side helpers that build local-state columns.

use crate::stack::status::{
    commit_reachable, count_revs, git_ref_exists, patch_in_base, status, LocalState,
};
use crate::stack::{GitRef, StackPrEntry, StackSpec};
use homeboy_core::test_support::home_env_guard;
use std::fs;

mod support;
use support::{commit_file, git, init_repo};

fn stack_spec(id: &str, component_path: String) -> StackSpec {
    StackSpec {
        id: id.to_string(),
        description: String::new(),
        component: "homeboy".to_string(),
        component_path,
        base: GitRef {
            remote: "origin".to_string(),
            branch: "main".to_string(),
        },
        target: GitRef {
            remote: "origin".to_string(),
            branch: "main".to_string(),
        },
        prs: vec![StackPrEntry {
            repo: "Extra-Chill/homeboy".to_string(),
            number: 11410,
            note: None,
        }],
    }
}

fn with_fake_gh(stdout: &str, exit_code: u8, run: impl FnOnce()) {
    let _guard = home_env_guard();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let gh = dir.path().join("gh");
    fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\nexit {}\n",
            stdout, exit_code
        ),
    )
    .expect("write fake gh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755))
            .expect("make fake gh executable");
    }

    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", dir.path().display(), old_path));
    run();
    std::env::set_var("PATH", old_path);
}

const PR_JSON: &str = r#"{"headRefOid":"deadbeef","state":"OPEN","title":"Checkout-free status","url":"https://github.com/Extra-Chill/homeboy/pull/11410","reviewDecision":"APPROVED","mergedAt":null}"#;

#[test]
fn status_reports_upstream_and_local_evidence_for_present_checkout() {
    let (_dir, path) = init_repo();
    with_fake_gh(PR_JSON, 0, || {
        let report = status(&stack_spec("present", path.clone())).expect("status report");
        assert!(report.local_evidence_unavailable.is_none());
        assert_eq!(report.prs[0].upstream_state.as_deref(), Some("OPEN"));
        assert_eq!(report.prs[0].local_state, LocalState::Unknown);
    });
}

#[test]
fn status_reports_github_metadata_when_checkout_is_missing() {
    let path = "/definitely/missing/homeboy-stack-status".to_string();
    with_fake_gh(PR_JSON, 0, || {
        let report = status(&stack_spec("missing", path.clone())).expect("status report");
        assert_eq!(report.prs[0].upstream_state.as_deref(), Some("OPEN"));
        assert_eq!(report.prs[0].local_state, LocalState::Unknown);
        assert_eq!(report.target_ahead, None);
        let evidence = report
            .local_evidence_unavailable
            .expect("missing checkout evidence");
        assert_eq!(
            evidence.reason,
            format!("Component path '{}' does not exist", path)
        );
        assert_eq!(
            evidence.recovery_commands[0],
            format!("git clone <repository-url> {}", path)
        );
    });
}

#[test]
fn status_reports_local_evidence_unavailable_for_non_git_directory() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().to_string_lossy().to_string();
    fs::write(dir.path().join("not-a-repository.txt"), "local files").expect("write fixture");

    with_fake_gh(PR_JSON, 0, || {
        let report = status(&stack_spec("non-git", path.clone())).expect("status report");
        assert_eq!(report.prs[0].upstream_state.as_deref(), Some("OPEN"));
        assert_eq!(report.prs[0].local_state, LocalState::Unknown);
        assert_eq!(report.target_ahead, None);
        assert_eq!(report.target_behind, None);
        assert_eq!(
            report
                .local_evidence_unavailable
                .expect("non-Git checkout evidence")
                .reason,
            format!("Component path '{}' is not a Git checkout", path)
        );
    });
}

#[test]
fn status_reports_both_unavailable_evidence_when_github_lookup_fails() {
    let path = "/definitely/missing/homeboy-stack-status".to_string();
    with_fake_gh("GitHub unavailable", 1, || {
        let report = status(&stack_spec("missing", path)).expect("status report");
        assert!(report.prs[0].upstream_state.is_none());
        assert_eq!(report.prs[0].local_state, LocalState::Unknown);
        assert!(report.prs[0]
            .error
            .as_deref()
            .unwrap()
            .contains("gh pr view Extra-Chill/homeboy#11410 failed"));
        assert!(report.local_evidence_unavailable.is_some());
    });
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

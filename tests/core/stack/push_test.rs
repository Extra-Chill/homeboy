//! Tests for `core::stack::push` — publishing the materialized target branch.

use crate::stack::push::{local_branch_ref, push, remote_branch_ref, PushStatus};
use crate::stack::spec::{GitRef, StackSpec};
use std::process::Command;
use tempfile::TempDir;

mod support;
use support::{commit_file, git, init_repo, rev_parse};

fn init_bare_remote() -> TempDir {
    let remote = TempDir::new().expect("remote tempdir");
    let out = Command::new("git")
        .args(["init", "--bare", "-q"])
        .current_dir(remote.path())
        .output()
        .expect("git init --bare");
    assert!(
        out.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    remote
}

fn stack_spec(path: &str) -> StackSpec {
    StackSpec {
        id: "test-stack".to_string(),
        description: String::new(),
        component: "homeboy-test".to_string(),
        component_path: path.to_string(),
        base: GitRef {
            remote: "origin".to_string(),
            branch: "main".to_string(),
        },
        target: GitRef {
            remote: "origin".to_string(),
            branch: "dev/combined-fixes".to_string(),
        },
        prs: Vec::new(),
    }
}

fn add_origin(path: &str, remote: &TempDir) {
    let remote_path = remote.path().to_string_lossy().to_string();
    git(path, &["remote", "add", "origin", &remote_path]);
}

#[test]
fn push_updates_remote_target_branch_with_force_with_lease() {
    let (dir, path) = init_repo();
    let remote = init_bare_remote();
    add_origin(&path, &remote);

    git(&path, &["checkout", "-q", "-b", "dev/combined-fixes"]);
    let before = commit_file(&dir, &path, "old.txt", "old\n", "old target");
    git(
        &path,
        &[
            "push",
            "origin",
            "refs/heads/dev/combined-fixes:refs/heads/dev/combined-fixes",
        ],
    );

    git(&path, &["checkout", "-q", "main"]);
    git(
        &path,
        &["checkout", "-q", "-B", "dev/combined-fixes", "main"],
    );
    let after = commit_file(&dir, &path, "new.txt", "new\n", "rebuilt target");

    let report = push(&stack_spec(&path)).expect("stack push");

    assert_eq!(report.stack_id, "test-stack");
    assert_eq!(report.remote, "origin");
    assert_eq!(report.branch, "dev/combined-fixes");
    assert_eq!(report.before_ref.as_deref(), Some(before.as_str()));
    assert_eq!(report.after_ref, after);
    assert_eq!(report.status, PushStatus::Updated);
    assert!(report.success);
    assert_eq!(
        remote_branch_ref(&path, "origin", "dev/combined-fixes")
            .expect("remote ref")
            .as_deref(),
        Some(after.as_str())
    );
}

#[test]
fn push_reports_unchanged_when_remote_already_matches_local_target() {
    let (dir, path) = init_repo();
    let remote = init_bare_remote();
    add_origin(&path, &remote);

    git(&path, &["checkout", "-q", "-b", "dev/combined-fixes"]);
    let head = commit_file(&dir, &path, "same.txt", "same\n", "same target");
    git(
        &path,
        &[
            "push",
            "origin",
            "refs/heads/dev/combined-fixes:refs/heads/dev/combined-fixes",
        ],
    );

    let report = push(&stack_spec(&path)).expect("stack push");

    assert_eq!(report.before_ref.as_deref(), Some(head.as_str()));
    assert_eq!(report.after_ref, head);
    assert_eq!(report.status, PushStatus::Unchanged);
    assert!(report.success);
}

#[test]
fn push_creates_remote_target_branch_when_missing() {
    let (dir, path) = init_repo();
    let remote = init_bare_remote();
    add_origin(&path, &remote);

    git(&path, &["checkout", "-q", "-b", "dev/combined-fixes"]);
    let head = commit_file(&dir, &path, "first.txt", "first\n", "first target");

    let report = push(&stack_spec(&path)).expect("stack push");

    assert!(report.before_ref.is_none());
    assert_eq!(report.after_ref, head);
    assert_eq!(report.status, PushStatus::Updated);
    assert_eq!(
        remote_branch_ref(&path, "origin", "dev/combined-fixes")
            .expect("remote ref")
            .as_deref(),
        Some(head.as_str())
    );
}

#[test]
fn local_branch_ref_errors_when_target_branch_missing() {
    let (_dir, path) = init_repo();
    let err = local_branch_ref(&path, "dev/combined-fixes").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("rev-parse") && msg.contains("dev/combined-fixes"),
        "expected missing local branch error, got: {}",
        msg
    );
}

#[test]
fn local_branch_ref_reads_multi_segment_branch_name() {
    let (dir, path) = init_repo();
    git(&path, &["checkout", "-q", "-b", "dev/combined-fixes"]);
    let head = commit_file(&dir, &path, "branch.txt", "branch\n", "branch commit");

    assert_eq!(local_branch_ref(&path, "dev/combined-fixes").unwrap(), head);
    assert_eq!(rev_parse(&path, "refs/heads/dev/combined-fixes"), head);
}

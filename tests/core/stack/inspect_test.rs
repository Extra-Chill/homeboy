//! Tests for `core::stack::inspect` — the spec-less commit inspector
//! (formerly `core::git::stack`, re-homed under `core::stack`).
//!
//! These cover the moved-but-unchanged behaviour: empty stack, oldest-first
//! ordering, no-upstream error, bad-base-ref error. PR-decoration paths are
//! intentionally not exercised end-to-end (they require a live `gh` and
//! GitHub) — `no_pr: true` keeps tests deterministic.

use crate::stack::inspect::{inspect_at, InspectOptions};
use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Create a fresh git repo with a single committed file.
fn init_repo() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().to_string_lossy().to_string();
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&path)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&path)
        .output()
        .unwrap();
    fs::write(dir.path().join("README.md"), "initial\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "initial"])
        .current_dir(&path)
        .output()
        .unwrap();
    (dir, path)
}

fn add_commit(dir: &TempDir, path: &str, file: &str, contents: &str, message: &str) {
    fs::write(dir.path().join(file), contents).unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", message])
        .current_dir(path)
        .output()
        .unwrap();
}

#[test]
fn empty_stack_when_branch_is_at_base() {
    let (_dir, path) = init_repo();

    let out = inspect_at(
        None,
        InspectOptions {
            base: Some("HEAD".to_string()),
            no_pr: true,
            ..Default::default()
        },
        Some(&path),
    )
    .expect("inspect_at");

    assert_eq!(out.commits.len(), 0);
    assert_eq!(out.base, "HEAD");
    assert!(
        !out.base_auto_detected,
        "explicit --base should not auto-detect"
    );
    assert!(out.success);
}

#[test]
fn lists_commits_oldest_first_over_explicit_base() {
    let (dir, path) = init_repo();
    Command::new("git")
        .args(["tag", "base"])
        .current_dir(&path)
        .output()
        .unwrap();

    add_commit(&dir, &path, "a.txt", "a\n", "first new");
    add_commit(&dir, &path, "b.txt", "b\n", "second new");
    add_commit(&dir, &path, "c.txt", "c\n", "third new");

    let out = inspect_at(
        None,
        InspectOptions {
            base: Some("base".to_string()),
            no_pr: true,
            ..Default::default()
        },
        Some(&path),
    )
    .expect("inspect_at");

    assert_eq!(out.commits.len(), 3);
    assert_eq!(out.commits[0].commit.subject, "first new");
    assert_eq!(out.commits[1].commit.subject, "second new");
    assert_eq!(out.commits[2].commit.subject, "third new");
    for c in &out.commits {
        assert_eq!(c.commit.short_sha.len(), 7);
        assert!(c.pr.is_none());
        assert!(c.pr_lookup_note.is_none());
    }
    assert_eq!(out.merged_count, 0);
}

#[test]
fn errors_helpfully_when_no_upstream_and_no_base_arg() {
    let (_dir, path) = init_repo();

    let err = inspect_at(None, InspectOptions::default(), Some(&path))
        .expect_err("inspect_at should Err without upstream or --base");

    let msg = err.to_string();
    assert!(
        msg.contains("upstream") || msg.contains("--base"),
        "expected helpful error, got: {}",
        msg
    );
}

#[test]
fn errors_when_base_ref_does_not_exist() {
    let (_dir, path) = init_repo();

    let err = inspect_at(
        None,
        InspectOptions {
            base: Some("does-not-exist".to_string()),
            no_pr: true,
            ..Default::default()
        },
        Some(&path),
    )
    .expect_err("inspect_at should Err on bad base ref");

    let msg = err.to_string();
    assert!(
        msg.contains("does-not-exist") || msg.contains("not found"),
        "expected ref-not-found error, got: {}",
        msg
    );
}

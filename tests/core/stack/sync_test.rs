//! Tests for `core::stack::sync` — the drop-decision logic.
//!
//! Like `apply_test.rs` and `status_test.rs`, these tests cover the
//! deterministic git-side helpers (`is_droppable`) without invoking `gh`.
//! The full `sync()` entry point is verified out-of-band against a real
//! GitHub fixture (PR #1543, squash-merged) — see the PR body's
//! "Live verification" section.
//!
//! `is_droppable` is the heart of `sync`'s behaviour: given pre-fetched
//! PR metadata + a base ref, decide whether a PR should be auto-removed
//! from the spec. The cherry-pick orchestration after that decision is
//! the same machinery `apply` uses (already tested in `apply_test.rs`).

use crate::stack::sync::{is_droppable, PrMeta};
use std::fs;
use std::process::Command;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers (intentionally duplicated from apply_test.rs / status_test.rs so
// each test file is self-contained — same convention as core::rig tests).
// ---------------------------------------------------------------------------

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

/// Build a PrMeta with overridable fields; defaults to a MERGED PR with
/// a placeholder SHA.
fn meta(state: &str, head_sha: &str) -> PrMeta {
    PrMeta {
        head_sha: head_sha.to_string(),
        head_owner: "Automattic".to_string(),
        head_name: "studio".to_string(),
        state: state.to_string(),
        title: Some("test PR".to_string()),
        merged_at: if state == "MERGED" {
            Some("2026-04-26T00:00:00Z".to_string())
        } else {
            None
        },
    }
}

// ---------------------------------------------------------------------------
// is_droppable — the drop-decision contract
// ---------------------------------------------------------------------------

#[test]
fn is_droppable_drops_merged_pr_with_head_reachable_from_base() {
    let (dir, path) = init_repo();
    // Simulate: PR's commit landed on main directly (the non-squash case).
    let sha = write_and_commit(&dir, &path, "a.txt", "a\n", "PR #1 commit on main");

    let meta = meta("MERGED", &sha);
    assert!(
        is_droppable(&meta, &path, "main"),
        "merged PR with head SHA in base should be droppable"
    );
}

#[test]
fn is_droppable_drops_merged_pr_with_squash_merged_content() {
    let (dir, path) = init_repo();
    // PR's head SHA on a feature branch (NOT in base)…
    run(&path, &["checkout", "-q", "-b", "pr-feature"]);
    let pr_head = write_and_commit(&dir, &path, "feat.txt", "feature\n", "PR #1 head");

    // …but main got a squash-merge with the same tree.
    run(&path, &["checkout", "-q", "main"]);
    fs::write(dir.path().join("feat.txt"), "feature\n").unwrap();
    run(&path, &["add", "."]);
    run(&path, &["commit", "-q", "-m", "Squash-merge PR #1"]);

    let meta = meta("MERGED", &pr_head);
    assert!(
        is_droppable(&meta, &path, "main"),
        "merged PR with squash-merged content should be droppable via patch_in_base"
    );
}

#[test]
fn is_droppable_keeps_open_pr() {
    let (dir, path) = init_repo();
    let sha = write_and_commit(&dir, &path, "a.txt", "a\n", "PR commit on main");

    // Even if content is in base, an OPEN PR is never droppable — the
    // reviewer may have cherry-picked early to a release branch.
    let meta = meta("OPEN", &sha);
    assert!(
        !is_droppable(&meta, &path, "main"),
        "OPEN PR must stay in spec regardless of base content"
    );
}

#[test]
fn is_droppable_keeps_closed_pr() {
    let (dir, path) = init_repo();
    let sha = write_and_commit(&dir, &path, "a.txt", "a\n", "commit");

    // A CLOSED-without-merge PR isn't droppable — the user may have a
    // reason for keeping the cherry-pick locally even after the upstream
    // closed.
    let meta = meta("CLOSED", &sha);
    assert!(
        !is_droppable(&meta, &path, "main"),
        "CLOSED PR must stay in spec — only MERGED qualifies for auto-drop"
    );
}

#[test]
fn is_droppable_keeps_merged_pr_when_content_not_in_base() {
    let (dir, path) = init_repo();
    // PR head SHA exists locally on a side branch, but main has no
    // equivalent commit.
    run(&path, &["checkout", "-q", "-b", "side"]);
    let pr_head = write_and_commit(&dir, &path, "x.txt", "x\n", "side commit");
    run(&path, &["checkout", "-q", "main"]);

    // Bizarre rebase-and-force-push scenario: gh says MERGED but the
    // content isn't anywhere in base. We keep the PR so the user
    // doesn't lose their cherry-pick by accident.
    let meta = meta("MERGED", &pr_head);
    assert!(
        !is_droppable(&meta, &path, "main"),
        "merged PR whose content isn't in base must stay (rebase-force-push edge case)"
    );
}

#[test]
fn is_droppable_keeps_pr_with_unknown_head_sha() {
    let (_dir, path) = init_repo();
    // SHA shape is valid hex but not in the local object store —
    // commit_reachable returns None, patch_in_base returns None.
    // Without local information, we must NOT drop.
    let meta = meta("MERGED", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    assert!(
        !is_droppable(&meta, &path, "main"),
        "PR with unfetchable head SHA must stay — no local evidence to drop on"
    );
}

#[test]
fn is_droppable_keeps_pr_with_empty_head_sha() {
    let (_dir, path) = init_repo();
    // Defensive: if `gh pr view` returned an empty headRefOid we have
    // nothing to compare against.
    let meta = meta("MERGED", "");
    assert!(
        !is_droppable(&meta, &path, "main"),
        "PR with empty head SHA must stay"
    );
}

#[test]
fn is_droppable_state_check_is_case_sensitive() {
    let (dir, path) = init_repo();
    let sha = write_and_commit(&dir, &path, "a.txt", "a\n", "commit");

    // gh returns canonical uppercase ("MERGED"). Lower/mixed case is
    // either a bug in the caller or upstream — refuse to drop.
    let mixed_case = meta("Merged", &sha);
    assert!(
        !is_droppable(&mixed_case, &path, "main"),
        "is_droppable must match gh's canonical 'MERGED' exactly"
    );

    let lower_case = meta("merged", &sha);
    assert!(
        !is_droppable(&lower_case, &path, "main"),
        "is_droppable must match gh's canonical 'MERGED' exactly"
    );
}

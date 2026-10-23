//! Candidate-commit admission refusals (#16303).
#![cfg(test)]

use super::super::promote::promote_with_provider;
use super::super::types::AgentTaskPromotionRequest;
use super::*;
use crate::agent_task::AgentTaskCandidate;

fn refuse(repo: &Path, candidate: &AgentTaskCandidate, reason: &str) -> homeboy_core::Error {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider::default();
    let error = promote_with_provider(
        AgentTaskPromotionRequest {
            source: with_candidate(&source, candidate),
            source_path: Some(source_path),
            gates: crate::agent_task_gate::VerifyGateOptions {
                verify: vec!["true".to_string()],
                ..Default::default()
            },
            ..promotion_options(&repo.display().to_string())
        },
        &mut provider,
    )
    .expect_err("candidate admission refuses");
    assert_eq!(error.details["field"], "promotion.candidate", "{error:?}");
    assert_eq!(error.details["reason"], reason, "{error:?}");
    assert!(provider.apply_calls.is_empty());
    assert!(provider.verify_calls.is_empty());
    error
}

#[test]
fn moved_destination_head_refuses_the_candidate() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    std::fs::write(repo.join("other.txt"), "moved\n").expect("write");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "destination moved"]);
    let moved = git_head(&repo, "HEAD");

    refuse(&repo, &candidate, "destination_moved");

    assert_eq!(git_head(&repo, "HEAD"), moved);
    assert_eq!(git_stdout(&repo, &["status", "--porcelain"]), "");
}

#[test]
fn dirty_destination_refuses_the_candidate() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    std::fs::write(repo.join("src/lib.rs"), "operator edit\n").expect("write");

    refuse(&repo, &candidate, "destination_moved");

    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        "operator edit\n"
    );
}

#[test]
fn candidate_tree_mismatch_is_refused() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let mut candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    candidate.tree_sha = git_stdout(&repo, &["rev-parse", "HEAD^{tree}"]);

    refuse(&repo, &candidate, "tree_mismatch");

    assert_eq!(git_stdout(&repo, &["status", "--porcelain"]), "");
}

#[test]
fn missing_candidate_commit_is_refused() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let mut candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    candidate.candidate_sha = "0123456789abcdef0123456789abcdef01234567".to_string();

    refuse(&repo, &candidate, "missing_commit");
}

#[test]
fn patch_only_outcome_is_not_promotable() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider::default();

    let error = promote_with_provider(
        AgentTaskPromotionRequest {
            source,
            source_path: Some(source_path),
            ..promotion_options(&repo.display().to_string())
        },
        &mut provider,
    )
    .expect_err("records without a candidate commit are refused");

    assert!(
        error.message.contains("no harvested candidate commit"),
        "{}",
        error.message
    );
    assert!(provider.apply_calls.is_empty());
    assert_eq!(git_stdout(&repo, &["status", "--porcelain"]), "");
}

/// A gate-feedback base can track files the destination's own index does not;
/// materializing a candidate that deletes one still yields the candidate tree.
#[test]
fn candidate_deleting_an_untracked_base_file_materializes_its_tree() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    std::fs::write(repo.join("prior.txt"), "prior candidate\n").expect("write prior");
    let prior = harvest_worktree_candidate(&repo, &git_head(&repo, "HEAD"));
    let index = tempfile::NamedTempFile::new().expect("index");
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_INDEX_FILE", index.path())
            .output()
            .expect("git");
        assert!(output.status.success(), "git {}", args.join(" "));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    run(&["read-tree", &prior.candidate_sha]);
    run(&["rm", "--cached", "-q", "prior.txt"]);
    let tree_sha = run(&["write-tree"]);
    let candidate_sha = run(&[
        "commit-tree",
        &tree_sha,
        "-p",
        &prior.candidate_sha,
        "-m",
        "drop",
    ]);
    let candidate = AgentTaskCandidate {
        base_sha: prior.candidate_sha,
        candidate_sha,
        tree_sha,
    };
    let (source_path, source) = write_empty_patch_source(&temp);

    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source: with_candidate(&source, &candidate),
            source_path: Some(source_path),
            ..promotion_options(&repo.display().to_string())
        },
        &mut FakePromotionWorkspaceProvider::default(),
    )
    .expect("candidate materializes over the gate-feedback base");

    assert_eq!(report.changed_files, vec!["prior.txt"]);
    assert!(!repo.join("prior.txt").exists());
}

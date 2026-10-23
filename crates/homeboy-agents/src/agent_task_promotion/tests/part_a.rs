//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::promote::{
    normalize_promotion_patch, promote_with_provider, promote_with_provider_and_checkpoint,
    promote_with_provider_in_observation_store, resume_promoted_patch,
};
use super::super::types::{AgentTaskPromotionRequest, AgentTaskPromotionStatus};
use super::*;
use crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA;
use crate::agent_task_gate::{AgentTaskGateRevealPolicy, VerifyGateOptions};
use serde_json::Value;
use std::process::Command;

/// The harvested commit is the candidate (#16303): a recovered runner
/// aggregate promotes from it whether or not its patch projection is present.
#[test]
fn candidate_promotion_ignores_missing_or_mismatched_recovered_controller_projection() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let observation_store =
            homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let run_id = "recovered-lab-run";
        let task_id = "implement";
        let artifact_id = "patch";
        let temp = tempfile::tempdir().expect("promotion tempdir");
        let repo = candidate_destination(temp.path());
        let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
        let source = with_candidate(
            &recovered_runner_aggregate(
                task_id,
                artifact_id,
                &sha256_hex(VALID_PATCH),
                VALID_PATCH.len(),
            ),
            &candidate,
        );
        let mut provider = FakePromotionWorkspaceProvider::default();
        let options = || AgentTaskPromotionRequest {
            source: source.clone(),
            source_run_id: Some(run_id.to_string()),
            task_id: Some(task_id.to_string()),
            artifact_id: Some(artifact_id.to_string()),
            ..promotion_options(&repo.display().to_string())
        };

        let missing = promote_with_provider_in_observation_store(
            options(),
            &mut provider,
            &observation_store,
        )
        .expect("missing projection does not block the candidate commit");
        assert_eq!(missing.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(missing.changed_files, vec!["src/lib.rs"]);

        git(&repo, &["checkout", "--", "."]);
        record_controller_projection(run_id, task_id, artifact_id, "different patch bytes");
        let mismatched = promote_with_provider_in_observation_store(
            options(),
            &mut provider,
            &observation_store,
        )
        .expect("mismatched projection does not block the candidate commit");
        assert_eq!(mismatched.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(
            std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
            "new\n"
        );
    });
}

#[test]
fn promotion_uses_recovered_controller_projection_without_public_artifact_path() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let observation_store =
            homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let run_id = "recovered-lab-run";
        let task_id = "implement";
        let artifact_id = "patch";
        let temp = tempfile::tempdir().expect("promotion tempdir");
        let repo = candidate_destination(temp.path());
        let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
        let mut source: Value = serde_json::from_str(&recovered_runner_aggregate(
            task_id,
            artifact_id,
            &sha256_hex(VALID_PATCH),
            VALID_PATCH.len(),
        ))
        .expect("aggregate JSON");
        source["outcomes"][0]["artifacts"][0]
            .as_object_mut()
            .expect("patch artifact")
            .remove("path");
        record_controller_projection(run_id, task_id, artifact_id, VALID_PATCH);
        let mut provider = FakePromotionWorkspaceProvider::default();

        let result = promote_with_provider_in_observation_store(
            AgentTaskPromotionRequest {
                source: with_candidate(&source.to_string(), &candidate),
                source_run_id: Some(run_id.to_string()),
                task_id: Some(task_id.to_string()),
                artifact_id: Some(artifact_id.to_string()),
                ..promotion_options(&repo.display().to_string())
            },
            &mut provider,
            &observation_store,
        )
        .expect("candidate is promoted without a public artifact path");

        assert_eq!(result.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(result.patch_artifact.id, artifact_id);
        assert_eq!(
            result.provenance["candidate_commit"]["candidate_sha"],
            candidate.candidate_sha
        );
    });
}

#[test]
fn normalize_promotion_patch_leaves_unrelated_workspace_paths() {
    let patch = "diff --git a/workspace/fixture.txt b/workspace/fixture.txt\n--- a/workspace/fixture.txt\n+++ b/workspace/fixture.txt\n@@ -1 +1 @@\n-old\n+new\n";

    let normalized = normalize_promotion_patch(patch, "homeboy@promoted-task")
        .expect("unrelated workspace path remains repo-relative");

    assert_eq!(normalized.changed_files, vec!["workspace/fixture.txt"]);
    assert_eq!(normalized.content, patch);
}

#[test]
fn empty_patch_failing_gate_is_reported_against_destination() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "homeboy@example.test"]);
    git(&repo, &["config", "user.name", "Homeboy Test"]);
    std::fs::write(repo.join("lib.rs"), "candidate\n").expect("write candidate");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "candidate"]);
    let (source_path, source) = write_empty_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        verify_exit_code: 7,
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source,
            source_run_id: Some("run-empty-fail".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "repo@candidate".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["failing-check".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                ..Default::default()
            },
            provider_invocation: None,
            repository_integrity_evidence: None,
        },
        &mut provider,
    )
    .expect("failed no-op gate is recorded");

    assert_eq!(report.status, AgentTaskPromotionStatus::NoChangesGateFailed);
    assert_eq!(
        report.deterministic_gates[0].status,
        crate::agent_task_gate::AgentTaskGateStatus::Failed
    );
    assert_eq!(
        report.gate_results[0].status,
        homeboy_core::gate::HomeboyGateStatus::Failed
    );
    assert_eq!(provider.verify_calls[0].0, repo);
}

/// #16303: a fresh promotion admits the harvested candidate commit; it needs
/// no patch artifact and keeps the destination branch checked out.
#[test]
fn candidate_without_patch_artifact_promotes_from_the_commit() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let remote = temp.path().join("origin.git");
    assert!(Command::new("git")
        .args(["init", "--bare", remote.to_str().expect("remote path")])
        .status()
        .expect("create remote")
        .success());
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git(&repo, &["push", "-q", "-u", "origin", "main"]);
    let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    let source_path = temp.path().join("outcome.json");
    let source = with_candidate(
        &serde_json::json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "task-1",
            "status": "succeeded",
            "artifacts": []
        })
        .to_string(),
        &candidate,
    );
    std::fs::write(&source_path, &source).expect("write source");
    let mut provider = FakePromotionWorkspaceProvider::default();
    let options = |base_ref: &str| AgentTaskPromotionRequest {
        source: source.clone(),
        source_run_id: Some("run-no-artifact".to_string()),
        source_path: Some(source_path.clone()),
        base_ref: Some(base_ref.to_string()),
        ..promotion_options(&repo.display().to_string())
    };

    let error = promote_with_provider(options("trunk"), &mut provider)
        .expect_err("invalid base rejects the candidate before materialization");
    assert!(error
        .message
        .contains("declared base `trunk` was not found"));
    assert_eq!(git_stdout(&repo, &["status", "--porcelain"]), "");

    let report = promote_with_provider(options("main"), &mut provider)
        .expect("candidate commit is promoted without a patch artifact");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.patch_artifact.kind, "candidate_commit");
    assert_eq!(report.patch_artifact.id, candidate.candidate_sha);
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(report.provenance["change_source"], "candidate_commit");
    assert_eq!(report.target.branch.as_deref(), Some("main"));
    assert_eq!(git_head(&repo, "HEAD"), candidate.base_sha);
    assert_eq!(
        git_stdout(&repo, &["status", "--porcelain"]),
        " M src/lib.rs\n"
    );
}

#[test]
fn normalize_promotion_patch_rejects_repo_sandbox_without_relative_suffix() {
    let patch = "diff --git a/workspace/homeboy-refactor b/workspace/homeboy-refactor\n--- a/workspace/homeboy-refactor\n+++ b/workspace/homeboy-refactor\n@@ -1 +1 @@\n-old\n+new\n";

    let err = normalize_promotion_patch(patch, "homeboy@promoted-task")
        .expect_err("repo sandbox path without suffix rejected");

    assert!(err.message.contains("no repo-relative suffix"));
}

#[test]
fn spoofed_generated_patch_provenance_does_not_change_promotion_artifact_id() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = candidate_destination(temp.path());
    let candidate = harvest_patch_candidate(&repo, VALID_PATCH);
    let (source_path, source) = write_patch_source(&temp);
    let mut source: Value =
        serde_json::from_str(&with_candidate(&source, &candidate)).expect("source JSON");
    source["artifacts"][0]["id"] = Value::String("task-1-attempt-1-committed-changes".to_string());
    source["artifacts"][0]["metadata"] = serde_json::json!({
        "artifact_provenance": "homeboy_generated_committed_patch",
        "task_id": "task-1",
        "producer_attempt": 1
    });
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("controlled-worktree")),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionRequest {
            source: source.to_string(),
            source_run_id: None,
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: repo.display().to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: true,
            gates: VerifyGateOptions {
                verify: Vec::new(),
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                ..Default::default()
            },
            provider_invocation: None,
            repository_integrity_evidence: None,
        },
        &mut provider,
    )
    .expect("dry-run promotion report");

    assert_eq!(
        report.patch_artifact.id,
        "task-1-attempt-1-committed-changes"
    );
}

#[test]
fn promotion_checkpoints_applied_target_before_gate_transport_failure() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree_path = temp.path().join("managed-target");
    std::fs::create_dir(&worktree_path).expect("create target");
    git(&worktree_path, &["init"]);
    git(
        &worktree_path,
        &["config", "user.email", "test@example.com"],
    );
    git(&worktree_path, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(worktree_path.join("src")).expect("source dir");
    std::fs::write(worktree_path.join("src/lib.rs"), "old\n").expect("base source");
    git(&worktree_path, &["add", "."]);
    git(&worktree_path, &["commit", "-m", "base"]);
    git(&worktree_path, &["branch", "-M", "main"]);
    let remote = temp.path().join("origin.git");
    assert!(Command::new("git")
        .args(["init", "--bare", remote.to_str().expect("remote path")])
        .status()
        .expect("create remote")
        .success());
    git(
        &worktree_path,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git(&worktree_path, &["push", "-u", "origin", "main"]);
    let candidate = harvest_patch_candidate(&worktree_path, VALID_PATCH);
    let (source_path, source) = write_patch_source(&temp);
    let source = with_candidate(&source, &candidate);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(worktree_path.clone()),
        verify_transport_error: true,
        ..Default::default()
    };
    let mut checkpoints = Vec::new();

    let error = promote_with_provider_and_checkpoint(
        AgentTaskPromotionRequest {
            source: source.clone(),
            source_run_id: Some("restartable-run".to_string()),
            source_path: Some(source_path.clone()),
            source_worktree_path: None,
            base_ref: Some("main".to_string()),
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: worktree_path.display().to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["true".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                ..Default::default()
            },
            provider_invocation: None,
            repository_integrity_evidence: None,
        },
        &mut provider,
        &mut |report| {
            checkpoints.push(report.clone());
            Ok(())
        },
    )
    .expect_err("gate transport failure propagates after checkpoint");

    assert!(!error.message.is_empty());
    assert_eq!(provider.verify_calls.len(), 1);
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(
        checkpoints[0].status,
        AgentTaskPromotionStatus::VerificationPending
    );
    assert!(checkpoints[0].status.patch_promoted());
    assert_eq!(
        checkpoints[0].target.path.as_deref(),
        worktree_path.to_str()
    );
    assert_eq!(checkpoints[0].provenance["post_apply"], true);
    assert!(checkpoints[0].provenance["candidate"].is_object());
    assert_eq!(
        checkpoints[0]
            .verified_base
            .as_ref()
            .map(|base| base.base.as_str()),
        Some("main")
    );
    assert_eq!(
        Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&worktree_path)
            .output()
            .expect("git status")
            .stdout,
        b" M src/lib.rs\n"
    );

    let resume_options = || AgentTaskPromotionRequest {
        source: source.clone(),
        source_run_id: Some("restartable-run".to_string()),
        source_path: Some(source_path.clone()),
        source_worktree_path: None,
        base_ref: Some("main".to_string()),
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: worktree_path.display().to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions {
            verify: vec!["true".to_string()],
            private_verify: Vec::new(),
            private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
            ..Default::default()
        },
        provider_invocation: None,
        repository_integrity_evidence: None,
    };
    let checkpoint = serde_json::to_value(&checkpoints[0]).expect("checkpoint value");
    let resumed = resume_promoted_patch(resume_options(), &worktree_path, &checkpoint)
        .expect("resume exact applied candidate");
    assert_eq!(resumed.status, AgentTaskPromotionStatus::Applied);

    std::fs::write(worktree_path.join("src/lib.rs"), "tampered\n").expect("tamper candidate");
    let mismatch = resume_promoted_patch(resume_options(), &worktree_path, &checkpoint)
        .expect_err("modified candidate rejected");
    assert!(mismatch
        .message
        .contains("differs from the exact checkpointed"));
    assert_eq!(
        mismatch
            .details
            .pointer("/recovery/action")
            .and_then(Value::as_str),
        Some("fork_replacement")
    );

    std::fs::write(worktree_path.join("extra.rs"), "extra\n").expect("add extra candidate file");
    let extra = resume_promoted_patch(resume_options(), &worktree_path, &checkpoint)
        .expect_err("extra candidate change rejected");
    assert!(extra
        .message
        .contains("differs from the exact checkpointed"));
    assert_eq!(
        extra
            .details
            .pointer("/recovery/action")
            .and_then(Value::as_str),
        Some("fork_replacement")
    );
}

#[test]
fn explicit_candidate_rejects_non_ancestor_base_and_dirty_source_before_apply() {
    let (temp, repo, base, candidate) = adopted_commit_repo();
    git(&repo, &["checkout", "-b", "unrelated", &base]);
    std::fs::write(repo.join("other.rs"), "other\n").expect("write unrelated");
    git(&repo, &["add", "other.rs"]);
    git(&repo, &["commit", "-m", "unrelated"]);
    let unrelated = git_head(&repo, "HEAD");
    git(&repo, &["checkout", "main"]);
    let mut provider = FakePromotionWorkspaceProvider::default();
    let error = promote_with_provider(
        adopted_commit_options(
            &temp,
            &repo,
            unrelated,
            candidate.clone(),
            VerifyGateOptions::default(),
        ),
        &mut provider,
    )
    .expect_err("non-ancestor base");
    assert!(
        error
            .message
            .contains("recorded task base is unrelated to the adopted candidate parent"),
        "{}",
        error.message
    );

    std::fs::write(repo.join("dirty.txt"), "dirty\n").expect("dirty source");
    let error = promote_with_provider(
        adopted_commit_options(&temp, &repo, base, candidate, VerifyGateOptions::default()),
        &mut provider,
    )
    .expect_err("dirty source");
    assert!(
        error.message.contains("promotion destination moved"),
        "{}",
        error.message
    );
}

#[test]
fn promotion_validates_declared_base_before_mutating_the_target_worktree() {
    // #9400: a nonexistent declared base must fail with no patch applied and no
    // durable post-apply state recorded, so the same artifact/target can be
    // retried with a corrected base.
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree_path = temp.path().join("managed-target");
        std::fs::create_dir(&worktree_path).expect("create target");
        git(&worktree_path, &["init"]);
        git(
            &worktree_path,
            &["config", "user.email", "test@example.com"],
        );
        git(&worktree_path, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(worktree_path.join("src")).expect("source dir");
        std::fs::write(worktree_path.join("src/lib.rs"), "old\n").expect("base source");
        git(&worktree_path, &["add", "."]);
        git(&worktree_path, &["commit", "-m", "base"]);
        git(&worktree_path, &["branch", "-M", "main"]);
        let remote = temp.path().join("origin.git");
        assert!(Command::new("git")
            .args(["init", "--bare", remote.to_str().expect("remote path")])
            .status()
            .expect("create remote")
            .success());
        git(
            &worktree_path,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        git(&worktree_path, &["push", "-u", "origin", "main"]);

        let candidate = harvest_patch_candidate(&worktree_path, VALID_PATCH);
        let (source_path, source) = write_patch_source(&temp);
        let source = with_candidate(&source, &candidate);
        let options = |base: &str| AgentTaskPromotionRequest {
            source: source.clone(),
            source_run_id: Some("cook-9400".to_string()),
            source_path: Some(source_path.clone()),
            source_worktree_path: None,
            base_ref: Some(base.to_string()),
            task_base_sha: None,
            candidate_ref: None,
            // Direct worktree paths are preflighted before apply.
            to_worktree: worktree_path.display().to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["true".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                ..Default::default()
            },
            provider_invocation: None,
            repository_integrity_evidence: None,
        };

        let mut provider = FakePromotionWorkspaceProvider::default();
        let mut checkpoints = Vec::new();
        let error =
            promote_with_provider_and_checkpoint(options("trunk"), &mut provider, &mut |report| {
                checkpoints.push(report.clone());
                Ok(())
            })
            .expect_err("nonexistent declared base must fail before apply");
        assert!(
            error
                .message
                .contains("declared base `trunk` was not found"),
            "unexpected error: {}",
            error.message
        );
        // Criterion 1 + 2: nothing materialized, no durable post-apply checkpoint.
        assert!(
            checkpoints.is_empty(),
            "no post-apply state may be recorded"
        );
        assert_eq!(
            Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&worktree_path)
                .output()
                .expect("git status")
                .stdout,
            b"",
            "the working tree must be clean"
        );

        // Criterion 3: retrying the same artifact/target with a valid base
        // succeeds.
        let mut provider = FakePromotionWorkspaceProvider::default();
        let report =
            promote_with_provider_and_checkpoint(options("main"), &mut provider, &mut |_| Ok(()))
                .expect("valid base promotes cleanly");
        assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(
            std::fs::read_to_string(worktree_path.join("src/lib.rs")).unwrap(),
            "new\n"
        );
        assert_eq!(
            report.verified_base.as_ref().map(|base| base.base.as_str()),
            Some("main")
        );
    });
}

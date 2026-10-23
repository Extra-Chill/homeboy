//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::promote::{
    normalize_promotion_patch, resume_promoted_patch, retain_committed_changes_artifact,
};
use super::super::types::{AgentTaskPromotionRequest, AgentTaskPromotionStatus};
use super::candidate_fixture::{
    promote_with_provider, promote_with_provider_and_checkpoint,
    promote_with_provider_and_checkpoint_in_observation_store,
    promote_with_provider_in_observation_store,
};
use super::*;
use crate::agent_task::AGENT_TASK_OUTCOME_SCHEMA;
use crate::agent_task_gate::{AgentTaskGateRevealPolicy, VerifyGateOptions};
use crate::agent_task_scheduler::AgentTaskAggregate;
use serde_json::Value;
use std::process::Command;

#[test]
fn promotion_uses_recovered_controller_projection_without_public_artifact_path() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let observation_store =
            homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let run_id = "recovered-lab-run";
        let task_id = "implement";
        let artifact_id = "patch";
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
        let temp = tempfile::tempdir().expect("promotion tempdir");
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(temp.path().join("target")),
            ..Default::default()
        };

        let result = promote_with_provider_in_observation_store(
            AgentTaskPromotionRequest {
                source: source.to_string(),
                source_run_id: Some(run_id.to_string()),
                source_path: None,
                source_worktree_path: None,
                base_ref: None,
                task_base_sha: None,
                candidate_ref: None,
                to_worktree: "homeboy@recovered-promotion".to_string(),
                task_id: Some(task_id.to_string()),
                artifact_id: Some(artifact_id.to_string()),
                dry_run: false,
                gates: VerifyGateOptions::default(),
                provider_invocation: None,
                repository_integrity_evidence: None,
            },
            &mut provider,
            &observation_store,
        )
        .expect("controller projection is promoted without a public artifact path");

        assert_eq!(result.status, AgentTaskPromotionStatus::Applied);
        assert!(provider.apply_calls.is_empty());
    });
}

#[test]
fn recoverable_promotion_projection_uses_the_explicit_observation_store() {
    let _home = gate_home();
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(left_context.path_roots())
            .open_observation_initialized()
            .expect("left observation store");
    let right_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(right_context.path_roots())
            .open_observation_initialized()
            .expect("right observation store");
    let run_id = "same-projected-run";
    let task_id = "same-projected-task";
    let artifact_id = "same-projected-patch";
    let mut source: Value = serde_json::from_str(&recovered_runner_aggregate(
        task_id,
        artifact_id,
        &sha256_hex(VALID_PATCH),
        VALID_PATCH.len(),
    ))
    .expect("aggregate JSON");
    source["status"] = Value::String("partial_recoverable".to_string());
    source["outcomes"][0]["status"] = Value::String("candidate_recoverable".to_string());
    source["outcomes"][0]["artifacts"][0]["metadata"] = serde_json::json!({
        "executor_artifact_finalized": true,
        "run_id": run_id,
        "task_id": task_id,
        "producer_attempt": 1,
        "base_ref": "base-sha",
        "provider_backend": "test",
        "repository_identity": "repo",
        "workspace_identity": "workspace",
        "gate_feedback_baseline": {
            "source_run_id": run_id,
            "source_task_id": task_id,
            "patch_artifact": {
                "id": artifact_id,
                "kind": "patch",
                "sha256": sha256_hex(VALID_PATCH),
            }
        }
    });
    source["outcomes"][0]["artifacts"][0]
        .as_object_mut()
        .expect("patch artifact")
        .remove("path");
    record_controller_projection_in_store(
        &right_store,
        run_id,
        task_id,
        artifact_id,
        "wrong patch bytes",
    );
    let projected = record_controller_projection_in_store(
        &left_store,
        run_id,
        task_id,
        artifact_id,
        VALID_PATCH,
    );
    let temp = tempfile::tempdir().expect("promotion tempdir");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("target")),
        ..Default::default()
    };

    let result = promote_with_provider_and_checkpoint_in_observation_store(
        AgentTaskPromotionRequest {
            source: source.to_string(),
            source_run_id: Some(run_id.to_string()),
            source_path: None,
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "homeboy@rooted-projection".to_string(),
            task_id: Some(task_id.to_string()),
            artifact_id: Some(artifact_id.to_string()),
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_invocation: None,
            repository_integrity_evidence: None,
        },
        &mut provider,
        &mut |_| Ok(()),
        &left_store,
    )
    .expect("explicit projection is promoted");

    assert!(projected.is_file());
    assert_eq!(result.patch_artifact.kind, "candidate_commit");
    assert!(provider.apply_calls.is_empty());
}

#[test]
fn committed_changes_retention_uses_the_explicit_observation_store() {
    let left_context = homeboy_core::test_support::HermeticTestContext::new();
    let right_context = homeboy_core::test_support::HermeticTestContext::new();
    let left_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(left_context.path_roots())
            .open_observation_initialized()
            .expect("left observation store");
    let right_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(right_context.path_roots())
            .open_observation_initialized()
            .expect("right observation store");
    let run_id = "same-committed-retention-run";
    left_store
        .upsert_imported_run(&homeboy_core::observation::RunRecord {
            id: run_id.to_string(),
            kind: "agent-task".to_string(),
            started_at: "2026-08-15T00:00:00Z".to_string(),
            finished_at: Some("2026-08-15T00:00:01Z".to_string()),
            status: "pass".to_string(),
            command: Some("homeboy agent-task".to_string()),
            homeboy_version: Some("test".to_string()),
            metadata_json: serde_json::json!({}),
            ..Default::default()
        })
        .expect("record explicit run");
    let aggregate: AgentTaskAggregate = serde_json::from_str(&recovered_runner_aggregate(
        "committed-task",
        "patch",
        &sha256_hex(VALID_PATCH),
        VALID_PATCH.len(),
    ))
    .expect("aggregate");
    let mut options = promotion_options("homeboy@committed-retention");
    options.source_run_id = Some(run_id.to_string());

    let retained = retain_committed_changes_artifact(
        &options,
        &aggregate.outcomes[0],
        VALID_PATCH,
        &sha256_hex(VALID_PATCH),
        &left_store,
    )
    .expect("retain committed changes")
    .expect("retained path");

    assert!(retained.starts_with(left_context.root()));
    assert_eq!(left_store.list_artifacts(run_id).unwrap().len(), 1);
    assert!(right_store.list_artifacts(run_id).unwrap().is_empty());
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

    assert_eq!(report.status, AgentTaskPromotionStatus::NoChanges);
    assert!(report.deterministic_gates.is_empty());
    assert!(report.gate_results.is_empty());
    assert!(provider.apply_calls.is_empty());
    assert!(provider.verify_calls.is_empty());
}

#[test]
fn promote_exports_committed_changes_when_executor_reports_no_patch_artifact() {
    let _home = gate_home();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "homeboy@example.test"]);
    git(&repo, &["config", "user.name", "Homeboy Test"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("lib.rs"), "old\n").expect("write base file");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "base"]);
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
    git(&repo, &["push", "-u", "origin", "main"]);
    let base = git_head(&repo, "HEAD");
    std::fs::write(repo.join("lib.rs"), "new\n").expect("write committed change");
    git(&repo, &["commit", "-am", "agent: committed change"]);

    let source_path = temp.path().join("outcome.json");
    let candidate_sha = git_head(&repo, "HEAD");
    let tree_sha = git_head(&repo, "HEAD^{tree}");
    git(&repo, &["reset", "--hard", &base]);
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-1",
        "status": "succeeded",
        "candidate": {"base_sha": base, "candidate_sha": candidate_sha, "tree_sha": tree_sha},
        "artifacts": []
    })
    .to_string();
    std::fs::write(&source_path, &source).expect("write source");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let options = |base_ref: &str| AgentTaskPromotionRequest {
        source: source.clone(),
        source_run_id: Some("run-no-artifact".to_string()),
        source_path: Some(source_path.clone()),
        source_worktree_path: Some(repo.clone()),
        base_ref: Some(base_ref.to_string()),
        task_base_sha: Some(base.clone()),
        candidate_ref: None,
        to_worktree: repo.display().to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions::default(),
        provider_invocation: None,
        repository_integrity_evidence: None,
    };

    let error = promote_with_provider(options("trunk"), &mut provider)
        .expect_err("invalid base rejects committed changes before apply");
    assert!(error
        .message
        .contains("declared base `trunk` was not found"));
    assert!(provider.apply_calls.is_empty());

    let report = promote_with_provider(options("main"), &mut provider)
        .expect("committed changes are promoted without a patch artifact");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.patch_artifact.id, candidate_sha);
    assert_eq!(report.changed_files, vec!["lib.rs"]);
    assert_eq!(report.provenance["candidate_commit"]["tree_sha"], tree_sha);
    assert!(provider.apply_calls.is_empty());
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
    let (source_path, source) = write_patch_source(&temp);
    let mut source: Value = serde_json::from_str(&source).expect("source JSON");
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
            to_worktree: "repo@promoted-task".to_string(),
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

    assert_ne!(
        report.patch_artifact.id,
        "task-1-attempt-1-committed-changes"
    );
    assert_eq!(report.patch_artifact.kind, "candidate_commit");
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
    let (source_path, source) = write_patch_source(&temp);
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
    assert!(provider.apply_calls.is_empty());
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
        b"M  src/lib.rs\n"
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
    let mut options = resume_options();
    let mut source: Value = serde_json::from_str(&options.source).unwrap();
    if source.get("outcomes").is_some() {
        source["outcomes"][0]["candidate"] = checkpoint["provenance"]["candidate_commit"].clone();
    } else {
        source["candidate"] = checkpoint["provenance"]["candidate_commit"].clone();
    }
    options.source = source.to_string();
    let sha = checkpoint["provenance"]["candidate_commit"]["candidate_sha"]
        .as_str()
        .unwrap();
    git(
        &worktree_path,
        &["update-ref", "refs/homeboy/candidates/restartable-run", sha],
    );
    let resumed = resume_promoted_patch(options.clone(), &worktree_path, &checkpoint)
        .expect("resume exact applied candidate");
    assert_eq!(resumed.status, AgentTaskPromotionStatus::Applied);
    assert!(provider.apply_calls.is_empty(), "resume must not reapply");

    std::fs::write(worktree_path.join("src/lib.rs"), "tampered\n").expect("tamper candidate");
    let restored = resume_promoted_patch(options, &worktree_path, &checkpoint)
        .expect("restore recorded candidate");
    assert_eq!(restored.patch_artifact.id, sha);
    assert_eq!(
        std::fs::read_to_string(worktree_path.join("src/lib.rs")).unwrap(),
        "new\n"
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
    assert!(provider.apply_calls.is_empty());

    std::fs::write(repo.join("dirty.txt"), "dirty\n").expect("dirty source");
    let error = promote_with_provider(
        adopted_commit_options(&temp, &repo, base, candidate, VerifyGateOptions::default()),
        &mut provider,
    )
    .expect_err("dirty source");
    assert!(error.message.contains("source worktree is dirty"));
    assert!(provider.apply_calls.is_empty());
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

        let (source_path, source) = write_patch_source(&temp);
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

        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(worktree_path.clone()),
            ..Default::default()
        };
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
        // Criterion 1 + 2: no patch applied, no durable post-apply checkpoint.
        assert!(
            provider.apply_calls.is_empty(),
            "the target worktree must not be mutated"
        );
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
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(worktree_path.clone()),
            ..Default::default()
        };
        let report =
            promote_with_provider_and_checkpoint(options("main"), &mut provider, &mut |_| Ok(()))
                .expect("valid base promotes cleanly");
        assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
        assert!(provider.apply_calls.is_empty());
        assert_eq!(
            report.verified_base.as_ref().map(|base| base.base.as_str()),
            Some("main")
        );
    });
}

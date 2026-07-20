//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::apply::{
    preflight_configured_workspace_provider_with_config, run_provider_command,
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
    AgentTaskPromotionWorkspaceProvider, ExternalPromotionWorkspaceProvider,
    AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA, AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
};
use super::super::promote::{
    normalize_promotion_patch, promote, promote_with_provider,
    promote_with_provider_and_checkpoint, resume_promoted_patch, select_patch_artifact,
    validate_artifact_content,
};
use super::super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandCapture,
    AgentTaskPromotionCommandReport, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};
use super::*;
use crate::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::agent_task_gate::{
    AgentTaskGateReport, AgentTaskGateRevealPolicy, AgentTaskGateVisibility, VerifyGateOptions,
};
use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::defaults::{
    HomeboyConfig, WorktreeProviderCommands, WorktreeProviderConfig, WorktreeProviderKind,
    WorktreeProviderListResultMapping,
};
use homeboy_core::lab_contract::AgentTaskDispatchIdentity;
use homeboy_core::worktree::{self, WorktreeAdoptOptions};
use homeboy_core::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn configured_promotion_preflight_rejects_missing_provider_before_dispatch() {
    let error = preflight_configured_workspace_provider_with_config(
        "fixture@missing",
        &HomeboyConfig::default(),
    )
    .expect_err("missing managed provider must fail preflight");

    assert_eq!(
        error.code,
        homeboy_core::ErrorCode::ValidationInvalidArgument
    );
    assert!(error
        .message
        .contains("no worktree providers are configured"));
}

#[cfg(unix)]
#[test]
fn adopted_workspace_wins_over_a_rejecting_configured_provider() {
    use std::os::unix::fs::PermissionsExt;

    homeboy_core::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("adopted workspace");
        let marker = tempfile::NamedTempFile::new().expect("provider marker");
        let marker_path = marker.into_temp_path();
        std::fs::remove_file(&marker_path).expect("remove provider marker");
        let provider = tempfile::NamedTempFile::new().expect("provider command");
        std::fs::write(
            provider.path(),
            format!("#!/bin/sh\ntouch '{}'\nexit 23\n", marker_path.display()),
        )
        .expect("write rejecting provider");
        let mut permissions = std::fs::metadata(provider.path())
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(provider.path(), permissions).expect("make provider executable");

        worktree::adopt(WorktreeAdoptOptions {
            handle: "fixture@adopted".to_string(),
            path: workspace.path().display().to_string(),
            kind: None,
            provenance: None,
        })
        .expect("adopt workspace");

        let mut config = HomeboyConfig::default();
        config.worktree_providers.insert(
            "rejecting".to_string(),
            WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![
                        provider.path().display().to_string(),
                        "{handle}".to_string(),
                    ]),
                    ..Default::default()
                },
                list_result_mapping: Some(WorktreeProviderListResultMapping {
                    items: "$.worktrees".to_string(),
                    handle: "$.handle".to_string(),
                    path: "$.path".to_string(),
                    branch: "$.branch".to_string(),
                    dirty: "$.safety.dirty".to_string(),
                    unpushed: "$.safety.unpushed".to_string(),
                    primary: "$.safety.primary".to_string(),
                }),
            },
        );

        preflight_configured_workspace_provider_with_config("fixture@adopted", &config)
            .expect("Homeboy adopted workspace bypasses provider preflight");
        assert!(!marker_path.exists(), "provider must not be invoked");
        let canonical_workspace = workspace
            .path()
            .canonicalize()
            .expect("canonical workspace");

        let mut promotion =
            ExternalPromotionWorkspaceProvider::from_options_with_config_and_environment(
                &promotion_options("fixture@adopted"),
                &config,
                Some(PathBuf::from("/fixture/homeboy")),
                None,
            );
        let error = promotion
            .apply_patch(AgentTaskPromotionApplyRequest {
                schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
                to_workspace: "fixture@adopted".to_string(),
                patch: Some(VALID_PATCH.to_string()),
                patch_path: "changes.patch".to_string(),
                changed_files: vec!["src/lib.rs".to_string()],
                gate_feedback_baseline: None,
                dry_run: false,
                trusted_unpushed_candidate_destination: None,
            })
            .expect_err("fixture executable is not an adapter");

        assert_eq!(
            promotion
                .invocation()
                .expect("adopted workspace invocation")
                .argv,
            vec![
                "/fixture/homeboy".to_string(),
                "agent-task".to_string(),
                "promotion-provider".to_string(),
                "--workspace".to_string(),
                canonical_workspace.display().to_string(),
            ]
        );
        assert_eq!(promotion.provenance().expect("provenance")["id"], "homeboy");
        assert_eq!(
            error.details["worktree_provider"]["path"],
            canonical_workspace.display().to_string()
        );
        assert!(!marker_path.exists(), "provider must not be invoked");

        std::fs::remove_dir_all(workspace.path()).expect("remove adopted workspace");
        let error = preflight_configured_workspace_provider_with_config("fixture@adopted", &config)
            .expect_err("stale Homeboy workspace remains fail-closed");
        assert!(error.message.contains("missing directory"));
        assert!(
            !marker_path.exists(),
            "stale Homeboy record must not fall back"
        );
    });
}

#[test]
fn promotion_rejects_missing_or_mismatched_recovered_controller_projection() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "recovered-lab-run";
        let task_id = "implement";
        let artifact_id = "patch";
        let source = recovered_runner_aggregate(
            task_id,
            artifact_id,
            &sha256_hex(VALID_PATCH),
            VALID_PATCH.len(),
        );
        let temp = tempfile::tempdir().expect("promotion tempdir");
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(temp.path().join("target")),
            ..Default::default()
        };
        let options = || AgentTaskPromotionOptions {
            source: source.clone(),
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
            provider_command: None,
            provider_invocation: None,
        };

        let missing = promote_with_provider(options(), &mut provider)
            .expect_err("missing projection rejected");
        assert!(missing
            .message
            .contains("verified controller-side artifact projection"));

        record_controller_projection(run_id, task_id, artifact_id, "different patch bytes");
        let mismatched = promote_with_provider(options(), &mut provider)
            .expect_err("mismatched projection rejected");
        assert!(mismatched
            .message
            .contains("does not match the aggregate SHA-256 and size"));
        assert!(provider.apply_calls.is_empty());
    });
}

#[test]
fn promote_recoverable_candidate_rejects_mismatched_run_provenance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_path, source) = recoverable_patch_source(&temp, 1);
    let mut source: Value = serde_json::from_str(&source).expect("source JSON");
    source["artifacts"][0]["metadata"]["run_id"] = Value::String("different-run".to_string());
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("target")),
        ..Default::default()
    };

    let error = promote_with_provider(
        AgentTaskPromotionOptions {
            source: source.to_string(),
            source_run_id: Some("recoverable-run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "repo@recoverable".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect_err("mismatched provenance rejected");

    assert!(error.message.contains("fingerprinted artifact"));
    assert!(provider.apply_calls.is_empty());
}

#[test]
fn explicit_promotion_provider_sources_precede_configured_resolution() {
    let mut options = promotion_options("fixture@missing");
    options.provider_command = Some("command-provider".to_string());
    options.provider_invocation = Some(CommandInvocation {
        argv: vec!["argv-provider".to_string()],
        ..Default::default()
    });

    let provider = ExternalPromotionWorkspaceProvider::from_options_with_config_and_environment(
        &options,
        &HomeboyConfig::default(),
        Some(PathBuf::from("/fixture/homeboy")),
        Some("environment-provider".to_string()),
    );

    assert_eq!(
        provider.invocation().expect("invocation").argv,
        vec!["argv-provider".to_string()]
    );

    options.provider_invocation = None;
    let provider = ExternalPromotionWorkspaceProvider::from_options_with_config_and_environment(
        &options,
        &HomeboyConfig::default(),
        Some(PathBuf::from("/fixture/homeboy")),
        Some("environment-provider".to_string()),
    );
    assert_eq!(
        provider.invocation().expect("invocation").argv,
        vec!["command-provider".to_string()]
    );

    options.provider_command = None;
    let provider = ExternalPromotionWorkspaceProvider::from_options_with_config_and_environment(
        &options,
        &HomeboyConfig::default(),
        Some(PathBuf::from("/fixture/homeboy")),
        Some("environment-provider".to_string()),
    );
    assert_eq!(
        provider.invocation().expect("invocation").argv,
        vec!["environment-provider".to_string()]
    );
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
fn empty_patch_failing_gate_is_reported_against_pinned_candidate() {
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
        AgentTaskPromotionOptions {
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
            provider_command: None,
            provider_invocation: None,
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
    assert_eq!(provider.apply_calls.len(), 0);
    assert_eq!(provider.verify_calls[0].0, repo);
}

#[test]
fn promote_exports_committed_changes_when_executor_reports_no_patch_artifact() {
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
    let base = git_head(&repo, "HEAD");
    std::fs::write(repo.join("lib.rs"), "new\n").expect("write committed change");
    git(&repo, &["commit", "-am", "agent: committed change"]);

    let source_path = temp.path().join("outcome.json");
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-1",
        "status": "succeeded",
        "artifacts": []
    })
    .to_string();
    std::fs::write(&source_path, &source).expect("write source");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run-no-artifact".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: Some("main".to_string()),
            task_base_sha: Some(base),
            candidate_ref: None,
            to_worktree: "repo@promoted".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("committed changes are promoted without a patch artifact");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.patch_artifact.id, "committed-changes");
    assert_eq!(report.changed_files, vec!["lib.rs"]);
    assert_eq!(report.provenance["change_source"], "local_commits");
    assert_eq!(
        report.provenance["commits"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(report.provenance["artifact_metadata"].is_null());
    assert_eq!(provider.apply_calls.len(), 1);
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
        AgentTaskPromotionOptions {
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
            provider_command: None,
            provider_invocation: None,
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
        apply_to_git: true,
        ..Default::default()
    };
    let mut checkpoints = Vec::new();

    let error = promote_with_provider_and_checkpoint(
        AgentTaskPromotionOptions {
            source: source.clone(),
            source_run_id: Some("restartable-run".to_string()),
            source_path: Some(source_path.clone()),
            source_worktree_path: None,
            base_ref: Some("main".to_string()),
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "homeboy@restartable".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["true".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
        &mut |report| {
            checkpoints.push(report.clone());
            Ok(())
        },
    )
    .expect_err("gate transport failure propagates after checkpoint");

    assert!(!error.message.is_empty());
    assert_eq!(provider.apply_calls.len(), 1);
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

    let resume_options = || AgentTaskPromotionOptions {
        source: source.clone(),
        source_run_id: Some("restartable-run".to_string()),
        source_path: Some(source_path.clone()),
        source_worktree_path: None,
        base_ref: Some("main".to_string()),
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: "homeboy@restartable".to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions {
            verify: vec!["true".to_string()],
            private_verify: Vec::new(),
            private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
            ..Default::default()
        },
        provider_command: None,
        provider_invocation: None,
    };
    let checkpoint = serde_json::to_value(&checkpoints[0]).expect("checkpoint value");
    let resumed = resume_promoted_patch(resume_options(), &worktree_path, &checkpoint)
        .expect("resume exact applied candidate");
    assert_eq!(resumed.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(provider.apply_calls.len(), 1, "resume must not reapply");

    std::fs::write(worktree_path.join("src/lib.rs"), "tampered\n").expect("tamper candidate");
    let mismatch = resume_promoted_patch(resume_options(), &worktree_path, &checkpoint)
        .expect_err("modified candidate rejected");
    assert!(mismatch
        .message
        .contains("differs from the exact checkpointed"));

    std::fs::write(worktree_path.join("extra.rs"), "extra\n").expect("add extra candidate file");
    let extra = resume_promoted_patch(resume_options(), &worktree_path, &checkpoint)
        .expect_err("extra candidate change rejected");
    assert!(extra
        .message
        .contains("differs from the exact checkpointed"));
}

#[test]
fn promote_rejects_unresolved_configured_provider_for_apply() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let (source_path, source) = write_patch_source(&temp);

        let err = promote(AgentTaskPromotionOptions {
            source,
            source_run_id: None,
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "repo@controlled-worktree".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: Vec::new(),
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        })
        .expect_err("unresolved configured provider rejected");

        assert!(err.message.contains("configured worktree provider"));
    });
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
    assert!(error.message.contains("ancestor"), "{}", error.message);
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
fn provider_command_response_supplies_workspace_and_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_path = temp.path().join("workspace");
    std::fs::create_dir(&workspace_path).expect("create workspace");
    assert!(std::process::Command::new("git")
        .arg("init")
        .arg(&workspace_path)
        .status()
        .expect("git init")
        .success());
    let response_path = temp.path().join("response.json");
    std::fs::write(
        &response_path,
        serde_json::json!({
            "schema": AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
            "workspace_path": workspace_path.display().to_string(),
            "command_evidence": [{
                "command": ["provider", "apply"],
                "exit_code": 0
            }]
        })
        .to_string(),
    )
    .expect("write response");
    let request_path = temp.path().join("request.json");

    let request = AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: "target-workspace".to_string(),
        patch: None,
        patch_path: temp.path().join("changes.patch").display().to_string(),
        changed_files: vec!["src/lib.rs".to_string()],
        gate_feedback_baseline: None,
        dry_run: false,
        trusted_unpushed_candidate_destination: None,
    };
    let workspace = run_provider_command(
        &CommandInvocation {
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "cat > {}; cat {}",
                    request_path.display(),
                    response_path.display()
                ),
            ],
            ..Default::default()
        },
        &request,
    )
    .expect("provider response");

    assert!(workspace.path.ends_with("workspace"));
    assert_eq!(
        workspace.command_evidence[0].command,
        vec!["provider", "apply"]
    );
    assert_eq!(
        serde_json::from_str::<AgentTaskPromotionApplyRequest>(
            &std::fs::read_to_string(request_path).expect("typed stdin request"),
        )
        .expect("decode typed request"),
        request
    );
}

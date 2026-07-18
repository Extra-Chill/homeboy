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
use homeboy_core::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn promotion_uses_verified_controller_projection_for_recovered_runner_aggregate_sources() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "recovered-lab-run";
        let task_id = "implement";
        let artifact_id = "patch";
        let projected_path =
            record_controller_projection(run_id, task_id, artifact_id, VALID_PATCH);
        let source = recovered_runner_aggregate(
            task_id,
            artifact_id,
            &sha256_hex(VALID_PATCH),
            VALID_PATCH.len(),
        );

        for aggregate_file_source in [false, true] {
            let aggregate_file = tempfile::NamedTempFile::new().expect("aggregate source");
            std::fs::write(aggregate_file.path(), &source).expect("write aggregate source");
            let source_path = aggregate_file_source.then(|| aggregate_file.path().to_path_buf());
            let temp = tempfile::tempdir().expect("promotion tempdir");
            let mut provider = FakePromotionWorkspaceProvider {
                workspace_path: Some(temp.path().join("target")),
                ..Default::default()
            };

            let report = promote_with_provider(
                AgentTaskPromotionOptions {
                    source: source.clone(),
                    source_run_id: Some(run_id.to_string()),
                    source_path,
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
                },
                &mut provider,
            )
            .expect("recovered aggregate promotes from controller projection");

            assert_eq!(
                report.patch_artifact.path,
                projected_path.display().to_string()
            );
            assert_eq!(
                provider.applied_patch_contents,
                vec![VALID_PATCH.to_string()]
            );
        }
    });
}

#[test]
fn promote_recoverable_candidate_applies_exactly_one_actionable_patch() {
    let (result, apply_calls) = promote_recoverable_patch_count(1);
    assert_eq!(
        result.expect("single candidate applies").status,
        AgentTaskPromotionStatus::Applied
    );
    assert_eq!(apply_calls, 1);
}

#[test]
fn lookup_only_configured_provider_cannot_construct_a_promotion_adapter() {
    let workspace = tempfile::tempdir().expect("workspace");
    git(workspace.path(), &["init", "-b", "cook-target"]);
    let provider = tempfile::NamedTempFile::new().expect("provider command");
    std::fs::write(
        provider.path(),
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\n",
            serde_json::json!({
                "worktrees": [{
                    "handle": "fixture@cook-target",
                    "path": workspace.path(),
                    "branch": "cook-target",
                    "safety": { "dirty": false, "unpushed": false, "primary": false }
                }]
            })
        ),
    )
    .expect("write provider command");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(provider.path())
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(provider.path(), permissions).expect("make provider executable");
    }
    let provider_path = provider.into_temp_path();
    let mut config = HomeboyConfig::default();
    config.worktree_providers.insert(
        "fixture".to_string(),
        WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            commands: WorktreeProviderCommands {
                resolve: Some(vec![
                    provider_path.display().to_string(),
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

    homeboy_core::worktree_providers::resolve_worktree_provider_from_config(
        "fixture@cook-target",
        &config,
    )
    .expect("lookup-only provider resolves for non-mutating callers");

    let error = ExternalPromotionWorkspaceProvider::from_options_with_config_and_environment(
        &promotion_options("fixture@cook-target"),
        &config,
        Some(PathBuf::from("/fixture/homeboy")),
        None,
    );
    let mut error = error;
    let error = error
        .apply_patch(AgentTaskPromotionApplyRequest {
            schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
            to_workspace: "fixture@cook-target".to_string(),
            patch: Some(VALID_PATCH.to_string()),
            patch_path: "changes.patch".to_string(),
            changed_files: vec!["src/lib.rs".to_string()],
            gate_feedback_baseline: None,
            dry_run: false,
            trusted_unpushed_candidate_destination: None,
        })
        .expect_err("lookup-only provider must not authorize promotion");

    assert!(error
        .message
        .contains("not apply-enabled provider(s): fixture"));
}

#[test]
fn normalize_promotion_patch_strips_lab_sandbox_workspace_prefix() {
    let patch = "diff --git a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n--- a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n+++ b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";

    let normalized = normalize_promotion_patch(patch, "homeboy@promoted-task")
        .expect("sandbox-prefixed patch normalizes");

    assert_eq!(normalized.changed_files, vec!["src/lib.rs"]);
    assert_eq!(
        normalized.content,
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
    );
}

#[test]
fn empty_patch_runs_public_and_private_gates_against_pinned_candidate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "homeboy@example.test"]);
    git(&repo, &["config", "user.name", "Homeboy Test"]);
    std::fs::write(repo.join("lib.rs"), "candidate\n").expect("write candidate");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "candidate"]);
    let revision = git_head(&repo, "HEAD");
    let (source_path, source) = write_empty_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run-empty".to_string()),
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
                verify: vec!["public-check".to_string()],
                private_verify: vec!["private-check".to_string()],
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("empty candidate verifies");

    assert_eq!(report.status, AgentTaskPromotionStatus::VerifiedNoChanges);
    assert_eq!(report.target.head.as_deref(), Some(revision.as_str()));
    assert_eq!(report.provenance["verified_revision"], revision);
    assert_eq!(provider.apply_calls.len(), 0);
    assert_eq!(provider.verify_calls.len(), 2);
    assert_eq!(provider.verify_calls[0].0, repo);
    assert_eq!(provider.verify_calls[1].2, AgentTaskGateVisibility::Private);
    assert_eq!(
        provider.verify_calls[1].3,
        AgentTaskGateRevealPolicy::SummaryOnly
    );
}

#[test]
fn promote_exports_committed_changes_when_patch_artifact_is_empty() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "homeboy@example.test"]);
    git(&repo, &["config", "user.name", "Homeboy Test"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::create_dir(repo.join("src")).expect("create src");
    std::fs::write(repo.join("src/lib.rs"), "old\n").expect("write base file");
    git(&repo, &["add", "src/lib.rs"]);
    git(&repo, &["commit", "-m", "base"]);
    git(&repo, &["checkout", "-b", "fix/committed"]);
    std::fs::write(repo.join("src/lib.rs"), "new\n").expect("write committed change");
    git(&repo, &["commit", "-am", "fix: committed change"]);

    let (source_path, source) = write_empty_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run-committed".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: Some("main".to_string()),
            task_base_sha: Some(git_head(&repo, "main")),
            candidate_ref: None,
            to_worktree: "repo@fix-committed".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["true".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("committed changes are promoted");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(report.patch_artifact.id, "committed-changes");
    assert!(Path::new(&report.patch_artifact.path).is_file());
    assert!(std::fs::read_to_string(&report.patch_artifact.path)
        .expect("read committed patch")
        .contains("+new"));
    assert_eq!(
        report.provenance["change_source"].as_str(),
        Some("local_commits")
    );
    assert_eq!(
        report.provenance["commits"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(provider.verify_calls.len(), 1);
    assert_eq!(provider.verify_calls[0].0, repo);
}

#[test]
fn validate_patch_rejects_path_traversal() {
    let patch = "--- a/src/lib.rs\n+++ b/../secret\n@@ -1 +1 @@\n-old\n+new\n";

    let err =
        normalize_promotion_patch(patch, "repo@promoted-task").expect_err("unsafe path rejected");

    assert!(err.message.contains("unsafe patch path"));
}

#[test]
fn promote_dry_run_validates_provider_request_without_applying() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_path, source) = write_patch_source(&temp);

    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("controlled-worktree")),
        ..Default::default()
    };
    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
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
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("dry-run promotion report");

    assert_eq!(report.status, AgentTaskPromotionStatus::DryRun);
    assert_eq!(report.source.task_id, "task-1");
    assert_eq!(report.patch_artifact.id, "patch");
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(provider.apply_calls[0].patch.as_deref(), Some(VALID_PATCH));
    assert!(provider.apply_calls[0].dry_run);
    assert_eq!(report.command_evidence.len(), 1);
    assert!(report.deterministic_gates.is_empty());
}

#[test]
fn promote_verification_failure_keeps_the_applied_target_recoverable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree_path = temp.path().join("fresh-managed-target");
    std::fs::create_dir(&worktree_path).expect("create target");
    git(&worktree_path, &["init"]);
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(worktree_path.clone()),
        verify_exit_code: 1,
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("runner-only-run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "homeboy@fix-7964".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["false".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("verification failure is a recoverable promotion report");

    assert_eq!(report.status, AgentTaskPromotionStatus::GateFailed);
    assert!(report.status.patch_promoted());
    assert_eq!(report.target.path.as_deref(), worktree_path.to_str());
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(provider.verify_calls.len(), 1);
    assert_eq!(report.operator_notification.status, "blocked");
}

#[test]
fn promote_applies_normalized_lab_sandbox_patch_with_fake_workspace_provider() {
    let temp = tempfile::tempdir().expect("tempdir");
    let patch = "diff --git a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n--- a/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n+++ b/workspace/homeboy-refactor-command-contract-boundaries-abc/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let patch_path = temp.path().join("changes.patch");
    std::fs::write(&patch_path, patch).expect("write patch");
    let source_path = temp.path().join("outcome.json");
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-1",
        "status": "succeeded",
        "artifacts": [{
            "schema": AGENT_TASK_ARTIFACT_SCHEMA,
            "id": "patch",
            "kind": "patch",
            "path": "changes.patch",
            "size_bytes": patch.len(),
            "sha256": sha256_hex(patch)
        }]
    })
    .to_string();
    std::fs::write(&source_path, &source).expect("write source");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("worktree")),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: None,
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "homeboy@promoted-task".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: Vec::new(),
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("promote applies normalized patch");

    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(provider.apply_calls[0].changed_files, vec!["src/lib.rs"]);
    assert_ne!(
        provider.apply_calls[0].patch_path,
        patch_path.display().to_string()
    );
    let provider_patch = &provider.applied_patch_contents[0];
    assert!(provider_patch.contains("diff --git a/src/lib.rs b/src/lib.rs"));
    assert!(!provider_patch.contains("workspace/homeboy-refactor-command-contract-boundaries"));
}

#[test]
fn explicit_candidate_rejections_leave_target_unmodified() {
    let (temp, repo, base, candidate) = adopted_commit_repo();
    let cases = [
        (
            "unresolved",
            "not-a-commit".to_string(),
            base.clone(),
            "not present",
        ),
        (
            "stale",
            candidate.clone(),
            "0000000000000000000000000000000000000000".to_string(),
            "git rev-parse",
        ),
    ];
    for (name, candidate_ref, task_base, expected) in cases {
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.clone()),
            ..Default::default()
        };
        let error = promote_with_provider(
            adopted_commit_options(
                &temp,
                &repo,
                task_base,
                candidate_ref,
                VerifyGateOptions::default(),
            ),
            &mut provider,
        )
        .expect_err(name);
        assert!(
            error.message.contains(expected),
            "{name}: {}",
            error.message
        );
        assert!(provider.apply_calls.is_empty(), "{name} mutated target");
    }
}

#[test]
fn promotion_report_serializes_generic_command_evidence() {
    let report = AgentTaskPromotionReport {
        schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
        status: AgentTaskPromotionStatus::Applied,
        source: AgentTaskPromotionSource {
            kind: "outcome".to_string(),
            task_id: "task-1".to_string(),
            run_id: Some("run-1".to_string()),
            path: None,
        },
        to_worktree: "repo@controlled-worktree".to_string(),
        target: AgentTaskPromotionTarget {
            worktree: "repo@controlled-worktree".to_string(),
            path: Some("/tmp/repo@controlled-worktree".to_string()),
            branch: Some("fix/test".to_string()),
            head: Some("abc123".to_string()),
            dirty: Some(true),
        },
        patch_artifact: AgentTaskPromotionArtifactRef {
            id: "patch".to_string(),
            kind: "patch".to_string(),
            path: "changes.patch".to_string(),
            sha256: None,
        },
        changed_files: vec!["src/lib.rs".to_string()],
        command_evidence: vec![command_report(vec![
            "fake-workspace-provider",
            "apply-patch",
        ])],
        deterministic_gates: Vec::new(),
        gate_results: Vec::new(),
        verified_base: None,
        provenance: Value::Null,
        operator_notification: AgentTaskPromotionNotification {
            status: "completed".to_string(),
            message: "patch promoted".to_string(),
            resumable_blocker: None,
            next_command: None,
        },
    };

    let value = serde_json::to_value(report).expect("serialize report");

    assert_eq!(
        value["command_evidence"][0]["command"][0].as_str(),
        Some("fake-workspace-provider")
    );
}

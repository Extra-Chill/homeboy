//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::apply::{
    preflight_configured_workspace_provider_with_config, run_provider_command,
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
    AgentTaskPromotionWorkspaceProvider, ExternalPromotionWorkspaceProvider,
    AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA, AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
};
use super::super::promote::{
    normalize_promotion_patch, promote, promote_with_provider_and_checkpoint_in_observation_store,
    resume_promoted_patch, retain_committed_changes_artifact, select_patch_artifact,
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

#[test]
fn recoverable_promotion_projection_uses_the_explicit_observation_store() {
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
        AgentTaskPromotionOptions {
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
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
        &mut |_| Ok(()),
        &left_store,
    )
    .expect("explicit projection is promoted");

    assert_eq!(result.patch_artifact.path, projected.display().to_string());
    assert_eq!(provider.apply_calls.len(), 1);
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
            component_id: None,
            started_at: "2026-08-15T00:00:00Z".to_string(),
            finished_at: Some("2026-08-15T00:00:01Z".to_string()),
            status: "pass".to_string(),
            command: Some("homeboy agent-task".to_string()),
            cwd: None,
            homeboy_version: Some("test".to_string()),
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({}),
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
fn normalize_promotion_patch_rejects_repo_sandbox_without_relative_suffix() {
    let patch = "diff --git a/workspace/homeboy-refactor b/workspace/homeboy-refactor\n--- a/workspace/homeboy-refactor\n+++ b/workspace/homeboy-refactor\n@@ -1 +1 @@\n-old\n+new\n";

    let err = normalize_promotion_patch(patch, "homeboy@promoted-task")
        .expect_err("repo sandbox path without suffix rejected");

    assert!(err.message.contains("no repo-relative suffix"));
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

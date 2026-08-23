//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::apply::{
    preflight_configured_workspace_provider_with_config, run_provider_command,
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
    AgentTaskPromotionWorkspaceProvider, ExternalPromotionWorkspaceProvider,
    AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA, AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
};
use super::super::promote::{
    normalize_promotion_patch, promote, resume_promoted_patch, select_patch_artifact,
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
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
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
                task_url: None,
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
fn validate_patch_rejects_path_traversal() {
    let patch = "--- a/src/lib.rs\n+++ b/../secret\n@@ -1 +1 @@\n-old\n+new\n";

    let err =
        normalize_promotion_patch(patch, "repo@promoted-task").expect_err("unsafe path rejected");

    assert!(err.message.contains("unsafe patch path"));
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

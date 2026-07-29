//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::apply::{
    preflight_configured_workspace_provider_with_config, run_provider_command,
    run_provider_command_with_timeout, AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
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
    AgentTaskGateExecutionPolicy, AgentTaskGateReport, AgentTaskGateRevealPolicy,
    AgentTaskGateStatus, AgentTaskGateVisibility, VerifyGateOptions,
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
fn bridge_reconciliation_recovers_mixed_runner_artifacts_for_local_promotion_idempotently() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let run_id = "recovered-typed-lab-run";
        let task_id = "implement";
        let artifact_id = "patch";
        let plan = AgentTaskPlan::new("recovered-typed-lab-plan", Vec::new());
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit plan");
        let finalized = homeboy_core::paths::artifact_root()
            .expect("artifact root")
            .join("executor-finalized")
            .join("recovered-run")
            .join("patch");
        std::fs::create_dir_all(finalized.parent().expect("finalized parent"))
            .expect("create finalized parent");
        std::fs::write(&finalized, VALID_PATCH).expect("write controller finalized patch");
        let aggregate: AgentTaskAggregate = serde_json::from_str(
            &serde_json::json!({
                "schema": "homeboy/agent-task-aggregate/v1",
                "plan_id": "recovered-typed-lab-plan",
                "status": "succeeded",
                "totals": { "skipped": 0, "succeeded": 1 },
                "outcomes": [{
                    "schema": AGENT_TASK_OUTCOME_SCHEMA,
                    "task_id": task_id,
                    "status": "succeeded",
                    "artifacts": [{
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": artifact_id,
                        "kind": "patch",
                        "path": "/home/runner/.homeboy/executor-finalized/patch.diff",
                        "url": "homeboy://agent-task/run/recovered-typed-lab-run/artifacts#task=implement&artifact=patch",
                        "size_bytes": VALID_PATCH.len(),
                        "sha256": sha256_hex(VALID_PATCH),
                        "metadata": {
                            "executor_artifact_finalized": true,
                            "source_provenance": { "runner_id": "homeboy-lab" }
                        }
                    }, {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "transcript",
                        "kind": "transcript",
                        "path": "/home/runner/.homeboy/executor-finalized/transcript.json",
                        "size_bytes": 10,
                        "sha256": "a".repeat(64),
                        "metadata": { "executor_artifact_finalized": true }
                    }, {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "result",
                        "kind": "result",
                        "path": "/home/runner/.homeboy/executor-finalized/result.json",
                        "size_bytes": 10,
                        "sha256": "b".repeat(64),
                        "metadata": { "executor_artifact_finalized": true }
                    }, {
                        "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                        "id": "runtime-log",
                        "kind": "runtime-log",
                        "path": "/home/runner/.homeboy/executor-finalized/runtime.log",
                        "size_bytes": 10,
                        "sha256": "c".repeat(64),
                        "metadata": { "executor_artifact_finalized": true }
                    }],
                    "typed_artifacts": [{
                        "name": "patch",
                        "artifact_type": "file",
                        "payload": {
                            "artifact_id": artifact_id,
                            "kind": "patch",
                            "path": "/home/runner/.homeboy/executor-finalized/patch.diff",
                            "sha256": sha256_hex(VALID_PATCH),
                            "size_bytes": VALID_PATCH.len(),
                            "url": "homeboy://agent-task/run/recovered-typed-lab-run/artifacts#task=implement&artifact=patch"
                        },
                        "artifact": {
                            "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                            "id": artifact_id,
                            "kind": "patch",
                            "path": "/home/runner/.homeboy/executor-finalized/patch.diff",
                            "size_bytes": VALID_PATCH.len(),
                            "sha256": sha256_hex(VALID_PATCH),
                            "metadata": { "executor_artifact_finalized": true }
                        }
                    }]
                }]
            })
            .to_string(),
        )
        .expect("recovered aggregate");
        let mut hash = Sha256::new();
        hash.update(run_id.as_bytes());
        hash.update([0]);
        hash.update(task_id.as_bytes());
        hash.update([0]);
        hash.update(artifact_id.as_bytes());
        let imported_id = format!("agent-task-{:x}", hash.finalize());
        let imported = tempfile::NamedTempFile::new().expect("imported patch");
        std::fs::write(imported.path(), VALID_PATCH).expect("write imported patch");
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        crate::agent_task_lifecycle::record_detached_lab_run(
            crate::agent_task_lifecycle::DetachedLabRunRecord {
                run_id,
                runner_id: "homeboy-lab",
                runner_job_id: "job-1",
                remote_workspace: "/runner/homeboy",
                remote_command: &command,
            },
        )
        .expect("record detached Lab run");
        homeboy_core::observation::ObservationStore::open_initialized()
            .expect("store")
            .import_artifact(&homeboy_core::observation::ArtifactRecord {
                id: imported_id,
                run_id: run_id.to_string(),
                kind: "patch".to_string(),
                artifact_type: "file".to_string(),
                path: imported.path().display().to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: Some(sha256_hex(VALID_PATCH)),
                size_bytes: Some(VALID_PATCH.len() as i64),
                mime: Some("text/x-patch".to_string()),
                metadata_json: serde_json::json!({ "name": artifact_id }),
                created_at: "2026-07-19T00:00:00Z".to_string(),
            })
            .expect("import matching bundle artifact");
        crate::agent_task_promotion::mirror_agent_task_run_plan_aggregate(
            "@runner-plan.json",
            run_id,
            aggregate.clone(),
            None,
            None,
        )
        .expect("bridge reconciliation");
        crate::agent_task_promotion::mirror_agent_task_run_plan_aggregate(
            "@runner-plan.json",
            run_id,
            aggregate.clone(),
            None,
            None,
        )
        .expect("idempotent bridge reconciliation");

        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let artifacts = store.list_artifacts(run_id).expect("projected artifacts");
        assert_eq!(artifacts.len(), 4);
        let patch = artifacts
            .iter()
            .find(|artifact| artifact.artifact_type == "file")
            .expect("projected patch");
        assert_eq!(patch.metadata_json["agent_task"]["task_id"], task_id);
        assert_eq!(
            patch.metadata_json["agent_task"]["logical_artifact_id"],
            artifact_id
        );
        let record = crate::agent_task_lifecycle::status(run_id).expect("recovered status");
        assert_eq!(record.metadata["runner_id"], "homeboy-lab");
        assert_eq!(record.metadata["artifact_projection"]["status"], "pending");
        assert_eq!(
            record.metadata["artifact_projection"]["recovery_action"]["kind"],
            "fetch_and_reconcile"
        );

        let remote_id = "runner-patch-reference";
        store
            .import_artifact(&homeboy_core::observation::ArtifactRecord {
                id: remote_id.to_string(),
                run_id: run_id.to_string(),
                kind: "patch".to_string(),
                artifact_type: "remote_file".to_string(),
                path: "runner-artifact://homeboy-lab/recovered-typed-lab-run/patch".to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: Some(sha256_hex(VALID_PATCH)),
                size_bytes: Some(VALID_PATCH.len() as i64),
                mime: Some("text/x-patch".to_string()),
                metadata_json: serde_json::json!({
                    "agent_task": {
                        "task_id": task_id,
                        "logical_artifact_id": artifact_id,
                    }
                }),
                created_at: "2026-07-16T00:00:01Z".to_string(),
            })
            .expect("remote patch reference");
        let selected_id = crate::agent_task_lifecycle::resolve_promotion_patch_artifact_id(
            run_id,
            Some(task_id),
            remote_id,
        )
        .expect("persisted remote record resolves to the logical patch id");
        assert_eq!(selected_id, artifact_id);

        let temp = tempfile::tempdir().expect("promotion tempdir");
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(temp.path().join("target")),
            ..Default::default()
        };
        let report = promote_with_provider(
            AgentTaskPromotionOptions {
                source: serde_json::to_string(&aggregate).expect("aggregate json"),
                source_run_id: Some(run_id.to_string()),
                source_path: None,
                source_worktree_path: None,
                base_ref: None,
                task_base_sha: None,
                candidate_ref: None,
                to_worktree: "homeboy@recovered-promotion".to_string(),
                task_id: Some(task_id.to_string()),
                artifact_id: Some(selected_id),
                dry_run: false,
                gates: VerifyGateOptions::default(),
                provider_command: None,
                provider_invocation: None,
            },
            &mut provider,
        )
        .expect("promote recovered controller projection");
        assert_eq!(report.patch_artifact.path, patch.path);
        assert_eq!(
            provider.applied_patch_contents,
            vec![VALID_PATCH.to_string()]
        );
    });
}

#[test]
fn aggregate_promotion_forwards_canonical_gate_feedback_baseline() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let patch_path = temp.path().join("remediation.patch");
        std::fs::write(&patch_path, VALID_PATCH).expect("write remediation patch");
        let baseline_patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        let baseline_sha256 = sha256_hex(baseline_patch);
        record_controller_projection(
            "source-run",
            "source-task",
            "baseline-patch",
            baseline_patch,
        );
        let baseline = serde_json::json!({
        "source_run_id": "source-run",
        "source_task_id": "source-task",
        "source_patch_task_id": "source-task",
        "to_worktree": "fixture@target",
        "current_diff": "diff --git a/a b/a",
        "failed_gates": [],
        "patch_artifact": {
            "id": "baseline-patch",
            "kind": "patch",
            "path": "/home/lab/ephemeral/candidate.patch",
            "sha256": baseline_sha256
        }
        });
        let source = serde_json::json!({
        "schema": "homeboy/agent-task-aggregate/v1",
        "plan_id": "follow-up-plan",
        "status": "succeeded",
        "totals": {
            "queued": 0,
            "running": 0,
            "blocked": 0,
            "skipped": 0,
            "succeeded": 1,
            "candidate_recoverable": 0,
            "recoverable_candidates": 0,
            "failed": 0,
            "cancelled": 0,
            "timed_out": 0
        },
        "outcomes": [{
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "follow-up",
            "status": "succeeded",
            "artifacts": [{
                "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                "id": "patch",
                "kind": "patch",
                "path": patch_path,
                "size_bytes": VALID_PATCH.len(),
                "sha256": sha256_hex(VALID_PATCH),
                "metadata": { "gate_feedback_baseline": baseline }
            }],
            "typed_artifacts": [{
                "name": "patch",
                "payload": { "artifact_id": "patch" },
                "metadata": { "normalized_from": "artifact" }
            }]
        }]
        })
        .to_string();
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        promote_with_provider(
            AgentTaskPromotionOptions {
                source,
                source_run_id: Some("follow-up-run".to_string()),
                source_path: None,
                source_worktree_path: None,
                base_ref: None,
                task_base_sha: None,
                candidate_ref: None,
                to_worktree: "fixture@target".to_string(),
                task_id: Some("follow-up".to_string()),
                artifact_id: Some("patch".to_string()),
                dry_run: false,
                gates: VerifyGateOptions::default(),
                provider_command: None,
                provider_invocation: None,
            },
            &mut provider,
        )
        .expect("aggregate promotion");
        let forwarded = provider.apply_calls[0]
            .gate_feedback_baseline
            .as_ref()
            .expect("baseline forwarded");
        assert!(forwarded["patch_artifact"].get("path").is_none());
        assert_eq!(
            forwarded["patch_artifact"]["controller_artifact"]["run_id"],
            "source-run"
        );
        assert_eq!(
            forwarded["patch_artifact"]["controller_artifact"]["sha256"],
            baseline_sha256
        );
    });
}

#[test]
fn follow_up_promotion_records_and_forwards_verified_chain_baseline() {
    let temp = tempfile::tempdir().expect("tempdir");
    let patch_path = temp.path().join("follow-up.patch");
    std::fs::write(&patch_path, VALID_PATCH).expect("write follow-up patch");
    let prior_sha256 = "b".repeat(64);
    let source_tree = "c".repeat(40);
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "follow-up",
        "status": "succeeded",
        "artifacts": [{
            "schema": AGENT_TASK_ARTIFACT_SCHEMA,
            "id": "patch",
            "kind": "patch",
            "path": patch_path,
            "size_bytes": VALID_PATCH.len(),
            "sha256": sha256_hex(VALID_PATCH),
            "metadata": {
                "source_provenance": {
                    "verified_cook_baseline": {
                        "source_run_id": "v1-run",
                        "source_task_id": "v1-task",
                        "promoted_patch_artifact_sha256": prior_sha256,
                        "baseline_tree": source_tree
                    }
                }
            }
        }]
    })
    .to_string();
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().to_path_buf()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("v2-run".to_string()),
            source_path: None,
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@target".to_string(),
            task_id: Some("follow-up".to_string()),
            artifact_id: Some("patch".to_string()),
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("promote follow-up");

    assert_eq!(
        provider.apply_calls[0].gate_feedback_baseline,
        Some(serde_json::json!({
            "schema": "homeboy/agent-task-promotion-chain-baseline/v1",
            "source_tree": source_tree,
            "prior_patch_artifact": {
                "sha256": prior_sha256,
                "source_run_id": "v1-run",
                "source_task_id": "v1-task"
            }
        }))
    );
    assert_eq!(
        report.provenance["prior_baseline"]["source_tree"],
        source_tree
    );
    assert_eq!(
        report.provenance["prior_baseline"]["prior_patch_artifact"]["sha256"],
        prior_sha256
    );
    assert_eq!(report.patch_artifact.id, "patch");
    assert!(report.provenance["destination_baseline"].is_object());
}

#[test]
fn promote_recoverable_candidate_collapses_duplicate_digest_aliases() {
    let (result, apply_calls) = promote_recoverable_patch_count(3);
    let report = result.expect("equivalent candidates are canonicalized");
    assert_eq!(report.patch_artifact.id, "candidate-0");
    assert_eq!(report.patch_artifact.kind, "patch");
    assert_eq!(apply_calls, 1);
}

#[test]
fn promote_recoverable_candidate_reports_distinct_patch_review_choices() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_path, source) = recoverable_patch_source(&temp, 2);
    let distinct = VALID_PATCH.replace("+new", "+different");
    std::fs::write(temp.path().join("candidate-1.patch"), &distinct).expect("write distinct patch");
    let mut source: Value = serde_json::from_str(&source).expect("source JSON");
    source["artifacts"][1]["sha256"] = Value::String(sha256_hex(&distinct));
    source["artifacts"][1]["size_bytes"] = Value::from(distinct.len());
    let source = source.to_string();
    std::fs::write(&source_path, &source).expect("rewrite source");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("target")),
        ..Default::default()
    };

    let error = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
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
    .expect_err("distinct candidates require a choice");

    assert_eq!(error.details["review_choices"].as_array().unwrap().len(), 2);
    assert_eq!(provider.apply_calls.len(), 0);
}

#[test]
fn promote_recoverable_candidate_keeps_same_patch_from_distinct_attempts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_path, source) = recoverable_patch_source(&temp, 2);
    let mut source: Value = serde_json::from_str(&source).expect("source JSON");
    source["artifacts"][1]["metadata"]["producer_attempt"] = Value::from(2);
    let source = source.to_string();
    std::fs::write(&source_path, &source).expect("rewrite source");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("target")),
        ..Default::default()
    };

    let error = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
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
    .expect_err("different attempts remain review choices");

    assert_eq!(error.details["review_choices"].as_array().unwrap().len(), 2);
    assert_eq!(provider.apply_calls.len(), 0);
}

#[test]
fn materialized_workspace_promotion_adapter_applies_inline_patch_when_artifact_is_remote() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("create workspace");
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Test User"]);
    std::fs::write(workspace.join("src.txt"), "old\n").expect("write source");
    git(&workspace, &["add", "src.txt"]);
    git(&workspace, &["commit", "-m", "initial"]);
    let patch =
        "diff --git a/src.txt b/src.txt\n--- a/src.txt\n+++ b/src.txt\n@@ -1 +1 @@\n-old\n+new\n";

    let response = super::super::apply_materialized_workspace_patch(
        &workspace,
        &serde_json::json!({
            "schema": AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA,
            "to_workspace": "homeboy@fix-7913",
            "patch": patch,
            "patch_path": "runner-artifact://homeboy-lab/run-1/changes.patch",
            "changed_files": ["src.txt"]
        })
        .to_string(),
    )
    .expect("adapter applies patch");
    let response: Value = serde_json::from_str(&response).expect("adapter response JSON");

    assert_eq!(
        response["schema"],
        AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA
    );
    assert_eq!(response["workspace_path"], workspace.display().to_string());
    assert_eq!(
        std::fs::read_to_string(workspace.join("src.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn validate_patch_rejects_empty_patch() {
    let err =
        normalize_promotion_patch("\n\t", "repo@promoted-task").expect_err("empty patch rejected");

    assert!(err.message.contains("empty patch"));
}

#[test]
fn promote_no_op_outcome_uses_audited_committed_candidate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "agent@example.test"]);
    git(&repo, &["config", "user.name", "Agent"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("lib.rs"), "base\n").expect("write base");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "base"]);
    let base = git_head(&repo, "HEAD");
    std::fs::write(repo.join("lib.rs"), "candidate\n").expect("write candidate");
    git(&repo, &["commit", "-am", "agent candidate"]);

    let source_path = temp.path().join("outcome.json");
    let mut outcome = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task",
        "status": "succeeded",
        "artifacts": []
    });
    outcome["status"] = Value::String("no_op".to_string());
    let source = outcome.to_string();
    std::fs::write(&source_path, &source).expect("write mutated outcome");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: None,
            task_base_sha: Some(base.clone()),
            candidate_ref: None,
            to_worktree: "repo@promotion".to_string(),
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
    )
    .expect("audited committed candidate promotes");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.patch_artifact.id, "committed-changes");
    assert_eq!(report.provenance["change_source"], "local_commits");
    assert_eq!(report.provenance["base_ref"], base);
    assert_eq!(report.provenance["candidate"]["kind"], "git");
    assert_eq!(
        report.provenance["candidate"]["fingerprint"]["head"],
        git_head(&repo, "HEAD")
    );
    assert_eq!(report.provenance["candidate"]["fingerprint"]["base"], base);
    assert_eq!(report.deterministic_gates.len(), 1);
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(provider.verify_calls.len(), 1);
}

#[test]
fn adopt_no_op_pre_existing_candidate_when_base_equals_candidate() {
    // #8895: a recovery agent prepares an immutable candidate commit, the cook
    // records that commit AS the task base, and the provider reviews it and
    // returns no-op. With an explicit candidate_ref the base is rebased to the
    // candidate's parent so the immutable commit is adopted and promoted.
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "agent@example.test"]);
    git(&repo, &["config", "user.name", "Agent"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("lib.rs"), "base\n").expect("write base");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "base"]);
    // The pre-existing immutable candidate the recovery agent committed.
    std::fs::write(repo.join("lib.rs"), "candidate\n").expect("write candidate");
    git(&repo, &["commit", "-am", "recovery: prepared candidate"]);
    let candidate = git_head(&repo, "HEAD");

    let source_path = temp.path().join("outcome.json");
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task",
        "status": "no_op",
        "artifacts": []
    })
    .to_string();
    std::fs::write(&source_path, &source).expect("write no-op outcome");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: None,
            // The cook recorded the candidate commit itself as the task base.
            task_base_sha: Some(candidate.clone()),
            candidate_ref: Some(candidate.clone()),
            to_worktree: "repo@promotion".to_string(),
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
    )
    .expect("pre-existing immutable candidate is adopted after no-op review");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.patch_artifact.id, "committed-changes");
    assert_eq!(report.changed_files, vec!["lib.rs"]);
    assert_eq!(report.provenance["base_ref"], git_head(&repo, "HEAD~1"));
    assert_eq!(report.provenance["historical_task_base"], candidate);
    assert_eq!(
        report.provenance["candidate"]["fingerprint"]["head"],
        candidate
    );
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(provider.verify_calls.len(), 1);
}

#[test]
fn adoption_scopes_a_rebased_two_file_candidate_to_its_parent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "agent@example.test"]);
    git(&repo, &["config", "user.name", "Agent"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("base.txt"), "base\n").expect("write base");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "historical base"]);
    let historical_base = git_head(&repo, "HEAD");
    for (path, contents) in [("upstream-a.txt", "a\n"), ("upstream-b.txt", "b\n")] {
        std::fs::write(repo.join(path), contents).expect("write upstream");
        git(&repo, &["add", path]);
        git(&repo, &["commit", "-m", "intervening upstream"]);
    }
    let candidate_base = git_head(&repo, "HEAD");
    git(&repo, &["checkout", "-b", "candidate"]);
    std::fs::write(repo.join("candidate-a.txt"), "a\n").expect("write candidate a");
    std::fs::write(repo.join("candidate-b.txt"), "b\n").expect("write candidate b");
    git(&repo, &["add", "candidate-a.txt", "candidate-b.txt"]);
    git(&repo, &["commit", "-m", "candidate"]);
    let candidate = git_head(&repo, "HEAD");
    let (source_path, source) = write_empty_patch_source(&temp);

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("adopted-run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: None,
            task_base_sha: Some(historical_base.clone()),
            candidate_ref: Some(candidate.clone()),
            to_worktree: "repo@adopted".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["true".to_string()],
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.clone()),
            ..Default::default()
        },
    )
    .expect("rebased candidate promotes from its immutable parent");

    assert_eq!(
        report.changed_files,
        vec!["candidate-a.txt", "candidate-b.txt"]
    );
    assert_eq!(report.provenance["base_ref"], candidate_base);
    assert_eq!(
        report.provenance["commit_range"],
        format!("{candidate_base}..{candidate}")
    );
    assert_eq!(
        report.provenance["commits"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(report.provenance["historical_task_base"], historical_base);
}

#[test]
fn adoption_rejects_an_unrelated_historical_task_base() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "agent@example.test"]);
    git(&repo, &["config", "user.name", "Agent"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("base.txt"), "base\n").expect("write base");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "base"]);
    std::fs::write(repo.join("candidate.txt"), "candidate\n").expect("write candidate");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "candidate"]);
    let candidate = git_head(&repo, "HEAD");
    git(&repo, &["checkout", "--orphan", "unrelated"]);
    git(&repo, &["rm", "-rf", "."]);
    std::fs::write(repo.join("unrelated.txt"), "unrelated\n").expect("write unrelated");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "unrelated"]);
    let unrelated = git_head(&repo, "HEAD");
    git(&repo, &["checkout", "--detach", &candidate]);
    let (source_path, source) = write_empty_patch_source(&temp);

    let error = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("adopted-run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: None,
            task_base_sha: Some(unrelated),
            candidate_ref: Some(candidate),
            to_worktree: "repo@adopted".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
        &mut FakePromotionWorkspaceProvider {
            workspace_path: Some(repo),
            ..Default::default()
        },
    )
    .expect_err("unrelated historical base must fail closed");

    assert!(error
        .message
        .contains("unrelated to the adopted candidate parent"));
}

#[test]
fn promote_exports_all_agent_commits_after_the_recorded_task_base() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "agent@example.test"]);
    git(&repo, &["config", "user.name", "Agent"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("first.txt"), "base\n").expect("base");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "base"]);
    let base = git_head(&repo, "HEAD");
    git(&repo, &["checkout", "-b", "agent/work"]);
    std::fs::write(repo.join("first.txt"), "one\n").expect("first");
    git(&repo, &["commit", "-am", "agent: first"]);
    std::fs::write(repo.join("second.txt"), "two\n").expect("second");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "agent: second"]);
    let (source_path, source) = write_empty_patch_source(&temp);

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run-two-commits".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo.clone()),
            base_ref: Some("main".to_string()),
            task_base_sha: Some(base.clone()),
            candidate_ref: None,
            to_worktree: "repo@promoted".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
        &mut FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.clone()),
            ..Default::default()
        },
    )
    .expect("commits promoted");

    assert_eq!(report.provenance["base_ref"].as_str(), Some(base.as_str()));
    assert_eq!(
        report.provenance["commits"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(report.provenance["commits"][0]["subject"], "agent: first");
    assert_eq!(report.provenance["commits"][1]["subject"], "agent: second");
}

#[test]
fn validate_artifact_content_rejects_sha_mismatch() {
    let artifact = AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "patch".to_string(),
        kind: "patch".to_string(),
        name: None,
        label: None,
        role: None,
        semantic_key: None,
        path: Some("changes.patch".to_string()),
        url: None,
        mime: None,
        size_bytes: Some(VALID_PATCH.len() as u64),
        sha256: Some("0".repeat(64)),
        metadata: Value::Null,
    };

    let err = validate_artifact_content(&artifact, VALID_PATCH).expect_err("sha rejected");

    assert!(err.message.contains("sha256 mismatch"));
}

#[test]
fn review_only_patch_cannot_be_selected_for_promotion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (_, source) = write_patch_source(&temp);
    let mut outcome: AgentTaskOutcome = serde_json::from_str(&source).expect("outcome JSON");
    outcome.artifacts[0].size_bytes = Some(128);
    outcome.artifacts[0].metadata = serde_json::json!({ "review_only": true });

    let error = select_patch_artifact(&outcome, Some("patch"))
        .expect_err("review-only external patch must not be selectable");

    assert!(error.message.contains("no matching patch artifact"));
}

#[test]
fn resume_promoted_patch_rebuilds_green_proof_from_pending_post_apply_checkpoint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target");
    std::fs::create_dir(&target).expect("target");
    git(&target, &["init"]);
    git(&target, &["config", "user.email", "test@example.com"]);
    git(&target, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(target.join("src")).expect("src");
    std::fs::write(target.join("src/lib.rs"), "old\n").expect("base");
    git(&target, &["add", "."]);
    git(&target, &["commit", "-m", "base"]);
    std::fs::write(target.join("src/lib.rs"), "new\n").expect("apply candidate");
    git(&target, &["add", "."]);
    git(&target, &["commit", "-m", "candidate plus gate correction"]);
    let (source_path, source) = write_patch_source(&temp);
    let options = AgentTaskPromotionOptions {
        source,
        source_run_id: Some("run-8307".to_string()),
        source_path: Some(source_path),
        source_worktree_path: None,
        base_ref: Some("main".to_string()),
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: "repo@fix-8307".to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions {
            verify: vec!["true".to_string()],
            private_verify: vec!["true".to_string()],
            private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
            ..Default::default()
        },
        provider_command: None,
        provider_invocation: None,
    };
    let previous = serde_json::json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": "verification_pending",
        "source_run_id": "run-8307",
        "source": { "task_id": "task-1" },
        "to_worktree": "repo@fix-8307",
        "target": { "worktree": "repo@fix-8307", "path": target },
        "patch_artifact": { "id": "patch", "kind": "patch", "sha256": sha256_hex(VALID_PATCH) },
        "verified_base": { "base": "main", "sha": "checkpointed-base" },
        "provenance": {
            "resume_contract": {
                "inputs": { "base_ref": "main", "task_base_sha": null, "candidate_ref": null },
                "gates": serde_json::to_value(&options.gates).expect("gate contract")
            }
        }
    });

    let report = resume_promoted_patch(options, &target, &previous).expect("resume proof");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(
        report.command_evidence[0].command,
        vec!["git", "apply", "--reverse", "--check", "-"]
    );
    assert_eq!(report.gate_results.len(), 2);
    assert_eq!(
        report.gate_results[0].status,
        homeboy_core::gate::HomeboyGateStatus::Passed
    );
    assert_eq!(report.provenance["resumed_post_apply_promotion"], true);
    assert_eq!(
        report.verified_base.expect("checkpointed base").sha,
        "checkpointed-base"
    );
    assert!(report.provenance["candidate"].is_object());
    assert_eq!(report.deterministic_gates.len(), 2);
    assert_eq!(
        report.deterministic_gates[1].visibility,
        AgentTaskGateVisibility::Private
    );
    assert_eq!(
        report.deterministic_gates[1].reveal_policy,
        AgentTaskGateRevealPolicy::FullEvidence
    );
    assert_eq!(report.provenance["resumed_post_apply_promotion"], true);
}

#[test]
fn legacy_post_apply_checkpoint_recovers_only_with_corrected_non_main_base() {
    // #9400 regression sequence: review selected a Cook artifact for a trunk
    // repository, a legacy generated promote command defaulted to main after
    // applying it, then the corrected review command resumes the exact target.
    let temp = tempfile::tempdir().expect("tempdir");
    let remote = temp.path().join("origin.git");
    let target = temp.path().join("target");
    git(
        temp.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=trunk",
            remote.to_str().unwrap(),
        ],
    );
    std::fs::create_dir(&target).expect("target");
    git(&target, &["init", "--initial-branch=trunk"]);
    git(&target, &["config", "user.email", "test@example.com"]);
    git(&target, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(target.join("src")).expect("src");
    std::fs::write(target.join("src/lib.rs"), "old\n").expect("base");
    git(&target, &["add", "."]);
    git(&target, &["commit", "-m", "base"]);
    git(
        &target,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&target, &["push", "-u", "origin", "trunk"]);
    std::fs::write(target.join("src/lib.rs"), "new\n").expect("legacy applied patch");
    let candidate = crate::agent_task_promotion::candidate_fingerprint(target.to_str().unwrap())
        .expect("legacy candidate");
    let (source_path, source) = write_patch_source(&temp);
    let options = AgentTaskPromotionOptions {
        source,
        source_run_id: Some("cook-9400-attempt-1".to_string()),
        source_path: Some(source_path),
        source_worktree_path: None,
        base_ref: Some("trunk".to_string()),
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: "fixture@target".to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions::default(),
        provider_command: None,
        provider_invocation: None,
    };
    let checkpoint = serde_json::json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": "verification_pending",
        "source_run_id": "cook-9400-attempt-1",
        "source": { "task_id": "task-1" },
        "to_worktree": "fixture@target",
        "target": { "worktree": "fixture@target", "path": target },
        "patch_artifact": { "id": "patch", "kind": "patch", "sha256": sha256_hex(VALID_PATCH) },
        "provenance": {
            "post_apply": true,
            "candidate": candidate,
            "resume_contract": {
                "inputs": { "base_ref": "main", "task_base_sha": null, "candidate_ref": null },
                "gates": serde_json::to_value(&options.gates).expect("gate contract"),
            },
        },
    });

    let resumed = resume_promoted_patch(options.clone(), &target, &checkpoint)
        .expect("corrected trunk command resumes the legacy checkpoint");
    assert_eq!(resumed.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(resumed.verified_base.expect("corrected base").base, "trunk");

    std::fs::write(target.join("unrelated.rs"), "conflict\n").expect("conflicting edit");
    let error = resume_promoted_patch(options, &target, &checkpoint)
        .expect_err("conflicting target edits fail closed");
    assert!(error
        .message
        .contains("differs from the exact checkpointed"));
}

#[test]
fn resume_applied_promotion_reruns_gates_for_exact_dirty_candidate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target");
    std::fs::create_dir(&target).expect("target");
    git(&target, &["init"]);
    git(&target, &["config", "user.email", "test@example.com"]);
    git(&target, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(target.join("src")).expect("src");
    std::fs::write(target.join("src/lib.rs"), "old\n").expect("base");
    git(&target, &["add", "."]);
    git(&target, &["commit", "-m", "base"]);
    std::fs::write(target.join("src/lib.rs"), "new\n").expect("apply candidate");
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(target.to_string_lossy().as_ref())
            .expect("candidate fingerprint");
    let (source_path, source) = write_patch_source(&temp);
    let options = |rerun_completed_gates| AgentTaskPromotionOptions {
        source: source.clone(),
        source_run_id: Some("run-9392".to_string()),
        source_path: Some(source_path.clone()),
        source_worktree_path: None,
        base_ref: None,
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: "repo@fix-9392".to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions {
            verify: vec!["true".to_string()],
            rerun_completed_gates,
            ..Default::default()
        },
        provider_command: None,
        provider_invocation: None,
    };
    let previous = serde_json::json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": "applied",
        "source_run_id": "run-9392",
        "source": { "task_id": "task-1" },
        "to_worktree": "repo@fix-9392",
        "target": { "worktree": "repo@fix-9392", "path": target },
        "patch_artifact": { "id": "patch", "kind": "patch", "sha256": sha256_hex(VALID_PATCH) },
        "provenance": { "candidate": candidate },
    });

    let rejected = resume_promoted_patch(options(false), &target, &previous)
        .expect_err("applied promotion requires explicit gate rerun");
    assert!(rejected.message.contains("explicit completed-gate rerun"));

    let report = resume_promoted_patch(options(true), &target, &previous)
        .expect("exact applied candidate resumes gates");
    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.gate_results.len(), 1);
    assert_eq!(report.provenance["resumed_post_apply_promotion"], true);
}

#[test]
fn promotion_options_keep_flat_verify_gate_serialized_shape() {
    // #4910: the shared VerifyGateOptions is `#[serde(flatten)]`-embedded so
    // the historical flat `verify` / `private_verify` / `private_gate_reveal`
    // keys must stay at the top level of the serialized options.
    let options = AgentTaskPromotionOptions {
        source: "source.json".to_string(),
        source_run_id: Some("run-1".to_string()),
        source_path: None,
        source_worktree_path: None,
        base_ref: None,
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: "repo@flatten".to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions {
            verify: vec!["cargo test".to_string()],
            private_verify: vec!["cargo test --lib hidden".to_string()],
            private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
            ..Default::default()
        },
        provider_command: None,
        provider_invocation: None,
    };

    let value = serde_json::to_value(&options).expect("serialize options");
    assert_eq!(value["verify"], serde_json::json!(["cargo test"]));
    assert_eq!(
        value["private_verify"],
        serde_json::json!(["cargo test --lib hidden"])
    );
    assert_eq!(
        value["private_gate_reveal"],
        serde_json::json!("summary_only")
    );
    assert!(
        value.get("gates").is_none(),
        "flattened gate fields must not nest under a `gates` key: {value}"
    );

    let round_trip: AgentTaskPromotionOptions =
        serde_json::from_value(value).expect("deserialize flat options");
    assert_eq!(round_trip, options);
}

#[test]
fn explicit_candidate_gate_failure_is_recorded_after_normal_promotion_handoff() {
    let (temp, repo, base, candidate) = adopted_commit_repo();
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo),
        verify_exit_code: 1,
        ..Default::default()
    };
    let report = promote_with_provider(
        adopted_commit_options(
            &temp,
            provider.workspace_path.as_deref().expect("workspace"),
            base,
            candidate,
            VerifyGateOptions {
                verify: vec!["failing-gate".to_string()],
                ..Default::default()
            },
        ),
        &mut provider,
    )
    .expect("gate failure is a promotion report");
    assert_eq!(report.status, AgentTaskPromotionStatus::GateFailed);
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(provider.verify_calls.len(), 1);
}

#[test]
fn ordered_gate_failure_skips_downstream_command_with_durable_blocker_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init", "-b", "main"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Homeboy Test"]);
    std::fs::create_dir_all(workspace.join("src")).expect("source directory");
    std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "base"]);
    let (source_path, source) = write_patch_source(&temp);
    let downstream_marker = temp.path().join("broad-gate-ran");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(workspace),
        apply_to_git: true,
        run_verify_command: true,
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("ordered-fail-fast".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@target".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec![
                    "exit 1".to_string(),
                    format!("touch '{}'", downstream_marker.display()),
                ],
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("failed gate produces promotion report");

    assert_eq!(report.status, AgentTaskPromotionStatus::GateFailed);
    assert_eq!(provider.verify_calls.len(), 1, "broad gate was not invoked");
    assert!(!downstream_marker.exists(), "broad gate was not executed");
    assert_eq!(
        report.deterministic_gates[1].status,
        AgentTaskGateStatus::Skipped
    );
    assert_eq!(
        report.deterministic_gates[1]
            .skip_reason
            .as_ref()
            .expect("skip reason")
            .blocking_gate_id,
        "gate-1"
    );
    assert_eq!(
        report.gate_results[1].status,
        homeboy_core::gate::HomeboyGateStatus::Skipped
    );
    assert!(report
        .operator_notification
        .message
        .contains("passed=[], failed=[gate-1], skipped=[gate-2]"));
}

#[test]
fn continue_all_gate_policy_runs_downstream_command_after_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init", "-b", "main"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Homeboy Test"]);
    std::fs::create_dir_all(workspace.join("src")).expect("source directory");
    std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "base"]);
    let (source_path, source) = write_patch_source(&temp);
    let downstream_marker = temp.path().join("broad-gate-ran");
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(workspace),
        apply_to_git: true,
        run_verify_command: true,
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("continue-all".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@target".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec![
                    "exit 1".to_string(),
                    format!("touch '{}'", downstream_marker.display()),
                ],
                execution_policy: AgentTaskGateExecutionPolicy::ContinueAll,
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("failed gate produces promotion report");

    assert_eq!(provider.verify_calls.len(), 2);
    assert!(
        downstream_marker.exists(),
        "continue-all ran the broad gate"
    );
    assert_eq!(
        report.deterministic_gates[1].status,
        AgentTaskGateStatus::Succeeded
    );
}

#[test]
fn promotion_runs_gates_in_the_destination_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init", "-b", "main"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Homeboy Test"]);
    std::fs::create_dir_all(workspace.join("src")).expect("source directory");
    std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "base"]);
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(workspace.clone()),
        apply_to_git: true,
        run_verify_command: true,
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("immutable-candidate-first-attempt".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@target".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["test \"$(cat src/lib.rs)\" = new".to_string()],
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("destination gate accepts the promoted candidate");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(provider.verify_worktrees_clean, vec![false]);
    assert_eq!(provider.verify_calls[0].0, workspace);
    assert!(Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&workspace)
        .output()
        .expect("inspect promotion destination")
        .stdout
        .starts_with(b" M src/lib.rs"));
    let checkout = report.deterministic_gates[0]
        .candidate_checkout
        .as_ref()
        .expect("gate candidate checkout identity");
    assert_eq!(
        checkout.tree,
        report.provenance["candidate_checkout"]["tree"]
    );
    assert_eq!(
        checkout.candidate_sha256,
        report.provenance["candidate_checkout"]["candidate_sha256"]
    );
}

#[test]
fn promotion_hydrates_destination_package_execution_projections_before_gates() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        git(&workspace, &["init", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::create_dir_all(workspace.join("src")).expect("source directory");
        std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
        std::fs::write(
            workspace.join("homeboy.json"),
            r#"{"id":"projection-fixture"}"#,
        )
        .expect("component manifest");
        std::fs::write(workspace.join("dependency.lock"), "fixture-lock\n")
            .expect("dependency lock fixture");
        std::fs::write(
            workspace.join("homeboy-deps.json"),
            r##"{"provider":"fixture-provider","commands":{"install":{"argv":["sh","-c","mkdir -p node_modules/fixture node_modules/.bin && printf 'console.log(\"explicit module path\")\n' > node_modules/fixture/explicit.js && printf '#!/bin/sh\nexit 0\n' > node_modules/.bin/fixture-bin && chmod +x node_modules/.bin/fixture-bin"]}}}"##,
        )
        .expect("provider declaration");
        git(&workspace, &["add", "."]);
        git(&workspace, &["commit", "-m", "base"]);
        let (source_path, source) = write_patch_source(&temp);
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(workspace.clone()),
            apply_to_git: true,
            run_verify_command: true,
            ..Default::default()
        };

        let report = promote_with_provider(
            AgentTaskPromotionOptions {
                source,
                source_run_id: Some("nested-dependency-hydration".to_string()),
                source_path: Some(source_path),
                source_worktree_path: None,
                base_ref: None,
                task_base_sha: None,
                candidate_ref: None,
                to_worktree: "fixture@target".to_string(),
                task_id: None,
                artifact_id: None,
                dry_run: false,
                gates: VerifyGateOptions {
                    // The first gate addresses a module directly; the second
                    // consumes the executable projection the provider created.
                    verify: vec![
                        "node ./node_modules/fixture/explicit.js".to_string(),
                        "./node_modules/.bin/fixture-bin".to_string(),
                    ],
                    ..Default::default()
                },
                provider_command: None,
                provider_invocation: None,
            },
            &mut provider,
        )
        .expect("destination dependency setup makes both gate forms runnable");

        assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(
            report.provenance["candidate_checkout_setup"],
            serde_json::json!([])
        );
        assert_eq!(
            report.provenance["destination_gate_setup"][0]["workspace"],
            "destination_gate_workspace"
        );
        assert_eq!(
            report.provenance["destination_gate_setup"][0]["setup_capability"],
            "dependency.install"
        );
        assert!(
            workspace.join("node_modules/fixture/explicit.js").exists(),
            "destination hydration creates the explicit module path"
        );
        assert!(
            workspace.join("node_modules/.bin/fixture-bin").exists(),
            "destination hydration creates the executable projection"
        );
    });
}

#[test]
fn promotion_can_disable_candidate_dependency_hydration() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init", "-b", "main"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Homeboy Test"]);
    std::fs::create_dir_all(workspace.join("src")).expect("source directory");
    std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "base"]);
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(workspace),
        apply_to_git: true,
        ..Default::default()
    };
    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("disable-dependency-hydration".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@target".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["true".to_string()],
                hydrate_dependencies: false,
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("disabled setup still runs gates");
    assert_eq!(
        report.provenance["candidate_checkout_setup"],
        serde_json::json!([])
    );
    assert_eq!(
        report.provenance["destination_gate_setup"],
        serde_json::json!([])
    );
}

#[test]
fn promotion_setup_failure_is_bounded_and_never_dispatches_a_gate() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        git(&workspace, &["init", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::create_dir_all(workspace.join("src")).expect("source directory");
        std::fs::create_dir_all(workspace.join("component")).expect("component directory");
        std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
        std::fs::write(
            workspace.join("component/homeboy-deps.json"),
            r#"{"provider":"fixture-provider","commands":{"install":{"argv":["sh","-c","exit 23"]}}}"#,
        )
        .expect("provider declaration");
        git(&workspace, &["add", "."]);
        git(&workspace, &["commit", "-m", "base"]);
        let (source_path, source) = write_patch_source(&temp);
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(workspace),
            apply_to_git: true,
            ..Default::default()
        };

        let error = promote_with_provider(
            AgentTaskPromotionOptions {
                source,
                source_run_id: Some("setup-failure-no-gate-dispatch".to_string()),
                source_path: Some(source_path),
                source_worktree_path: None,
                base_ref: None,
                task_base_sha: None,
                candidate_ref: None,
                to_worktree: "fixture@target".to_string(),
                task_id: None,
                artifact_id: None,
                dry_run: false,
                gates: VerifyGateOptions {
                    verify: vec!["false".to_string()],
                    ..Default::default()
                },
                provider_command: None,
                provider_invocation: None,
            },
            &mut provider,
        )
        .expect_err("failed setup stops before a gate can spend repair capacity");

        assert_eq!(error.code.as_str(), "dependency_step_failed");
        assert_eq!(
            error.details["cause"]["classification"],
            "destination_gate_setup"
        );
        assert!(
            error.details["cause"]["details"]
                .as_str()
                .expect("bounded setup details")
                .len()
                <= 20 * 1024
        );
        assert!(
            provider.verify_calls.is_empty(),
            "no gate/provider dispatch occurs"
        );
    });
}

#[test]
fn missing_destination_tool_is_a_typed_setup_failure_before_provider_verification() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init", "-b", "main"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Homeboy Test"]);
    std::fs::create_dir_all(workspace.join("src")).expect("source directory");
    std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "base"]);
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(workspace),
        apply_to_git: true,
        ..Default::default()
    };

    let error = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("missing-destination-tool".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@target".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["homeboy-fixture-missing-tool".to_string()],
                hydrate_dependencies: false,
                gate_toolchains: vec![crate::agent_task_gate::AgentTaskGateToolchainRequirement {
                    command: "homeboy-fixture-missing-tool".to_string(),
                    probe_arguments: vec!["--version".to_string()],
                }],
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect_err("missing tool is setup evidence, not a candidate gate failure");

    assert_eq!(error.code.as_str(), "dependency_step_failed");
    assert_eq!(
        error.details["cause"]["classification"],
        "destination_gate_toolchain"
    );
    assert_eq!(
        error.details["cause"]["retry_action"],
        "retry_dependency_hydration"
    );
    assert!(
        provider.verify_calls.is_empty(),
        "provider budget was not spent"
    );
}

#[test]
fn promotion_rejects_mutation_after_checkpoint_before_gate_materialization() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init", "-b", "main"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Homeboy Test"]);
    std::fs::create_dir_all(workspace.join("src")).expect("source directory");
    std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "base"]);
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(workspace.clone()),
        apply_to_git: true,
        ..Default::default()
    };

    let error = promote_with_provider_and_checkpoint(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("immutable-candidate-checkpoint".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@target".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["true".to_string()],
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
        &mut |_| {
            std::fs::write(workspace.join("src/lib.rs"), "tampered\n")
                .expect("mutate destination after checkpoint");
            Ok(())
        },
    )
    .expect_err("verification rejects a destination that changed after checkpointing");

    assert!(error
        .message
        .contains("differs from the checkpointed candidate"));
}

#[test]
fn resumed_verification_runs_destination_gate_for_exact_dirty_candidate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target");
    std::fs::create_dir(&target).expect("target");
    git(&target, &["init", "-b", "main"]);
    git(&target, &["config", "user.email", "test@example.com"]);
    git(&target, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(target.join("src")).expect("src");
    std::fs::write(target.join("src/lib.rs"), "old\n").expect("base");
    git(&target, &["add", "."]);
    git(&target, &["commit", "-m", "base"]);
    std::fs::write(target.join("src/lib.rs"), "new\n").expect("apply candidate");
    let candidate =
        crate::agent_task_promotion::candidate_fingerprint(target.to_string_lossy().as_ref())
            .expect("candidate fingerprint");
    let (source_path, source) = write_patch_source(&temp);
    let options = AgentTaskPromotionOptions {
        source,
        source_run_id: Some("immutable-candidate-follow-up".to_string()),
        source_path: Some(source_path),
        source_worktree_path: None,
        base_ref: None,
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: "fixture@target".to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions {
            verify: vec!["test \"$(cat src/lib.rs)\" = new".to_string()],
            rerun_completed_gates: true,
            ..Default::default()
        },
        provider_command: None,
        provider_invocation: None,
    };
    let previous = serde_json::json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": "applied",
        "source_run_id": "immutable-candidate-follow-up",
        "source": { "task_id": "task-1" },
        "to_worktree": "fixture@target",
        "target": { "worktree": "fixture@target", "path": target },
        "patch_artifact": { "id": "patch", "kind": "patch", "sha256": sha256_hex(VALID_PATCH) },
        "provenance": { "candidate": candidate },
    });

    let report = resume_promoted_patch(options.clone(), &target, &previous)
        .expect("follow-up verification accepts the exact dirty candidate");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.deterministic_gates.len(), 1);
    assert!(report.deterministic_gates[0].candidate_checkout.is_some());
    assert!(Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&target)
        .output()
        .expect("inspect promotion destination")
        .stdout
        .starts_with(b" M src/lib.rs"));
    std::fs::write(target.join("unrelated.txt"), "drift\n").expect("diverge target");
    let error = resume_promoted_patch(options, &target, &previous);
    assert!(error.is_err(), "divergent destination fails closed");
}

#[test]
fn gate_failure_preserves_the_pre_gate_candidate_baseline_for_feedback_retry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    git(&workspace, &["init", "-b", "main"]);
    git(&workspace, &["config", "user.email", "test@example.com"]);
    git(&workspace, &["config", "user.name", "Homeboy Test"]);
    std::fs::create_dir_all(workspace.join("src")).expect("source directory");
    std::fs::write(workspace.join("src/lib.rs"), "old\n").expect("base file");
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-m", "base"]);
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(workspace.clone()),
        apply_to_git: true,
        verify_exit_code: 1,
        ..Default::default()
    };
    let mut checkpoint = None;
    let report = promote_with_provider_and_checkpoint(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("gate-failure-run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "fixture@target".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["failing-gate".to_string()],
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
        &mut |saved| {
            checkpoint = Some(saved.clone());
            Ok(())
        },
    )
    .expect("gate failure is durable promotion evidence");

    assert_eq!(report.status, AgentTaskPromotionStatus::GateFailed);
    let baseline = report.provenance["gate_feedback_baseline"].clone();
    assert_eq!(
        checkpoint.expect("post-apply checkpoint").provenance["gate_feedback_baseline"],
        baseline
    );
    assert!(baseline["current_diff"]
        .as_str()
        .is_some_and(|diff| !diff.is_empty()));

    let feedback_baseline = serde_json::json!({
        "current_diff": baseline["current_diff"],
        "patch_artifact": report.patch_artifact,
    });
    crate::agent_task_candidate_baseline::validate_gate_feedback_candidate_baseline(
        &workspace,
        &feedback_baseline,
    )
    .expect("exactly matching applied candidate is safe to retry");

    std::fs::write(workspace.join("unrelated.txt"), "drift\n").expect("divergent dirt");
    assert!(
        crate::agent_task_candidate_baseline::validate_gate_feedback_candidate_baseline(
            &workspace,
            &feedback_baseline,
        )
        .is_err()
    );
}

#[test]
fn provider_failure_surfaces_bounded_stdout_and_stderr_evidence() {
    let request = AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: "target-workspace".to_string(),
        patch: None,
        patch_path: "changes.patch".to_string(),
        changed_files: vec!["src/lib.rs".to_string()],
        gate_feedback_baseline: None,
        dry_run: false,
        trusted_unpushed_candidate_destination: None,
    };

    let error = run_provider_command(
        &CommandInvocation {
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf provider-stdout; printf provider-stderr >&2; exit 7".to_string(),
            ],
            ..Default::default()
        },
        &request,
    )
    .expect_err("provider failure");

    assert_eq!(error.details["command_evidence"]["exit_code"], 7);
    assert_eq!(
        error.details["command_evidence"]["stdout"],
        "provider-stdout"
    );
    assert_eq!(
        error.details["command_evidence"]["stderr"],
        "provider-stderr"
    );
}

#[test]
fn configured_provider_timeout_is_bounded_and_retains_command_evidence() {
    let request = AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: "target-workspace".to_string(),
        patch: None,
        patch_path: "changes.patch".to_string(),
        changed_files: vec!["src/lib.rs".to_string()],
        gate_feedback_baseline: None,
        dry_run: false,
        trusted_unpushed_candidate_destination: None,
    };
    let started = std::time::Instant::now();
    let error = run_provider_command_with_timeout(
        &CommandInvocation {
            argv: vec!["sh".to_string(), "-c".to_string(), "sleep 2".to_string()],
            ..Default::default()
        },
        &request,
        std::time::Duration::from_millis(100),
    )
    .expect_err("silent provider must be terminated");

    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(error.message.contains("timed out after 100 ms"));
    assert_eq!(
        error.details["command_evidence"]["command"],
        "sh -c sleep 2"
    );
}

#[test]
fn provider_response_validation_distinguishes_json_schema_and_required_field_errors() {
    let request = AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: "target-workspace".to_string(),
        patch: None,
        patch_path: "changes.patch".to_string(),
        changed_files: vec!["src/lib.rs".to_string()],
        gate_feedback_baseline: None,
        dry_run: false,
        trusted_unpushed_candidate_destination: None,
    };
    let cases = [
        ("{", "Invalid JSON"),
        (
            r#"{"workspace_path":"/workspace"}"#,
            "expected homeboy/agent-task-promotion-apply-response/v1, got missing schema",
        ),
        (
            r#"{"schema":"homeboy/agent-task-promotion-apply-request/v1"}"#,
            "expected homeboy/agent-task-promotion-apply-response/v1, got homeboy/agent-task-promotion-apply-request/v1",
        ),
        (
            r#"{"schema":1}"#,
            "expected homeboy/agent-task-promotion-apply-response/v1, got 1",
        ),
        (
            r#"{"schema":"homeboy/agent-task-promotion-apply-response/v1"}"#,
            "missing field `workspace_path`",
        ),
    ];

    for (response, expected) in cases {
        let error = run_provider_command(
            &CommandInvocation {
                argv: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("printf '%s' '{response}'"),
                ],
                ..Default::default()
            },
            &request,
        )
        .expect_err("invalid provider response");

        assert!(error.message.contains(expected), "{}", error.message);
        assert_eq!(error.details["command_evidence"]["exit_code"], 0);
        assert_eq!(error.details["command_evidence"]["stdout"], response);
    }
}

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
        assert_eq!(record.metadata["artifact_projection"]["status"], "complete");

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
    let temp = tempfile::tempdir().expect("tempdir");
    let patch_path = temp.path().join("remediation.patch");
    std::fs::write(&patch_path, VALID_PATCH).expect("write remediation patch");
    let baseline = serde_json::json!({
        "source_run_id": "source-run",
        "source_task_id": "source-task",
        "source_patch_task_id": "source-task",
        "to_worktree": "fixture@target",
        "current_diff": "diff --git a/a b/a",
        "failed_gates": [],
        "patch_artifact": { "path": "/candidate.patch", "sha256": "a".repeat(64) }
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
    assert_eq!(
        provider.apply_calls[0].gate_feedback_baseline,
        Some(baseline),
        "only canonical artifact metadata authorizes the dirty target"
    );
}

#[test]
fn promote_recoverable_candidate_rejects_multiple_actionable_patches() {
    let (result, apply_calls) = promote_recoverable_patch_count(2);
    assert!(result
        .expect_err("ambiguous candidate rejected")
        .message
        .contains("exactly one actionable patch"));
    assert_eq!(apply_calls, 0);
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
        base_ref: None,
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: "repo@fix-8307".to_string(),
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
    };
    let previous = serde_json::json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": "verification_pending",
        "source_run_id": "run-8307",
        "source": { "task_id": "task-1" },
        "to_worktree": "repo@fix-8307",
        "target": { "worktree": "repo@fix-8307", "path": target },
        "patch_artifact": { "id": "patch", "kind": "patch", "sha256": sha256_hex(VALID_PATCH) },
    });

    let report = resume_promoted_patch(options, &target, &previous).expect("resume proof");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(
        report.command_evidence[0].command,
        vec!["git", "apply", "--reverse", "--check", "-"]
    );
    assert_eq!(report.gate_results.len(), 1);
    assert_eq!(
        report.gate_results[0].status,
        homeboy_core::gate::HomeboyGateStatus::Passed
    );
    assert_eq!(report.provenance["resumed_post_apply_promotion"], true);
    assert!(report.provenance["candidate"].is_object());
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

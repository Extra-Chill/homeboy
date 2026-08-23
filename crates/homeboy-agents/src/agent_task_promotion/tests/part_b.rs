//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::apply::{
    run_provider_command, run_provider_command_with_timeout, AgentTaskPromotionApplyRequest,
    ExternalPromotionWorkspaceProvider, AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA,
    AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
};
use super::super::promote::{
    normalize_promotion_patch, resume_promoted_patch, select_patch_artifact,
    validate_artifact_content,
};
use super::super::types::{
    AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionSource,
    AgentTaskPromotionStatus, AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};
use super::*;
use crate::agent_task::{AgentTaskArtifact, AgentTaskOutcome, AGENT_TASK_ARTIFACT_SCHEMA};
use crate::agent_task_gate::{
    AgentTaskGateRevealPolicy, AgentTaskGateVisibility, VerifyGateOptions,
};
use homeboy_core::command_invocation::CommandInvocation;
use serde_json::Value;
use std::process::Command;

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
                "cat >/dev/null; printf provider-stdout; printf provider-stderr >&2; exit 7"
                    .to_string(),
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
                    "printf '%s' \"$1\"".to_string(),
                    "sh".to_string(),
                    response.to_string(),
                ],
                ..Default::default()
            },
            &request,
        )
        .expect_err("invalid provider response");

        // Print `details` too. `Error::message` for an infrastructure failure
        // is the canned string "IO error"; the operation that actually failed
        // lives only in `details` (#12741). Asserting on `message` alone made
        // every such failure undiagnosable from CI.
        assert!(
            error.message.contains(expected),
            "{} {}",
            error.message,
            error.details
        );
        assert_eq!(error.details["command_evidence"]["exit_code"], 0);
        assert_eq!(error.details["command_evidence"]["stdout"], response);
    }
}

/// A provider that exits without draining its request keeps its own verdict.
///
/// Rust sets SIGPIPE to SIG_IGN, so writing to a pipe whose read end has closed
/// returns `BrokenPipe` here instead of terminating this process. Treating that
/// as `internal_io` replaced the provider's actual result with the canned
/// message "IO error" and discarded its exit code and captured stdout (#12741).
///
/// Every provider in this module's other tests exits immediately without
/// reading stdin, so they hit this racily -- whichever side won scheduling
/// decided whether the run passed. The request below is deliberately larger
/// than a pipe buffer (64 KiB on Linux) so `write_all` cannot possibly finish
/// before the provider exits. That makes `BrokenPipe` certain rather than
/// probable, which is what makes this a regression test rather than one more
/// dice roll.
#[test]
fn provider_that_exits_without_draining_its_request_still_yields_its_own_verdict() {
    let request = AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: "target-workspace".to_string(),
        patch: None,
        patch_path: "changes.patch".to_string(),
        changed_files: (0..20_000)
            .map(|index| format!("src/generated/file_{index}.rs"))
            .collect(),
        gate_feedback_baseline: None,
        dry_run: false,
        trusted_unpushed_candidate_destination: None,
    };

    let error = run_provider_command(
        &CommandInvocation {
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s' \"$1\"".to_string(),
                "sh".to_string(),
                r#"{"schema":"homeboy/agent-task-promotion-apply-response/v1"}"#.to_string(),
            ],
            ..Default::default()
        },
        &request,
    )
    .expect_err("the provider response is still validated");

    // The provider's own verdict, not an opaque infrastructure error.
    assert!(
        error.message.contains("missing field `workspace_path`"),
        "a provider that ignored its request must still be judged on what it \
         returned, not on the write that failed: {} {}",
        error.message,
        error.details
    );
    assert_eq!(error.details["command_evidence"]["exit_code"], 0);
}

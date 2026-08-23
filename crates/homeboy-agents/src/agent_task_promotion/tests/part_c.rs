//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::apply::{
    run_provider_command, AgentTaskPromotionApplyRequest, TrustedUnpushedCandidateDestination,
    AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA, AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
};
use super::super::promote::{normalize_promotion_patch, promote, select_patch_artifact};
use super::super::types::{
    AgentTaskPromotionOptions, AgentTaskPromotionStatus, AgentTaskPromotionTarget,
    AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};
use super::*;
use crate::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::agent_task_gate::{AgentTaskGateRevealPolicy, VerifyGateOptions};
use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::lab_contract::AgentTaskDispatchIdentity;
use serde_json::Value;

#[test]
fn bridge_reconciliation_marks_missing_or_mismatched_finalized_bytes_pending() {
    homeboy_core::test_support::with_isolated_home(|_| {
        for (run_id, contents) in [
            ("recovered-missing-finalized", None),
            (
                "recovered-mismatched-finalized",
                Some("different patch bytes"),
            ),
        ] {
            let plan = AgentTaskPlan::new("recovered-lab-plan", Vec::new());
            crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit plan");
            if let Some(contents) = contents {
                let finalized = homeboy_core::paths::artifact_root()
                    .expect("artifact root")
                    .join("executor-finalized")
                    .join(run_id)
                    .join("patch");
                std::fs::create_dir_all(finalized.parent().expect("finalized parent"))
                    .expect("create finalized parent");
                std::fs::write(finalized, contents).expect("write mismatched finalized bytes");
            }
            let aggregate: AgentTaskAggregate = serde_json::from_str(&recovered_runner_aggregate(
                "implement",
                "patch",
                &sha256_hex(VALID_PATCH),
                VALID_PATCH.len(),
            ))
            .expect("recovered aggregate");
            let identity = AgentTaskDispatchIdentity {
                runner_id: "homeboy-lab".to_string(),
                runner_job_id: format!("job-{run_id}"),
                ..Default::default()
            };

            crate::agent_task_promotion::mirror_agent_task_run_plan_aggregate(
                "@runner-plan.json",
                run_id,
                aggregate,
                None,
                Some(&identity),
            )
            .expect("bridge preserves aggregate while surfacing recoverable projection");

            let record = crate::agent_task_lifecycle::status(run_id).expect("lifecycle status");
            assert_eq!(record.metadata["artifact_projection"]["status"], "pending");
            assert_eq!(
                record.metadata["artifact_projection"]["recovery_action"]["kind"],
                "fetch_and_reconcile"
            );
            let artifacts = homeboy_core::observation::ObservationStore::open_initialized()
                .expect("store")
                .list_artifacts(run_id)
                .expect("artifact references");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].artifact_type, "remote_file");
        }
    });
}

#[test]
fn validate_patch_extracts_safe_changed_files() {
    let patch = normalize_promotion_patch(VALID_PATCH, "repo@promoted-task").expect("valid patch");

    assert_eq!(patch.changed_files, vec!["src/lib.rs"]);
    assert_eq!(patch.content, VALID_PATCH);
}

#[test]
fn promote_reports_no_changes_for_empty_patch_metadata() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let prior_promotion_command = std::env::var_os("HOMEBOY_AGENT_TASK_PROMOTION_COMMAND");
        std::env::remove_var("HOMEBOY_AGENT_TASK_PROMOTION_COMMAND");
        let temp = tempfile::tempdir().expect("tempdir");
        let patch_path = temp.path().join("patch.diff");
        std::fs::write(&patch_path, "").expect("write empty patch");
        let source_path = temp.path().join("outcome.json");
        let outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: None,
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch".to_string(),
                kind: "patch".to_string(),
                name: Some("patch.diff".to_string()),
                label: None,
                role: None,
                semantic_key: None,
                path: Some("patch.diff".to_string()),
                url: None,
                mime: Some("text/x-patch".to_string()),
                size_bytes: Some(0),
                sha256: Some(
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
                ),
                metadata: serde_json::json!({ "role": "patch" }),
            }],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };
        let source = serde_json::to_string(&outcome).expect("serialize outcome");
        std::fs::write(&source_path, &source).expect("write source");

        let report = promote(AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run-empty".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "repo@promoted-task".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["cargo test".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                ..Default::default()
            },
            provider_command: None,
            provider_invocation: None,
        })
        .expect("empty patch reports no changes");

        assert_eq!(report.status, AgentTaskPromotionStatus::VerifiedNoChanges);
        assert!(report.changed_files.is_empty());
        match prior_promotion_command {
            Some(command) => std::env::set_var("HOMEBOY_AGENT_TASK_PROMOTION_COMMAND", command),
            None => std::env::remove_var("HOMEBOY_AGENT_TASK_PROMOTION_COMMAND"),
        }
    });
}

#[test]
fn select_patch_artifact_requires_unambiguous_patch() {
    let outcome = AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: "task-1".to_string(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: None,
        failure_classification: None,
        artifacts: vec![
            AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch-a".to_string(),
                kind: "patch".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: Some("a.patch".to_string()),
                url: None,
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: Value::Null,
            },
            AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch-b".to_string(),
                kind: "diff".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: Some("b.patch".to_string()),
                url: None,
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: Value::Null,
            },
        ],
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    };

    let err = select_patch_artifact(&outcome, None).expect_err("ambiguous patch rejected");
    assert!(err.message.contains("multiple patch artifacts"));

    let artifact = select_patch_artifact(&outcome, Some("patch-b")).expect("selected patch");
    assert_eq!(artifact.id, "patch-b");
}

#[test]
fn promotion_options_deserialize_legacy_flat_gate_payload() {
    // Payloads authored before the refactor used flat keys; they must still
    // deserialize into the flattened `gates` field unchanged.
    let payload = serde_json::json!({
        "source": "source.json",
        "to_worktree": "repo@legacy",
        "verify": ["cargo build"],
        "private_verify": [],
        "private_gate_reveal": "full_evidence"
    });

    let options: AgentTaskPromotionOptions =
        serde_json::from_value(payload).expect("deserialize legacy flat payload");
    assert_eq!(options.gates.verify, vec!["cargo build".to_string()]);
    assert!(options.gates.private_verify.is_empty());
    assert_eq!(
        options.gates.private_gate_reveal,
        AgentTaskGateRevealPolicy::FullEvidence
    );
}

#[test]
fn provider_response_overflow_is_terminated_with_bounded_evidence() {
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
                "yes response | head -c 1048577".to_string(),
            ],
            ..Default::default()
        },
        &request,
    )
    .expect_err("oversized provider response rejected");

    assert!(error.message.contains("response exceeded"));
    assert_eq!(
        error.details["command_evidence"]["stdout"]
            .as_str()
            .expect("bounded stdout")
            .len(),
        65_536
    );
    assert_eq!(error.details["command_evidence"]["truncated"], true);
}

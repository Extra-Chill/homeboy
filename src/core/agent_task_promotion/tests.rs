#![cfg(test)]

use std::path::PathBuf;

use serde_json::{json, Value};

use super::apply::{
    run_provider_command, AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
    AgentTaskPromotionWorkspaceProvider, AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA,
    AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
};
use super::promote::{
    normalize_promotion_patch, promote, promote_with_provider, select_patch_artifact,
    validate_artifact_content,
};
use super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandCapture,
    AgentTaskPromotionCommandReport, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};
use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_gate::{
    AgentTaskGateEnvironment, AgentTaskGateReport, AgentTaskGateRevealPolicy,
    AgentTaskGateVisibility, VerifyGateOptions,
};
use crate::core::command_invocation::CommandInvocation;
use crate::core::{Error, Result};

const VALID_PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";

#[derive(Debug, Default)]
struct FakePromotionWorkspaceProvider {
    workspace_path: Option<PathBuf>,
    apply_calls: Vec<AgentTaskPromotionApplyRequest>,
    applied_patch_contents: Vec<String>,
    verify_calls: Vec<(
        PathBuf,
        String,
        AgentTaskGateVisibility,
        AgentTaskGateRevealPolicy,
    )>,
}

impl AgentTaskPromotionWorkspaceProvider for FakePromotionWorkspaceProvider {
    fn apply_patch(
        &mut self,
        request: AgentTaskPromotionApplyRequest,
    ) -> Result<AgentTaskPromotionWorkspace> {
        self.applied_patch_contents
            .push(std::fs::read_to_string(&request.patch_path).unwrap_or_else(|_| String::new()));
        self.apply_calls.push(request.clone());
        let path = self.workspace_path.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "to_worktree",
                "fake workspace provider could not resolve the requested workspace",
                None,
                None,
            )
        })?;
        Ok(AgentTaskPromotionWorkspace {
            path,
            command_evidence: vec![command_report(vec![
                "fake-workspace-provider",
                "apply-patch",
                request.to_workspace.as_str(),
            ])],
        })
    }

    fn verify(
        &mut self,
        cwd: &Path,
        index: usize,
        command: &str,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
    ) -> Result<AgentTaskGateReport> {
        self.verify_calls.push((
            cwd.to_path_buf(),
            command.to_string(),
            visibility,
            reveal_policy,
        ));
        Ok(AgentTaskGateReport::new(
            format!("gate-{index}"),
            vec!["sh".to_string(), "-lc".to_string(), command.to_string()],
            0,
            String::new(),
            String::new(),
            None,
            visibility,
            reveal_policy,
            crate::core::agent_task_gate::AgentTaskGateEnvironment::default(),
        ))
    }
}

fn command_report(parts: Vec<&str>) -> AgentTaskPromotionCommandReport {
    AgentTaskPromotionCommandReport {
        command: parts.into_iter().map(str::to_string).collect(),
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        capture: AgentTaskPromotionCommandCapture::default(),
    }
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn write_patch_source(temp: &tempfile::TempDir) -> (PathBuf, String) {
    let patch_path = temp.path().join("changes.patch");
    std::fs::write(&patch_path, VALID_PATCH).expect("write patch");
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
            "size_bytes": VALID_PATCH.len(),
            "sha256": sha256_hex(VALID_PATCH)
        }]
    })
    .to_string();
    (source_path, source)
}

#[test]
fn validate_patch_extracts_safe_changed_files() {
    let patch = normalize_promotion_patch(VALID_PATCH, "repo@promoted-task").expect("valid patch");

    assert_eq!(patch.changed_files, vec!["src/lib.rs"]);
    assert_eq!(patch.content, VALID_PATCH);
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
fn normalize_promotion_patch_leaves_unrelated_workspace_paths() {
    let patch = "diff --git a/workspace/fixture.txt b/workspace/fixture.txt\n--- a/workspace/fixture.txt\n+++ b/workspace/fixture.txt\n@@ -1 +1 @@\n-old\n+new\n";

    let normalized = normalize_promotion_patch(patch, "homeboy@promoted-task")
        .expect("unrelated workspace path remains repo-relative");

    assert_eq!(normalized.changed_files, vec!["workspace/fixture.txt"]);
    assert_eq!(normalized.content, patch);
}

#[test]
fn validate_patch_rejects_empty_patch() {
    let err =
        normalize_promotion_patch("\n\t", "repo@promoted-task").expect_err("empty patch rejected");

    assert!(err.message.contains("empty patch"));
}

#[test]
fn select_patch_artifact_rejects_empty_patch_metadata() {
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

    let err = select_patch_artifact(&outcome, None).expect_err("empty patch rejected");

    assert!(err.message.contains("non-empty patch artifact"));
    assert!(err.message.contains("agent result or transcript"));
}

#[test]
fn validate_patch_rejects_path_traversal() {
    let patch = "--- a/src/lib.rs\n+++ b/../secret\n@@ -1 +1 @@\n-old\n+new\n";

    let err =
        normalize_promotion_patch(patch, "repo@promoted-task").expect_err("unsafe path rejected");

    assert!(err.message.contains("unsafe patch path"));
}

#[test]
fn normalize_promotion_patch_rejects_repo_sandbox_without_relative_suffix() {
    let patch = "diff --git a/workspace/homeboy-refactor b/workspace/homeboy-refactor\n--- a/workspace/homeboy-refactor\n+++ b/workspace/homeboy-refactor\n@@ -1 +1 @@\n-old\n+new\n";

    let err = normalize_promotion_patch(patch, "homeboy@promoted-task")
        .expect_err("repo sandbox path without suffix rejected");

    assert!(err.message.contains("no repo-relative suffix"));
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
fn promote_dry_run_reports_selected_patch_without_provider_mutation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_path, source) = write_patch_source(&temp);

    let report = promote(AgentTaskPromotionOptions {
        source,
        source_run_id: None,
        source_path: Some(source_path),
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
    })
    .expect("dry-run promotion report");

    assert_eq!(report.status, AgentTaskPromotionStatus::DryRun);
    assert_eq!(report.source.task_id, "task-1");
    assert_eq!(report.patch_artifact.id, "patch");
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert!(report.command_evidence.is_empty());
    assert!(report.deterministic_gates.is_empty());
}

#[test]
fn promote_applies_patch_with_fake_workspace_provider() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree_path = temp.path().join("controlled-worktree");
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(worktree_path.clone()),
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run-1".to_string()),
            source_path: Some(source_path),
            to_worktree: "repo@controlled-worktree".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions {
                verify: vec!["cargo test --lib agent_task_promotion".to_string()],
                private_verify: vec!["cargo test --lib hidden".to_string()],
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
            },
            provider_command: None,
        },
        &mut provider,
    )
    .expect("applied promotion report");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(report.changed_files, vec!["src/lib.rs"]);
    assert_eq!(
        report.provenance["worktree_path"].as_str(),
        Some(worktree_path.to_str().expect("utf-8 temp path"))
    );
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(
        provider.apply_calls[0].to_workspace,
        "repo@controlled-worktree"
    );
    assert!(provider.apply_calls[0]
        .patch_path
        .ends_with("changes.patch"));
    assert_eq!(provider.apply_calls[0].changed_files, vec!["src/lib.rs"]);
    assert_eq!(
        provider.verify_calls,
        vec![
            (
                worktree_path.clone(),
                "cargo test --lib agent_task_promotion".to_string(),
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
            ),
            (
                worktree_path,
                "cargo test --lib hidden".to_string(),
                AgentTaskGateVisibility::Private,
                AgentTaskGateRevealPolicy::SummaryOnly,
            )
        ]
    );
    assert_eq!(report.command_evidence.len(), 1);
    assert_eq!(
        report.command_evidence[0].command[0],
        "fake-workspace-provider"
    );
    assert_eq!(report.deterministic_gates.len(), 2);
    assert_eq!(report.deterministic_gates[0].id, "gate-1");
    assert_eq!(
        report.deterministic_gates[1].visibility,
        AgentTaskGateVisibility::Private
    );
}

#[test]
fn promote_materializes_worktree_dependencies_before_verify_gate() {
    // #3771: a freshly created worktree has no installed dependencies, so a
    // verify gate that touches autoloaded deps fatals on missing deps
    // instead of reporting a real pass/fail. Promotion must run the
    // component's dependency install step against the worktree before the
    // verify gate executes. This uses a runtime-agnostic component `deps`
    // script (no composer/npm binary required) to prove the install ran.
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let (source_path, source) = write_patch_source(&temp);

        let worktree_path = temp.path().join("worktree");
        std::fs::create_dir_all(&worktree_path).expect("worktree dir");
        let deps_marker = worktree_path.join("deps-installed.txt");
        std::fs::write(
            worktree_path.join("homeboy.json"),
            serde_json::json!({
                "id": "deps-worktree",
                "scripts": {
                    "deps": ["sh -c 'printf installed > deps-installed.txt'"]
                }
            })
            .to_string(),
        )
        .expect("worktree manifest");

        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(worktree_path.clone()),
            ..Default::default()
        };

        let report = promote_with_provider(
            AgentTaskPromotionOptions {
                source,
                source_run_id: Some("run-3771".to_string()),
                source_path: Some(source_path),
                to_worktree: "repo@worktree".to_string(),
                task_id: None,
                artifact_id: None,
                dry_run: false,
                gates: VerifyGateOptions {
                    verify: vec!["true".to_string()],
                    private_verify: Vec::new(),
                    private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                },
                provider_command: None,
            },
            &mut provider,
        )
        .expect("promotion materializes deps then verifies");

        assert!(
            deps_marker.exists(),
            "dependency install step should run against the worktree before the verify gate"
        );
        assert_eq!(
            report.provenance["dependencies_materialized"].as_bool(),
            Some(true),
            "promotion report should record that dependencies were materialized"
        );
        assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
        assert_eq!(provider.verify_calls.len(), 1);
    });
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
fn promote_requires_provider_for_apply() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_path, source) = write_patch_source(&temp);

    let err = promote(AgentTaskPromotionOptions {
        source,
        source_run_id: None,
        source_path: Some(source_path),
        to_worktree: "repo@controlled-worktree".to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions {
            verify: Vec::new(),
            private_verify: Vec::new(),
            private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
        },
        provider_command: None,
    })
    .expect_err("missing provider rejected");

    assert!(err.message.contains("workspace provider command"));
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

#[test]
fn provider_command_response_supplies_workspace_and_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let response_path = temp.path().join("response.json");
    std::fs::write(
        &response_path,
        serde_json::json!({
            "schema": AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
            "workspace_path": temp.path().join("workspace").display().to_string(),
            "command_evidence": [{
                "command": ["provider", "apply"],
                "exit_code": 0
            }]
        })
        .to_string(),
    )
    .expect("write response");

    let request = AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: "target-workspace".to_string(),
        patch_path: temp.path().join("changes.patch").display().to_string(),
        changed_files: vec!["src/lib.rs".to_string()],
    };
    let workspace = run_provider_command(
        &CommandInvocation {
            argv: vec!["cat".to_string(), response_path.display().to_string()],
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
}

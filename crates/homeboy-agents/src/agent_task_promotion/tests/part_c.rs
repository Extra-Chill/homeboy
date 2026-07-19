//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::super::apply::{
    preflight_configured_workspace_provider_with_config, run_provider_command,
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
    AgentTaskPromotionWorkspaceProvider, ExternalPromotionWorkspaceProvider,
    TrustedUnpushedCandidateDestination, AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA,
    AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
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
            .expect("bridge preserves aggregate while surfacing pending projection");

            let record = crate::agent_task_lifecycle::status(run_id).expect("lifecycle status");
            assert_eq!(record.metadata["artifact_projection"]["status"], "pending");
            assert!(record.metadata["artifact_projection"]["error"]
                .as_str()
                .expect("actionable error")
                .contains("controller-finalized bytes"));
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
fn promote_recoverable_candidate_rejects_zero_actionable_patches() {
    let (result, apply_calls) = promote_recoverable_patch_count(0);
    assert!(result
        .expect_err("missing candidate rejected")
        .message
        .contains("exactly one actionable patch"));
    assert_eq!(apply_calls, 0);
}

#[test]
fn configured_command_provider_is_resolved_lazily_with_provenance() {
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
            apply_enabled: true,
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

    let provider = ExternalPromotionWorkspaceProvider::from_options_with_config_and_environment(
        &promotion_options("fixture@cook-target"),
        &config,
        Some(PathBuf::from("/fixture/homeboy")),
        None,
    );
    let mut provider = provider;
    assert!(provider.invocation().is_none());
    assert!(provider.provenance().is_none());

    let error = provider
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
        .expect_err("fixture executable is not an adapter");

    assert_eq!(
        provider.invocation().expect("invocation").argv,
        vec![
            "/fixture/homeboy".to_string(),
            "agent-task".to_string(),
            "promotion-provider".to_string(),
            "--workspace".to_string(),
            workspace.path().display().to_string(),
        ]
    );
    assert_eq!(provider.provenance().expect("provenance")["id"], "fixture");
    assert_eq!(error.details["worktree_provider"]["id"], "fixture");
    assert_eq!(
        error.details["worktree_provider"]["handle"],
        "fixture@cook-target"
    );
    assert_eq!(
        error.details["worktree_provider"]["path"],
        workspace.path().display().to_string()
    );
}

#[test]
fn configured_provider_accepts_only_the_unpushed_immutable_candidate_destination() {
    let (_temp, repo, _base, candidate) = adopted_commit_repo();
    let provider = tempfile::NamedTempFile::new().expect("provider command");
    std::fs::write(
        provider.path(),
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\n",
            serde_json::json!({
                "worktrees": [{
                    "handle": "fixture@candidate",
                    "path": repo,
                    "branch": "main",
                    "safety": { "dirty": false, "unpushed": true, "primary": false }
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
    let mut config = HomeboyConfig::default();
    config.worktree_providers.insert(
        "fixture".to_string(),
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
    let mut provider = ExternalPromotionWorkspaceProvider::from_options_with_config_and_environment(
        &promotion_options("fixture@candidate"),
        &config,
        Some(PathBuf::from("/fixture/homeboy")),
        None,
    );
    let error = provider
        .apply_patch(AgentTaskPromotionApplyRequest {
            schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
            to_workspace: "fixture@candidate".to_string(),
            patch: Some(VALID_PATCH.to_string()),
            patch_path: "changes.patch".to_string(),
            changed_files: vec!["lib.rs".to_string()],
            gate_feedback_baseline: None,
            dry_run: false,
            trusted_unpushed_candidate_destination: Some(TrustedUnpushedCandidateDestination {
                path: repo.clone(),
                head: candidate,
            }),
        })
        .expect_err("fixture executable is not an adapter");
    assert!(!error.message.contains("unpushed"), "{}", error.message);
    assert_eq!(
        provider
            .invocation()
            .expect("resolved provider")
            .argv
            .last(),
        Some(&repo.display().to_string())
    );
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
            },
            provider_command: None,
            provider_invocation: None,
        })
        .expect("empty patch reports no changes");

        assert_eq!(report.status, AgentTaskPromotionStatus::NoChanges);
        assert!(report.changed_files.is_empty());
        match prior_promotion_command {
            Some(command) => std::env::set_var("HOMEBOY_AGENT_TASK_PROMOTION_COMMAND", command),
            None => std::env::remove_var("HOMEBOY_AGENT_TASK_PROMOTION_COMMAND"),
        }
    });
}

#[test]
fn promote_no_op_outcome_without_committed_candidate_rejects_before_apply() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "agent@example.test"]);
    git(&repo, &["config", "user.name", "Agent"]);
    std::fs::write(repo.join("lib.rs"), "base\n").expect("write base");
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-m", "base"]);
    let base = git_head(&repo, "HEAD");

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

    let error = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: Some(repo),
            base_ref: None,
            task_base_sha: Some(base),
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
    .expect_err("no-op without an audited candidate is rejected");

    assert!(error
        .message
        .contains("no-op promotion requires an audited committed candidate"));
    assert!(provider.apply_calls.is_empty());
    assert!(provider.verify_calls.is_empty());
}

#[test]
fn committed_change_promotion_rejects_a_non_ancestor_task_base() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "agent@example.test"]);
    git(&repo, &["config", "user.name", "Agent"]);
    git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("file.txt"), "base\n").expect("base");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "base"]);
    git(&repo, &["checkout", "-b", "unrelated"]);
    std::fs::write(repo.join("file.txt"), "unrelated\n").expect("unrelated");
    git(&repo, &["commit", "-am", "unrelated"]);
    let unrelated = git_head(&repo, "HEAD");
    git(&repo, &["checkout", "main"]);
    std::fs::write(repo.join("file.txt"), "agent\n").expect("agent");
    git(&repo, &["commit", "-am", "agent"]);
    let (source_path, source) = write_empty_patch_source(&temp);

    let error = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: None,
            source_path: Some(source_path),
            source_worktree_path: Some(repo),
            base_ref: None,
            task_base_sha: Some(unrelated),
            candidate_ref: None,
            to_worktree: "repo@promoted".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
        &mut FakePromotionWorkspaceProvider::default(),
    )
    .expect_err("unrelated base rejected");
    assert!(error.message.contains("not an ancestor"));
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
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
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
            provider_invocation: None,
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
fn promote_persists_force_added_ignored_git_candidate_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worktree_path = temp.path().join("controlled-worktree");
    std::fs::create_dir(&worktree_path).expect("worktree");
    git(&worktree_path, &["init"]);
    git(
        &worktree_path,
        &["config", "user.email", "test@example.com"],
    );
    git(&worktree_path, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(worktree_path.join("src")).expect("src");
    std::fs::write(worktree_path.join("src/lib.rs"), "old\n").expect("base source");
    git(&worktree_path, &["add", "."]);
    git(&worktree_path, &["commit", "-m", "base"]);
    let (source_path, source) = write_patch_source(&temp);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(worktree_path),
        force_add_ignored_file: true,
        ..Default::default()
    };

    let report = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("run-8935".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            candidate_ref: None,
            to_worktree: "repo@controlled-worktree".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    )
    .expect("promotion accepts the force-added ignored candidate file");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(
        report.changed_files,
        vec![
            "ignored/nested/force-added.rs".to_string(),
            "src/lib.rs".to_string(),
        ]
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
    homeboy_core::test_support::with_isolated_home(|_| {
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
                source_worktree_path: None,
                base_ref: None,
                task_base_sha: None,
                candidate_ref: None,
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
                provider_invocation: None,
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
fn explicit_candidate_adoption_ignores_recoverable_provider_patch_selection() {
    let (temp, repo, base, candidate) = adopted_commit_repo();
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };

    let mut options = adopted_commit_options(
        &temp,
        &repo,
        base,
        candidate.clone(),
        VerifyGateOptions {
            verify: vec!["cargo test --lib".to_string()],
            ..Default::default()
        },
    );
    let mut source: Value = serde_json::from_str(&options.source).expect("adoption outcome");
    source["status"] = Value::String("candidate_recoverable".to_string());
    options.source = source.to_string();

    let report = promote_with_provider(options, &mut provider)
        .expect("recoverable candidate adoption promotes committed changes");

    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(
        report.provenance["commit_range"],
        format!(
            "{}..{}",
            report.provenance["base_ref"].as_str().unwrap(),
            candidate
        )
    );
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(provider.verify_calls.len(), 1);
    assert_eq!(
        report.deterministic_gates[0].status,
        crate::agent_task_gate::AgentTaskGateStatus::Succeeded
    );
}

#[test]
fn explicit_candidate_adopts_only_durable_pre_provider_transport_failures() {
    let (temp, repo, base, candidate) = adopted_commit_repo();
    let failed_source = |recovery: Value| {
        serde_json::json!({
            "schema": AGENT_TASK_OUTCOME_SCHEMA,
            "task_id": "adoption-task",
            "status": "failed",
            "artifacts": [],
            "metadata": { "candidate_adoption_recovery": recovery }
        })
        .to_string()
    };
    let eligible = serde_json::json!({
        "schema": "homeboy/agent-task-candidate-adoption-recovery/v1",
        "reason": "pre_provider_transport_failure",
        "provider_executions_consumed": 0
    });
    let mut options = adopted_commit_options(
        &temp,
        &repo,
        base.clone(),
        candidate.clone(),
        VerifyGateOptions {
            verify: vec!["cargo test --lib".to_string()],
            ..Default::default()
        },
    );
    options.source = failed_source(eligible);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(repo.clone()),
        ..Default::default()
    };
    let report = promote_with_provider(options.clone(), &mut provider)
        .expect("eligible transport failure adopts immutable candidate through gates");
    assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
    assert_eq!(provider.apply_calls.len(), 1);
    assert_eq!(provider.verify_calls.len(), 1);

    for recovery in [
        Value::Null,
        serde_json::json!({ "reason": "provider_failure" }),
    ] {
        options.source = failed_source(recovery);
        let error = promote_with_provider(options.clone(), &mut provider)
            .expect_err("legacy and provider failures fail closed");
        assert!(
            error
                .message
                .contains("explicit durable pre-provider transport recovery eligibility"),
            "{}",
            error.message
        );
    }
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

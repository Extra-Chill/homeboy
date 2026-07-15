#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::apply::{
    run_provider_command, AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
    AgentTaskPromotionWorkspaceProvider, ExternalPromotionWorkspaceProvider,
    AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA, AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
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
    AgentTaskGateReport, AgentTaskGateRevealPolicy, AgentTaskGateVisibility, VerifyGateOptions,
};
use crate::core::command_invocation::CommandInvocation;
use crate::core::defaults::{
    HomeboyConfig, WorktreeProviderCommands, WorktreeProviderConfig, WorktreeProviderKind,
    WorktreeProviderListResultMapping,
};
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
    verify_exit_code: i32,
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
            self.verify_exit_code,
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

fn write_empty_patch_source(temp: &tempfile::TempDir) -> (PathBuf, String) {
    let patch_path = temp.path().join("empty.patch");
    std::fs::write(&patch_path, "").expect("write empty patch");
    let source_path = temp.path().join("outcome.json");
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-1",
        "status": "succeeded",
        "artifacts": [{
            "schema": AGENT_TASK_ARTIFACT_SCHEMA,
            "id": "patch",
            "kind": "patch",
            "path": "empty.patch",
            "size_bytes": 0,
            "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        }]
    })
    .to_string();
    std::fs::write(&source_path, &source).expect("write source");
    (source_path, source)
}

fn recoverable_patch_source(temp: &tempfile::TempDir, patch_count: usize) -> (PathBuf, String) {
    let artifacts = (0..patch_count)
        .map(|index| {
            let name = format!("candidate-{index}.patch");
            std::fs::write(temp.path().join(&name), VALID_PATCH).expect("write candidate patch");
            serde_json::json!({
                "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                "id": format!("candidate-{index}"),
                "kind": "patch",
                "path": name,
                "size_bytes": VALID_PATCH.len(),
                "sha256": sha256_hex(VALID_PATCH),
                "metadata": { "role": "patch" }
            })
        })
        .collect::<Vec<_>>();
    let source_path = temp.path().join("recoverable-outcome.json");
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "task-1",
        "status": "candidate_recoverable",
        "artifacts": artifacts
    })
    .to_string();
    std::fs::write(&source_path, &source).expect("write recoverable source");
    (source_path, source)
}

fn promote_recoverable_patch_count(
    patch_count: usize,
) -> (Result<AgentTaskPromotionReport>, usize) {
    let temp = tempfile::tempdir().expect("tempdir");
    let (source_path, source) = recoverable_patch_source(&temp, patch_count);
    let mut provider = FakePromotionWorkspaceProvider {
        workspace_path: Some(temp.path().join("target")),
        ..Default::default()
    };
    let result = promote_with_provider(
        AgentTaskPromotionOptions {
            source,
            source_run_id: Some("recoverable-run".to_string()),
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
            to_worktree: "repo@recoverable".to_string(),
            task_id: None,
            artifact_id: None,
            dry_run: false,
            gates: VerifyGateOptions::default(),
            provider_command: None,
            provider_invocation: None,
        },
        &mut provider,
    );
    (result, provider.apply_calls.len())
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
fn promote_recoverable_candidate_applies_exactly_one_actionable_patch() {
    let (result, apply_calls) = promote_recoverable_patch_count(1);
    assert_eq!(
        result.expect("single candidate applies").status,
        AgentTaskPromotionStatus::Applied
    );
    assert_eq!(apply_calls, 1);
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

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn promotion_options(to_worktree: &str) -> AgentTaskPromotionOptions {
    AgentTaskPromotionOptions {
        source: "{}".to_string(),
        source_run_id: None,
        source_path: None,
        source_worktree_path: None,
        base_ref: None,
        task_base_sha: None,
        to_worktree: to_worktree.to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions::default(),
        provider_command: None,
        provider_invocation: None,
    }
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
            dry_run: false,
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

    crate::core::worktree_providers::resolve_worktree_provider_from_config(
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
            dry_run: false,
        })
        .expect_err("lookup-only provider must not authorize promotion");

    assert!(error
        .message
        .contains("not apply-enabled provider(s): fixture"));
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

    let response = super::apply_materialized_workspace_patch(
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
fn promote_reports_no_changes_for_empty_patch_metadata() {
    crate::test_support::with_isolated_home(|_| {
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

    assert_eq!(
        report.patch_artifact.id,
        "task-1-attempt-1-committed-changes"
    );
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
                source_worktree_path: None,
                base_ref: None,
                task_base_sha: None,
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
fn promote_rejects_unresolved_configured_provider_for_apply() {
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let (source_path, source) = write_patch_source(&temp);

        let err = promote(AgentTaskPromotionOptions {
            source,
            source_run_id: None,
            source_path: Some(source_path),
            source_worktree_path: None,
            base_ref: None,
            task_base_sha: None,
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
            provider_invocation: None,
        })
        .expect_err("unresolved configured provider rejected");

        assert!(err.message.contains("configured worktree provider"));
    });
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

fn git_head(cwd: &Path, reference: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", reference])
        .current_dir(cwd)
        .output()
        .expect("resolve git ref");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
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
        dry_run: false,
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

#[test]
fn provider_failure_surfaces_bounded_stdout_and_stderr_evidence() {
    let request = AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: "target-workspace".to_string(),
        patch: None,
        patch_path: "changes.patch".to_string(),
        changed_files: vec!["src/lib.rs".to_string()],
        dry_run: false,
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
fn provider_response_overflow_is_terminated_with_bounded_evidence() {
    let request = AgentTaskPromotionApplyRequest {
        schema: AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA.to_string(),
        to_workspace: "target-workspace".to_string(),
        patch: None,
        patch_path: "changes.patch".to_string(),
        changed_files: vec!["src/lib.rs".to_string()],
        dry_run: false,
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

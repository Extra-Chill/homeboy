#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::apply::{
    preflight_configured_workspace_provider_with_config, run_provider_command,
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
    AgentTaskPromotionWorkspaceProvider, ExternalPromotionWorkspaceProvider,
    AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA, AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
};

use super::promote::{
    normalize_promotion_patch, promote, promote_with_provider,
    promote_with_provider_and_checkpoint, resume_promoted_patch, select_patch_artifact,
    validate_artifact_content,
};
use super::types::{
    AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandCapture,
    AgentTaskPromotionCommandReport, AgentTaskPromotionNotification, AgentTaskPromotionOptions,
    AgentTaskPromotionReport, AgentTaskPromotionSource, AgentTaskPromotionStatus,
    AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};
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

pub(super) const VALID_PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";

#[derive(Debug, Default)]
pub(super) struct FakePromotionWorkspaceProvider {
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
    verify_transport_error: bool,
    force_add_ignored_file: bool,
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
        if self.force_add_ignored_file {
            git(&path, &["apply", &request.patch_path]);
            std::fs::write(path.join(".git/info/exclude"), "ignored/\n")
                .expect("ignore nested candidate file");
            let ignored = path.join("ignored/nested/force-added.rs");
            std::fs::create_dir_all(ignored.parent().expect("ignored parent"))
                .expect("create ignored nested directory");
            std::fs::write(&ignored, "pub const FORCED: bool = true;\n")
                .expect("write ignored nested candidate file");
            git(&path, &["add", "-f", "ignored/nested/force-added.rs"]);
        }
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
        if self.verify_transport_error {
            return Err(Error::internal_io(
                "simulated verification transport interruption",
                Some("promotion gate transport".to_string()),
            ));
        }
        Ok(AgentTaskGateReport::new(
            format!("gate-{index}"),
            vec!["sh".to_string(), "-lc".to_string(), command.to_string()],
            self.verify_exit_code,
            String::new(),
            String::new(),
            None,
            visibility,
            reveal_policy,
            crate::agent_task_gate::AgentTaskGateEnvironment::default(),
        ))
    }
}

pub(super) fn command_report(parts: Vec<&str>) -> AgentTaskPromotionCommandReport {
    AgentTaskPromotionCommandReport {
        command: parts.into_iter().map(str::to_string).collect(),
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        capture: AgentTaskPromotionCommandCapture::default(),
    }
}

pub(super) fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn record_controller_projection(
    run_id: &str,
    task_id: &str,
    artifact_id: &str,
    contents: &str,
) -> PathBuf {
    let store =
        homeboy_core::observation::ObservationStore::open_initialized().expect("observation store");
    store
        .upsert_imported_run(&homeboy_core::observation::RunRecord {
            id: run_id.to_string(),
            kind: "agent-task".to_string(),
            component_id: None,
            started_at: "2026-07-16T00:00:00Z".to_string(),
            finished_at: Some("2026-07-16T00:00:01Z".to_string()),
            status: "pass".to_string(),
            command: Some("homeboy agent-task".to_string()),
            cwd: None,
            homeboy_version: Some("test".to_string()),
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({}),
        })
        .expect("record run");
    let input = tempfile::NamedTempFile::new().expect("projection input");
    std::fs::write(input.path(), contents).expect("write projection input");
    PathBuf::from(
        store
            .record_artifact_with_id(
                run_id,
                "patch",
                input.path(),
                "controller-finalized-patch",
                serde_json::json!({
                    "agent_task": {
                        "task_id": task_id,
                        "logical_artifact_id": artifact_id,
                    }
                }),
            )
            .expect("record controller projection")
            .path,
    )
}

pub(super) fn recovered_runner_aggregate(
    task_id: &str,
    artifact_id: &str,
    sha256: &str,
    size: usize,
) -> String {
    serde_json::json!({
        "schema": "homeboy/agent-task-aggregate/v1",
        "plan_id": "recovered-lab-plan",
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
                "size_bytes": size,
                "sha256": sha256,
                "metadata": { "executor_artifact_finalized": true }
            }]
        }]
    })
    .to_string()
}

pub(super) fn write_patch_source(temp: &tempfile::TempDir) -> (PathBuf, String) {
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

pub(super) fn write_empty_patch_source(temp: &tempfile::TempDir) -> (PathBuf, String) {
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

pub(super) fn recoverable_patch_source(
    temp: &tempfile::TempDir,
    patch_count: usize,
) -> (PathBuf, String) {
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
                "metadata": {
                    "role": "patch",
                    "run_id": "recoverable-run",
                    "task_id": "task-1",
                    "producer_attempt": 1,
                    "base_ref": "base-fingerprint",
                    "provider_backend": "provider",
                    "repository_identity": "repository-identity",
                    "workspace_identity": "workspace-identity"
                }
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

pub(super) fn promote_recoverable_patch_count(
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
    );
    (result, provider.apply_calls.len())
}

pub(super) fn git(cwd: &Path, args: &[&str]) {
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

pub(super) fn promotion_options(to_worktree: &str) -> AgentTaskPromotionOptions {
    AgentTaskPromotionOptions {
        source: "{}".to_string(),
        source_run_id: None,
        source_path: None,
        source_worktree_path: None,
        base_ref: None,
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: to_worktree.to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: VerifyGateOptions::default(),
        provider_command: None,
        provider_invocation: None,
    }
}

pub(super) fn adopted_commit_options(
    temp: &tempfile::TempDir,
    repo: &Path,
    base: String,
    candidate_ref: String,
    gates: VerifyGateOptions,
) -> AgentTaskPromotionOptions {
    let source_path = temp.path().join("adoption-outcome.json");
    let source = serde_json::json!({
        "schema": AGENT_TASK_OUTCOME_SCHEMA,
        "task_id": "adoption-task",
        "status": "succeeded",
        "artifacts": []
    })
    .to_string();
    std::fs::write(&source_path, &source).expect("write adoption outcome");
    AgentTaskPromotionOptions {
        source,
        source_run_id: Some("adoption-run".to_string()),
        source_path: Some(source_path),
        source_worktree_path: Some(repo.to_path_buf()),
        // The immutable task base is the candidate contract; this fixture does
        // not configure a remote branch snapshot for finalization.
        base_ref: None,
        task_base_sha: Some(base),
        candidate_ref: Some(candidate_ref),
        to_worktree: "repo@adopted".to_string(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates,
        provider_command: None,
        provider_invocation: None,
    }
}

pub(super) fn adopted_commit_repo() -> (tempfile::TempDir, PathBuf, String, String) {
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
    git(&repo, &["commit", "-am", "candidate"]);
    let candidate = git_head(&repo, "HEAD");
    (temp, repo, base, candidate)
}

pub(super) fn git_head(cwd: &Path, reference: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", reference])
        .current_dir(cwd)
        .output()
        .expect("resolve git ref");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

mod part_a;
mod part_b;
mod part_c;
mod part_d;

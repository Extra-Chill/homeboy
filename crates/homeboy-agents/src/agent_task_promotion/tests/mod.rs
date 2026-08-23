#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::apply::{
    AgentTaskPromotionApplyRequest, AgentTaskPromotionWorkspace,
    AgentTaskPromotionWorkspaceProvider, AGENT_TASK_PROMOTION_APPLY_REQUEST_SCHEMA,
    AGENT_TASK_PROMOTION_APPLY_RESPONSE_SCHEMA,
};

use super::types::{
    AgentTaskPromotionCommandCapture, AgentTaskPromotionCommandReport, AgentTaskPromotionOptions,
    AgentTaskPromotionSource, AgentTaskPromotionStatus, AgentTaskPromotionTarget,
    AGENT_TASK_PROMOTION_REPORT_SCHEMA,
};
use crate::agent_task::{AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA};
use crate::agent_task_gate::{
    AgentTaskGateReport, AgentTaskGateRevealPolicy, AgentTaskGateVisibility, VerifyGateOptions,
};
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
    run_verify_command: bool,
    verify_worktrees_clean: Vec<bool>,
    force_add_ignored_file: bool,
    apply_to_git: bool,
    replace_source_on_apply: Option<(PathBuf, String)>,
}

impl AgentTaskPromotionWorkspaceProvider for FakePromotionWorkspaceProvider {
    fn apply_patch(
        &mut self,
        request: AgentTaskPromotionApplyRequest,
    ) -> Result<AgentTaskPromotionWorkspace> {
        if let Some((path, contents)) = self.replace_source_on_apply.take() {
            std::fs::write(path, contents).expect("replace source during provider handoff");
        }
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
        if self.apply_to_git {
            git(&path, &["apply", &request.patch_path]);
        }
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
        let status = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(cwd)
            .output();
        self.verify_worktrees_clean
            .push(status.is_ok_and(|status| status.status.success() && status.stdout.is_empty()));
        if self.run_verify_command {
            return crate::agent_task_gate::run_gate_command_with_policy(
                cwd,
                index,
                command,
                visibility,
                reveal_policy,
            );
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

    fn verify_with_runtime_tmpdir(
        &mut self,
        cwd: &Path,
        index: usize,
        command: &str,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
        runtime_tmpdir: &Path,
        gate_environment: &crate::agent_task_gate::AgentTaskGateEnvironmentPolicy,
        package_artifacts: &[crate::agent_task_gate::AgentTaskGatePackageArtifactRequirement],
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
        let status = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(cwd)
            .output();
        self.verify_worktrees_clean
            .push(status.is_ok_and(|status| status.status.success() && status.stdout.is_empty()));
        if self.run_verify_command {
            return crate::agent_task_gate::run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
                cwd,
                index,
                command,
                visibility,
                reveal_policy,
                Some(runtime_tmpdir),
                gate_environment,
                package_artifacts,
            );
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

pub(super) fn record_controller_projection_in_store(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact_id: &str,
    contents: &str,
) -> PathBuf {
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

mod part_a;
mod part_b;
mod part_c;
mod part_d;

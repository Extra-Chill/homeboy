use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task_gate::{AgentTaskGateReport, VerifyGateOptions};
use crate::core::gate::HomeboyGateResult;
use crate::core::stream_capture::StreamCaptureMetadata;

pub const AGENT_TASK_PROMOTION_REPORT_SCHEMA: &str = "homeboy/agent-task-promotion-report/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionOptions {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    pub source_path: Option<PathBuf>,
    pub to_worktree: String,
    pub task_id: Option<String>,
    pub artifact_id: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    /// Deterministic verification gates. Flattened so the serialized shape keeps
    /// the historical flat `verify` / `private_verify` / `private_gate_reveal`
    /// keys while the field group is defined once in `VerifyGateOptions`.
    #[serde(flatten)]
    pub gates: VerifyGateOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPromotionReport {
    #[serde(default = "promotion_report_schema")]
    pub schema: String,
    pub status: AgentTaskPromotionStatus,
    pub source: AgentTaskPromotionSource,
    pub to_worktree: String,
    pub target: AgentTaskPromotionTarget,
    pub patch_artifact: AgentTaskPromotionArtifactRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_evidence: Vec<AgentTaskPromotionCommandReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deterministic_gates: Vec<AgentTaskGateReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<HomeboyGateResult>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provenance: Value,
    pub operator_notification: AgentTaskPromotionNotification,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskPromotionStatus {
    DryRun,
    Applied,
    GateFailed,
    NoChanges,
}

impl AgentTaskPromotionStatus {
    /// Whether a patch was actually promoted into the target worktree for this
    /// status (true for both clean applies and gate-failed applies).
    pub fn patch_promoted(self) -> bool {
        matches!(self, Self::Applied | Self::GateFailed)
    }

    /// Whether deterministic gates failed after the patch was promoted.
    pub fn gate_failed(self) -> bool {
        matches!(self, Self::GateFailed)
    }

    /// Stable handoff boundary identifier for this promotion status.
    pub fn handoff_boundary(self) -> &'static str {
        match self {
            Self::Applied => "patch_promoted_no_pr",
            Self::GateFailed => "patch_promoted_gates_failed",
            Self::DryRun => "patch_not_promoted_dry_run",
            Self::NoChanges => "patch_not_promoted_no_changes",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionSource {
    pub kind: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionTarget {
    pub worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionArtifactRef {
    pub id: String,
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionCommandReport {
    pub command: Vec<String>,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    /// Per-stream truncation metadata describing the retained-byte bound applied
    /// to `stdout`/`stderr`. Skipped when both streams fit within the cap so the
    /// historical serialized shape is preserved (#5077).
    #[serde(
        default,
        skip_serializing_if = "AgentTaskPromotionCommandCapture::is_empty"
    )]
    pub capture: AgentTaskPromotionCommandCapture,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionCommandCapture {
    #[serde(default, skip_serializing_if = "is_untruncated_stream")]
    pub stdout: StreamCaptureMetadata,
    #[serde(default, skip_serializing_if = "is_untruncated_stream")]
    pub stderr: StreamCaptureMetadata,
}

impl AgentTaskPromotionCommandCapture {
    fn is_empty(&self) -> bool {
        is_untruncated_stream(&self.stdout) && is_untruncated_stream(&self.stderr)
    }
}

fn is_untruncated_stream(stream: &StreamCaptureMetadata) -> bool {
    !stream.truncated
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPromotionNotification {
    pub status: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumable_blocker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
}

impl AgentTaskPromotionTarget {
    pub(crate) fn from_worktree(worktree: String, path: Option<&Path>) -> Self {
        Self {
            worktree,
            path: path.map(|path| path.display().to_string()),
            branch: path.and_then(|path| git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"])),
            head: path.and_then(|path| git_output(path, &["rev-parse", "HEAD"])),
            dirty: path.and_then(|path| {
                git_output(path, &["status", "--porcelain"]).map(|status| !status.is_empty())
            }),
        }
    }
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn promotion_report_schema() -> String {
    AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string()
}

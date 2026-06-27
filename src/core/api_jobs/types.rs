use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::remote_runner::JobArtifactMetadata;
use super::remote_runner::RunnerJobLifecycleMetadata;
use crate::core::source_snapshot::SourceSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub(super) fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    pub fn run_status_label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "pass",
            Self::Failed => "fail",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn daemon_status_label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobEventKind {
    Status,
    Stdout,
    Stderr,
    Progress,
    Result,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub operation: String,
    pub status: JobStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    pub event_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<SourceSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by_runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<JobArtifactMetadata>,
}

/// Shared lease/claim identity fields carried by jobs that can be claimed by a
/// runner. Flattened into the owning structs so the on-wire JSON shape is
/// identical to the previously inlined fields (each field keeps its
/// `skip_serializing_if`/`default` attrs verbatim).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobClaimMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by_runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveRunnerJobSummary {
    pub runner_id: String,
    pub job_id: String,
    pub operation: String,
    pub source: String,
    pub kind: String,
    pub status: JobStatus,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub started_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub heartbeat_age_ms: u64,
    #[serde(flatten)]
    pub claim: JobClaimMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_expires_in_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<RunnerJobLifecycleMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_child_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_cell_count: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerJobSource {
    Broker,
    Daemon,
    #[serde(rename = "runner-daemon")]
    RunnerDaemon,
    #[serde(rename = "direct-daemon")]
    DirectDaemon,
    #[serde(rename = "reverse-broker")]
    ReverseBroker,
    Unknown,
}

impl RunnerJobSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Broker => "broker",
            Self::Daemon => "daemon",
            Self::RunnerDaemon => "runner-daemon",
            Self::DirectDaemon => "direct-daemon",
            Self::ReverseBroker => "reverse-broker",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_metadata(value: &str) -> Self {
        match value {
            "broker" => Self::Broker,
            "daemon" => Self::Daemon,
            "runner-daemon" | "runner_daemon" => Self::RunnerDaemon,
            "direct-daemon" | "direct_daemon" => Self::DirectDaemon,
            "reverse-broker" | "reverse_broker" => Self::ReverseBroker,
            _ => Self::Unknown,
        }
    }

    pub fn lifecycle_owner(self) -> RunnerJobLifecycleOwner {
        match self {
            Self::Broker | Self::ReverseBroker => RunnerJobLifecycleOwner::Broker,
            _ => RunnerJobLifecycleOwner::Controller,
        }
    }
}

impl fmt::Display for RunnerJobSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerJobLifecycleOwner {
    Broker,
    Controller,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRunnerJobRunSummary {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub command: String,
    pub cwd: Option<String>,
    pub status_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEvent {
    pub sequence: u64,
    pub job_id: Uuid,
    pub kind: JobEventKind,
    pub timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

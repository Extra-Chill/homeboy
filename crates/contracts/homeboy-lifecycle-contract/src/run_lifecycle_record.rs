use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RUN_LIFECYCLE_RECORD_SCHEMA: &str = "homeboy/run-lifecycle-record/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunLifecycleRecord {
    #[serde(default = "record_schema")]
    pub schema: String,
    pub execution: RunExecutionLifecycle,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_runtime: Vec<ProviderRuntimeLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat: Option<RunHeartbeat>,
    #[serde(default)]
    pub cleanup: CleanupLifecycle,
    #[serde(default)]
    pub finalization: FinalizationLifecycle,
    #[serde(default)]
    pub artifact_retention: ArtifactRetentionLifecycle,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_runtime_ids: Vec<ExternalRuntimeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Default for RunLifecycleRecord {
    fn default() -> Self {
        Self {
            schema: record_schema(),
            execution: RunExecutionLifecycle::default(),
            provider_runtime: Vec::new(),
            heartbeat: None,
            cleanup: CleanupLifecycle::default(),
            finalization: FinalizationLifecycle::default(),
            artifact_retention: ArtifactRetentionLifecycle::default(),
            external_runtime_ids: Vec::new(),
            updated_at: None,
        }
    }
}

impl RunLifecycleRecord {
    pub fn with_execution_state(state: RunExecutionState) -> Self {
        Self {
            execution: RunExecutionLifecycle {
                state,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn provider_runtime_state(&self) -> ProviderRuntimeState {
        let mut states = self.provider_runtime.iter().map(|runtime| runtime.state);
        let Some(first) = states.next() else {
            return ProviderRuntimeState::NotStarted;
        };
        if states.all(|state| state == first) {
            first
        } else {
            ProviderRuntimeState::Mixed
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunExecutionLifecycle {
    pub state: RunExecutionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Default for RunExecutionLifecycle {
    fn default() -> Self {
        Self {
            state: RunExecutionState::Unknown,
            started_at: None,
            finished_at: None,
            updated_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunExecutionState {
    Unknown,
    Queued,
    Running,
    Succeeded,
    PartialFailure,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderRuntimeLifecycle {
    pub task_id: String,
    pub backend: String,
    pub state: ProviderRuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_runtime_ids: Vec<ExternalRuntimeId>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRuntimeState {
    Unknown,
    NotStarted,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Mixed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalRuntimeId {
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunHeartbeat {
    pub last_seen_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupLifecycle {
    pub state: CleanupState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Default for CleanupLifecycle {
    fn default() -> Self {
        Self {
            state: CleanupState::Unknown,
            policy: None,
            updated_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CleanupState {
    Unknown,
    NotRequired,
    Pending,
    Running,
    Succeeded,
    Failed,
    Preserved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalizationLifecycle {
    pub state: FinalizationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Default for FinalizationLifecycle {
    fn default() -> Self {
        Self {
            state: FinalizationState::NotRequested,
            updated_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinalizationState {
    NotRequested,
    Pending,
    Running,
    Succeeded,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRetentionLifecycle {
    pub status: ArtifactRetentionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Default for ArtifactRetentionLifecycle {
    fn default() -> Self {
        Self {
            status: ArtifactRetentionStatus::Unknown,
            policy: None,
            updated_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRetentionStatus {
    Unknown,
    NotApplicable,
    Pending,
    Retained,
    Expired,
    Deleted,
    Failed,
}

fn record_schema() -> String {
    RUN_LIFECYCLE_RECORD_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_record_serializes_typed_runtime_state() {
        let record = RunLifecycleRecord {
            execution: RunExecutionLifecycle {
                state: RunExecutionState::Running,
                started_at: Some("2026-06-16T00:00:00Z".to_string()),
                finished_at: None,
                updated_at: Some("2026-06-16T00:00:05Z".to_string()),
            },
            provider_runtime: vec![ProviderRuntimeLifecycle {
                task_id: "task-a".to_string(),
                backend: "sample-runtime".to_string(),
                state: ProviderRuntimeState::Running,
                stream_uri: Some("provider://runs/provider-run-123/events".to_string()),
                external_runtime_ids: vec![ExternalRuntimeId {
                    kind: "provider_run_id".to_string(),
                    value: "provider-run-123".to_string(),
                    provider: Some("sample-runtime".to_string()),
                    url: None,
                }],
                metadata: Value::Null,
            }],
            heartbeat: Some(RunHeartbeat {
                last_seen_at: "2026-06-16T00:00:05Z".to_string(),
                owner_pid: Some(42),
                stale_after_seconds: Some(300),
            }),
            artifact_retention: ArtifactRetentionLifecycle {
                status: ArtifactRetentionStatus::Pending,
                policy: Some("retain".to_string()),
                updated_at: Some("2026-06-16T00:00:05Z".to_string()),
            },
            ..RunLifecycleRecord::default()
        };

        let json = serde_json::to_value(&record).expect("serialize lifecycle record");

        assert_eq!(json["schema"], RUN_LIFECYCLE_RECORD_SCHEMA);
        assert_eq!(json["execution"]["state"], "running");
        assert_eq!(json["provider_runtime"][0]["state"], "running");
        assert_eq!(
            json["provider_runtime"][0]["external_runtime_ids"][0]["value"],
            "provider-run-123"
        );
        assert_eq!(json["heartbeat"]["owner_pid"], 42);
        assert_eq!(json["artifact_retention"]["status"], "pending");

        let round_trip: RunLifecycleRecord =
            serde_json::from_value(json).expect("deserialize lifecycle record");
        assert_eq!(round_trip, record);
    }
}

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

    /// Recompute `external_runtime_ids` from `provider_runtime`.
    ///
    /// The field is a flattened index of the ids already carried by each entry
    /// in `provider_runtime`; it exists so a consumer can ask "what external
    /// runtimes did this run touch" without walking the per-task structure. It
    /// is therefore never independent data, and any writer that sets
    /// `provider_runtime` owes a call here.
    ///
    /// It stays a serialized field rather than becoming a method because it is
    /// published in `homeboy/run-lifecycle-record/v1` and this crate cannot see
    /// its out-of-tree consumers.
    ///
    /// Readers that test both this field *and* every entry's own ids are not
    /// being redundant, and collapsing them onto `provider_runtime` alone
    /// regresses four lifecycle tests plus a cook adoption test. The field is
    /// `#[serde(default)]`, so a record written before it existed deserializes
    /// with an empty vector while `provider_runtime` is populated: for those
    /// records "the flattened index is empty" and "no entry has ids" are
    /// genuinely different questions, and the conjunction is what keeps a
    /// migrated legacy run classified as having had no real provider
    /// execution. Leave those call sites alone.
    pub fn refresh_external_runtime_ids(&mut self) {
        self.external_runtime_ids = self
            .provider_runtime
            .iter()
            .flat_map(|runtime| runtime.external_runtime_ids.clone())
            .collect();
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

/// The generic execution state carried by [`RunLifecycleRecord`].
///
/// # Recoverable states are distinct from partial failure
///
/// `CandidateRecoverable` and `PartialRecoverable` used to collapse into
/// `PartialFailure` on the way in from `AgentTaskRunState`. They are not the
/// same situation: a recoverable run stopped holding something a consumer can
/// still act on — a promotable candidate, or partial work that can be resumed —
/// whereas `PartialFailure` is the terminal, unsuccessful, nothing-left-to-do
/// case. Collapsing them meant a consumer reading only the serialized record
/// (an `activity` listing, an HTTP response, a chat-plane message) could not
/// tell "there is a candidate waiting to be promoted" from "this partially
/// failed", because both arrived as the same string.
///
/// All three remain terminal and unsuccessful. The distinction is *what an
/// operator or orchestrator can do next*, not whether the run is finished.
///
/// # Unknown is the tolerance hatch
///
/// `Unknown` is `#[serde(other)]`, so a record written by a newer binary that
/// has learned a state this one has not degrades to `Unknown` instead of
/// failing the whole record parse. Without it, adding a variant here breaks
/// every older reader of the durable record. Do not remove it, and do not add
/// a second catch-all.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunExecutionState {
    Queued,
    Running,
    Succeeded,
    /// Stopped, unsuccessful, but a promotable candidate survived.
    CandidateRecoverable,
    /// Stopped, unsuccessful, but the partial work can be resumed.
    PartialRecoverable,
    PartialFailure,
    Failed,
    Cancelled,
    /// A state minted by a producer this binary does not know. Never guessed.
    #[serde(other)]
    Unknown,
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

/// Cleanup progress for a run.
///
/// `Unknown` is `#[serde(other)]`, so a state this binary does not know —
/// including `not_required`, removed in #13398 as unreachable — degrades to
/// "no verdict" rather than failing the enclosing record parse.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CleanupState {
    Pending,
    Running,
    Succeeded,
    Failed,
    /// Cleanup ran and deliberately kept the workspace.
    Preserved,
    #[serde(other)]
    Unknown,
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

/// Finalization progress for a run.
///
/// # Nothing writes this
///
/// Every reference to this type is structural — the field, the re-export, and
/// the `Default` above. No code assigns it, so `finalization.state` is
/// `not_requested` in every record ever written and the remaining variants have
/// never been observed on disk.
///
/// It is left in place because removing it changes the durable
/// `RunLifecycleRecord` shape, which is a separate decision from deleting
/// unreachable variants (#13398). The open question is whether finalization
/// *should* be reporting progress here — in which case the missing writer is
/// the defect — or whether this and [`FinalizationLifecycle`] should come out
/// of the record entirely.
///
/// Unlike its two siblings this enum has no catch-all, so it cannot carry a
/// `#[serde(other)]` hatch without first choosing which variant means "no
/// verdict". Do not add variants here until the question above is answered.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinalizationState {
    NotRequested,
    Pending,
    Running,
    Succeeded,
    Failed,
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
/// Retention outcome for a run's artifacts.
///
/// `Unknown` is `#[serde(other)]`, so a status this binary does not know —
/// including `expired` and `deleted`, removed in #13398 as unreachable —
/// degrades to "no verdict" rather than failing the enclosing record parse.
pub enum ArtifactRetentionStatus {
    NotApplicable,
    Pending,
    /// Retention ran and the artifacts were kept.
    Retained,
    Failed,
    #[serde(other)]
    Unknown,
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

#[cfg(test)]
mod run_execution_state_compatibility_tests {
    use super::*;

    /// A record written before #6761 added the recoverable variants must still
    /// read. This is the read-path migration test: every label the previous
    /// vocabulary could persist is listed, so a rename or reorder that would
    /// strand durable records fails here.
    #[test]
    fn every_pre_existing_durable_label_still_deserializes() {
        for (label, expected) in [
            ("unknown", RunExecutionState::Unknown),
            ("queued", RunExecutionState::Queued),
            ("running", RunExecutionState::Running),
            ("succeeded", RunExecutionState::Succeeded),
            ("partial_failure", RunExecutionState::PartialFailure),
            ("failed", RunExecutionState::Failed),
            ("cancelled", RunExecutionState::Cancelled),
        ] {
            let parsed: RunExecutionState =
                serde_json::from_value(label.into()).expect("legacy label must parse");
            assert_eq!(parsed, expected, "{label}");
            assert_eq!(
                serde_json::to_value(parsed).expect("serialize"),
                serde_json::json!(label),
                "{label} must round-trip to the same durable string"
            );
        }
    }

    /// The variants added by #6761, pinned to their wire strings. These are
    /// now persisted, so changing them is a durable-format break.
    #[test]
    fn recoverable_states_have_stable_wire_labels() {
        for (label, state) in [
            (
                "candidate_recoverable",
                RunExecutionState::CandidateRecoverable,
            ),
            ("partial_recoverable", RunExecutionState::PartialRecoverable),
        ] {
            assert_eq!(
                serde_json::to_value(state).expect("serialize"),
                serde_json::json!(label)
            );
            let parsed: RunExecutionState = serde_json::from_value(label.into()).expect("parse");
            assert_eq!(parsed, state);
        }
    }

    /// The tolerance hatch, and the reason adding a variant here is safe from
    /// now on: a state minted by a newer binary degrades to `Unknown` instead
    /// of failing the enclosing record parse.
    ///
    /// Without `#[serde(other)]` this test fails and, more importantly, every
    /// older reader of a record containing a new state fails with it.
    #[test]
    fn a_state_from_a_newer_binary_degrades_to_unknown() {
        let parsed: RunExecutionState =
            serde_json::from_value("some_state_from_a_newer_binary".into())
                .expect("unknown state must not fail the parse");

        assert_eq!(parsed, RunExecutionState::Unknown);
    }

    /// The degradation must hold through the enclosing record, which is the
    /// shape that actually lands on disk.
    #[test]
    fn an_unknown_state_does_not_fail_the_enclosing_record() {
        let record: RunLifecycleRecord = serde_json::from_value(serde_json::json!({
            "schema": RUN_LIFECYCLE_RECORD_SCHEMA,
            "execution": { "state": "a_state_this_binary_has_never_heard_of" },
        }))
        .expect("record with an unknown execution state must still parse");

        assert_eq!(record.execution.state, RunExecutionState::Unknown);
    }
}

#[cfg(test)]
mod subsystem_state_compatibility_tests {
    use super::*;

    /// The variants #13398 removed had no producer anywhere in the tree, so no
    /// record can contain them. This pins the belt-and-braces case anyway: if
    /// one ever appears — a hand-edited record, a binary from another branch —
    /// it degrades to `Unknown` instead of failing the whole record parse.
    ///
    /// This is what makes deleting a variant from a durable enum safe, and it
    /// is why the removal shipped together with the `#[serde(other)]` hatch
    /// rather than on its own.
    #[test]
    fn removed_states_degrade_to_unknown_rather_than_failing() {
        for label in ["not_required", "some_state_from_another_binary"] {
            let parsed: CleanupState =
                serde_json::from_value(label.into()).expect("must not fail the parse");
            assert_eq!(parsed, CleanupState::Unknown, "{label}");
        }

        for label in ["expired", "deleted", "a_status_this_binary_never_had"] {
            let parsed: ArtifactRetentionStatus =
                serde_json::from_value(label.into()).expect("must not fail the parse");
            assert_eq!(parsed, ArtifactRetentionStatus::Unknown, "{label}");
        }
    }

    /// Degradation has to survive the enclosing record, which is the shape that
    /// actually lands on disk.
    #[test]
    fn a_removed_state_does_not_fail_the_enclosing_record() {
        let record: RunLifecycleRecord = serde_json::from_value(serde_json::json!({
            "schema": RUN_LIFECYCLE_RECORD_SCHEMA,
            "execution": { "state": "running" },
            "cleanup": { "state": "not_required" },
            "artifact_retention": { "status": "deleted" },
        }))
        .expect("record carrying removed states must still parse");

        assert_eq!(record.cleanup.state, CleanupState::Unknown);
        assert_eq!(
            record.artifact_retention.status,
            ArtifactRetentionStatus::Unknown
        );
    }

    /// Every surviving variant keeps its durable spelling. Deleting a variant
    /// must not renumber or rename its neighbours.
    #[test]
    fn surviving_states_keep_their_wire_labels() {
        for (label, state) in [
            ("pending", CleanupState::Pending),
            ("running", CleanupState::Running),
            ("succeeded", CleanupState::Succeeded),
            ("failed", CleanupState::Failed),
            ("preserved", CleanupState::Preserved),
            ("unknown", CleanupState::Unknown),
        ] {
            assert_eq!(
                serde_json::to_value(state).expect("serialize"),
                serde_json::json!(label)
            );
        }

        for (label, status) in [
            ("not_applicable", ArtifactRetentionStatus::NotApplicable),
            ("pending", ArtifactRetentionStatus::Pending),
            ("retained", ArtifactRetentionStatus::Retained),
            ("failed", ArtifactRetentionStatus::Failed),
            ("unknown", ArtifactRetentionStatus::Unknown),
        ] {
            assert_eq!(
                serde_json::to_value(status).expect("serialize"),
                serde_json::json!(label)
            );
        }

        // FinalizationState has no catch-all, so its labels are pinned without
        // a degradation case. `blocked` was removed with the others.
        for (label, state) in [
            ("not_requested", FinalizationState::NotRequested),
            ("pending", FinalizationState::Pending),
            ("running", FinalizationState::Running),
            ("succeeded", FinalizationState::Succeeded),
            ("failed", FinalizationState::Failed),
        ] {
            assert_eq!(
                serde_json::to_value(state).expect("serialize"),
                serde_json::json!(label)
            );
        }
    }
}

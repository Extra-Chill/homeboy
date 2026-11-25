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
/// # A recoverable state is distinct from partial failure
///
/// `CandidateRecoverable` used to collapse into `PartialFailure` on the way in
/// from `RunExecutionState`. They are not the same situation: a recoverable
/// run stopped holding a promotable candidate a consumer can still act on,
/// whereas `PartialFailure` is the terminal, unsuccessful, nothing-left-to-do
/// case. Collapsing them meant a consumer reading only the serialized record
/// (an HTTP response or a chat-plane message) could not
/// tell "there is a candidate waiting to be promoted" from "this partially
/// failed", because both arrived as the same string.
///
/// Both remain terminal and unsuccessful. The distinction is *what an
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
    PartialFailure,
    Failed,
    Cancelled,
    /// A state minted by a producer this binary does not know. Never guessed.
    #[serde(other)]
    Unknown,
}

impl RunExecutionState {
    /// Whether this is a terminal run state — the run has finished (successfully,
    /// with a recoverable candidate, partially, or by failure/cancellation) and
    /// will not transition further on its own.
    ///
    /// The single definition of run terminality. Every place that used to
    /// enumerate this set inline (`matches!(state, Succeeded | CandidateRecoverable
    /// | ...)`) delegates here, so adding or reclassifying a run state cannot
    /// leave a stale copy behind.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunExecutionState::Succeeded
                | RunExecutionState::CandidateRecoverable
                | RunExecutionState::PartialFailure
                | RunExecutionState::Failed
                | RunExecutionState::Cancelled
                | RunExecutionState::Unknown
        )
    }

    /// The run state transition graph: whether a committed record in `self`
    /// may be replaced by one in `next`.
    ///
    /// The single definition of legal run state edges. Every record write
    /// passes through `write_record_guarded`, which refuses an edge this
    /// rejects with `agent_task_illegal_transition`, so no closure or prepared
    /// record can commit a state change the graph does not name (#15718).
    ///
    /// | from | to |
    /// |---|---|
    /// | any | itself (annotation, idempotent re-projection) |
    /// | `Queued`, `Running` | any |
    /// | any terminal | `Queued` |
    /// | `Cancelled` | `Running` |
    /// | `CandidateRecoverable` | `Cancelled` |
    /// | non-cancelled terminal | non-cancelled terminal |
    pub fn can_transition_to(self, next: RunExecutionState) -> bool {
        use RunExecutionState::*;
        match (self, next) {
            (Unknown, _) | (_, Unknown) => self == next,
            _ if self == next => true,
            // A live run is claimed (`Start`, `BindLabHandoff`), returned to
            // the queue (`Requeue`, `reset_for_requeue`) or finished.
            (Queued | Running, _) => true,
            // A finished run re-enters only through the queue: a plan
            // resubmission (`Submit`), the zero-execution rearm
            // (`rearm_pre_execution_record_inner`), and the reclaim of a
            // detached Cook that failed before its handoff projected
            // (`claim_detached_cook_handoff_parent_in_store`).
            (_, Queued) => true,
            // A runner's late acceptance of an expired Lab handoff supersedes
            // the synthetic expiry cancellation (`BindLabHandoff`).
            (Cancelled, Running) => true,
            // Otherwise a cancellation is final: its `run.cancelled` receipt
            // is immutable, so nothing may re-project the run as another
            // outcome.
            (Cancelled, _) => false,
            // No other finished run resumes in place.
            (_, Running) => false,
            // A run stopped with a recoverable candidate is waiting on a
            // decision, which the operator may make by cancelling it
            // (`cancel`). Every other outcome refuses cancellation.
            (CandidateRecoverable, Cancelled) => true,
            (_, Cancelled) => false,
            // Between the other outcomes a finished run is re-projected from
            // later evidence: an aggregate (`apply_aggregate_to_record`), a
            // Cook's `no_candidate` result, or a candidate adoption that
            // supersedes a pre-execution failure.
            _ => true,
        }
    }
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
mod durable_state_label_tests {
    use super::*;

    /// Every state label a durable record can hold keeps its spelling in both
    /// directions. A rename or reorder that would strand records on disk
    /// fails here.
    #[test]
    fn durable_state_labels_round_trip() {
        fn assert_round_trip<T>(label: &str, state: T)
        where
            T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
        {
            let parsed: T = serde_json::from_value(label.into()).expect("durable label parses");
            assert_eq!(parsed, state, "{label}");
            assert_eq!(
                serde_json::to_value(parsed).expect("serialize"),
                serde_json::json!(label),
                "{label} must round-trip to the same durable string"
            );
        }

        for (label, state) in [
            ("unknown", RunExecutionState::Unknown),
            ("queued", RunExecutionState::Queued),
            ("running", RunExecutionState::Running),
            ("succeeded", RunExecutionState::Succeeded),
            (
                "candidate_recoverable",
                RunExecutionState::CandidateRecoverable,
            ),
            ("partial_failure", RunExecutionState::PartialFailure),
            ("failed", RunExecutionState::Failed),
            ("cancelled", RunExecutionState::Cancelled),
        ] {
            assert_round_trip(label, state);
        }
        for (label, state) in [
            ("pending", CleanupState::Pending),
            ("running", CleanupState::Running),
            ("succeeded", CleanupState::Succeeded),
            ("failed", CleanupState::Failed),
            ("preserved", CleanupState::Preserved),
            ("unknown", CleanupState::Unknown),
        ] {
            assert_round_trip(label, state);
        }
        for (label, status) in [
            ("not_applicable", ArtifactRetentionStatus::NotApplicable),
            ("pending", ArtifactRetentionStatus::Pending),
            ("retained", ArtifactRetentionStatus::Retained),
            ("failed", ArtifactRetentionStatus::Failed),
            ("unknown", ArtifactRetentionStatus::Unknown),
        ] {
            assert_round_trip(label, status);
        }
    }

    /// States minted by a newer binary degrade to `Unknown` instead of failing
    /// the enclosing record parse, which is the shape that lands on disk.
    #[test]
    fn states_from_a_newer_binary_degrade_to_unknown_through_the_record() {
        let record: RunLifecycleRecord = serde_json::from_value(serde_json::json!({
            "schema": RUN_LIFECYCLE_RECORD_SCHEMA,
            "execution": { "state": "a_state_this_binary_has_never_heard_of" },
            "cleanup": { "state": "a_future_cleanup_state" },
            "artifact_retention": { "status": "a_future_retention_status" },
            // Retired, never-written field: old records still parse.
            "finalization": { "state": "not_requested" },
        }))
        .expect("record carrying unknown states must still parse");

        assert_eq!(record.execution.state, RunExecutionState::Unknown);
        assert_eq!(record.cleanup.state, CleanupState::Unknown);
        assert_eq!(
            record.artifact_retention.status,
            ArtifactRetentionStatus::Unknown
        );
    }
}

#[cfg(test)]
mod execution_state_policy_tests {
    use super::RunExecutionState::*;

    #[test]
    fn unknown_is_terminal_unsuccessful_and_cannot_transition() {
        assert!(Unknown.is_terminal());
        assert_ne!(Unknown, Succeeded);
        assert!(Unknown.can_transition_to(Unknown));
        for state in [
            Queued,
            Running,
            Succeeded,
            CandidateRecoverable,
            PartialFailure,
            Failed,
            Cancelled,
        ] {
            assert!(!Unknown.can_transition_to(state));
            assert!(!state.can_transition_to(Unknown));
        }
        assert!(Running.can_transition_to(Succeeded));
        assert!(Failed.can_transition_to(Queued));
        assert!(!Succeeded.can_transition_to(Running));
        assert!(!Cancelled.can_transition_to(Succeeded));
    }
}

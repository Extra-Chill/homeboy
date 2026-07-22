use super::*;

pub(crate) mod schemas {
    pub(crate) const RUN: &str = "homeboy/agent-task-run/v1";
    pub(crate) const RUN_LOG: &str = "homeboy/agent-task-run-log/v2";
    pub(crate) const EVENT: &str = "homeboy/agent-task-event/v1";
    pub(crate) const RUN_STATUS: &str = "homeboy/agent-task-run-status/v1";
    pub(crate) const RUN_ARTIFACTS: &str = "homeboy/agent-task-run-artifacts/v1";
    pub(crate) const COOK_INDEX: &str = "homeboy/agent-task-cook-index/v1";
}

pub const AGENT_TASK_RECORD_HEALTH_SCHEMA: &str = "homeboy/agent-task-record-health/v1";
pub const AGENT_TASK_RECORD_RECONCILIATION_SCHEMA: &str =
    "homeboy/agent-task-record-reconciliation/v1";

// Untyped run-record metadata keys for the controller's staleness / runner
// liveness projection. Centralized so every read and write goes through one
// name — a typo in any scattered string literal would otherwise silently break
// staleness detection. (Step toward migrating these flags onto the typed
// lifecycle state; for now this removes the string-drift hazard without
// changing the on-disk format.)
pub(crate) const METADATA_KEY_STALE_RUNNING: &str = "stale_running";
pub(crate) const METADATA_KEY_STALE_RUNNING_REASON: &str = "stale_running_reason";
pub(crate) const METADATA_KEY_RUNNER_LIVENESS: &str = "runner_liveness";
pub(crate) const METADATA_KEY_RETRYABLE: &str = "retryable";
pub(crate) const METADATA_KEY_RECLAIMED_STALE_RUNNING: &str = "reclaimed_stale_running";
pub(crate) const METADATA_KEY_CANCELLED_STALE_RUNNING: &str = "cancelled_stale_running";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskRecordHealthReason {
    MissingMetadata,
    MalformedMetadata,
    LegacySchema,
    ConflictingProjections,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRecordHealthItem {
    pub run_id: String,
    pub reason: AgentTaskRecordHealthReason,
    pub quarantined: bool,
    pub remediation: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRecordHealthSummary {
    pub schema: String,
    pub healthy: usize,
    pub malformed: usize,
    pub legacy: usize,
    pub conflicting: usize,
    pub quarantined: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<AgentTaskRecordHealthItem>,
}

impl AgentTaskRecordHealthSummary {
    pub(crate) fn healthy() -> Self {
        Self {
            schema: AGENT_TASK_RECORD_HEALTH_SCHEMA.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRecordReconciliationItem {
    pub run_id: String,
    pub reason: AgentTaskRecordHealthReason,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRecordReconciliationReport {
    pub schema: String,
    pub dry_run: bool,
    pub considered: usize,
    pub migrated: usize,
    pub quarantined: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<AgentTaskRecordReconciliationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunRecord {
    pub schema: String,
    pub run_id: String,
    pub plan_id: String,
    pub state: AgentTaskRunState,
    pub submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub plan_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<crate::agent_task_scheduler::AgentTaskAggregateTotals>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTaskRunTask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_handles: Vec<AgentTaskRunProviderHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_executor_evidence: Option<AgentTaskLatestExecutorEvidence>,
    #[serde(default)]
    pub lifecycle: RunLifecycleRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lab_handoff: Option<AgentTaskLabHandoff>,
    /// Controller-owned progress for an externally prepared candidate being
    /// verified against this durable cook run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_adoption: Option<AgentTaskCandidateAdoptionAttempt>,
    /// Owning run for `candidate_adoption` when status was resolved through a
    /// Cook alias. Exact run status leaves this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adoption_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCandidateAdoptionAttempt {
    pub candidate_sha: String,
    pub ai_model: String,
    pub state: String,
    pub phase: String,
    pub active_gate: String,
    pub started_at: String,
    pub updated_at: String,
    pub owner_pid: u32,
    pub heartbeat_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_process_group: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub gate_output_tail: String,
    #[serde(default)]
    pub resume_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Durable controller outcome for idempotent adoption replay. A completed
    /// adoption can legitimately be non-green when remediation was blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

impl AgentTaskCandidateAdoptionAttempt {
    pub fn is_active(&self) -> bool {
        self.state == "verification_running"
    }
}

/// Durable authority for a controller-to-Lab runner handoff. This lives on the
/// controller record because it remains authoritative across transport loss.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLabHandoff {
    pub state: AgentTaskLabHandoffState,
    pub authority: AgentTaskLabHandoffAuthority,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_deadline_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLabHandoffState {
    Pending,
    Accepted,
    Expired,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLabHandoffAuthority {
    Controller,
    RunnerDaemon,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookIndex {
    #[serde(default = "cook_index_schema")]
    pub schema: String,
    pub cook_id: String,
    pub latest_run_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<AgentTaskCookIndexAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookIndexAttempt {
    pub attempt: u32,
    pub run_id: String,
    pub recorded_at: String,
}

fn cook_index_schema() -> String {
    schemas::COOK_INDEX.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskLatestExecutorEvidence {
    pub task_id: String,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub input_ref: AgentTaskEvidenceRef,
    pub normalized_output_ref: AgentTaskEvidenceRef,
    pub outcome_ref: AgentTaskEvidenceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_contracts: Vec<AgentTaskComponentContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_component_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub typed_artifact_expectations: Vec<String>,
}

impl AgentTaskLatestExecutorEvidence {
    pub(crate) fn refs(&self) -> [AgentTaskEvidenceRef; 3] {
        [
            self.input_ref.clone(),
            self.normalized_output_ref.clone(),
            self.outcome_ref.clone(),
        ]
    }
}

impl AgentTaskRunRecord {
    /// Hydrate only the established v1 Lab metadata shape. Invalid or
    /// inconsistent legacy projections deliberately remain untrusted.
    pub(crate) fn hydrate_legacy_lab_handoff(&mut self) {
        if self.lab_handoff.is_none() {
            self.lab_handoff = AgentTaskLabHandoff::from_legacy_metadata(&self.metadata);
        }
    }

    pub(crate) fn has_accepted_lab_handoff(&self) -> bool {
        self.lab_handoff.as_ref().is_some_and(|handoff| {
            handoff.is_valid() && handoff.state == AgentTaskLabHandoffState::Accepted
        })
    }

    /// Whether this run already carries durable evidence of a *completed*
    /// provider attempt whose candidate must survive a later transport/handoff
    /// error. When true, recording a pre-execution failure would overwrite a
    /// succeeded candidate with a `Failed`, zero-artifact,
    /// `provider_executions_consumed: 0` terminal record and strand the work
    /// (#9377).
    ///
    /// This deliberately requires *completed-work* evidence, not merely an
    /// accepted handoff or a claimed runner job id: an accepted job can later be
    /// confirmed absent/lost with no candidate, and that genuinely-lost case
    /// must still terminalize (see `terminalize_lost_accepted_runner_job`, which
    /// only fires when `provider_handles` is empty). The completed-work signals:
    /// - one or more provider handles (a provider run id was returned), or
    /// - a successful / recoverable-candidate terminal state (the run produced a
    ///   candidate), i.e. a terminal state other than a bare `Failed`/`Cancelled`.
    pub(crate) fn has_recorded_provider_progress(&self) -> bool {
        !self.provider_handles.is_empty() || self.has_candidate_terminal_state()
    }

    /// A terminal state that carries a produced candidate (success or a
    /// recoverable/partial candidate), as opposed to a bare failure/cancellation
    /// that produced nothing to preserve.
    fn has_candidate_terminal_state(&self) -> bool {
        matches!(
            self.state,
            AgentTaskRunState::Succeeded
                | AgentTaskRunState::CandidateRecoverable
                | AgentTaskRunState::PartialRecoverable
                | AgentTaskRunState::PartialFailure
        )
    }

    pub(crate) fn lab_handoff_validation_error(&self) -> Option<&'static str> {
        self.lab_handoff
            .as_ref()
            .and_then(AgentTaskLabHandoff::validation_error)
    }

    pub(crate) fn has_expired_pending_lab_handoff(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        self.state == AgentTaskRunState::Queued
            && self.lab_handoff.as_ref().is_some_and(|handoff| {
                handoff.is_valid()
                    && handoff.state == AgentTaskLabHandoffState::Pending
                    && handoff
                        .acceptance_deadline_at
                        .as_deref()
                        .and_then(parse_rfc3339)
                        .is_some_and(|deadline| deadline <= now)
            })
    }

    pub(crate) fn record_runner_metadata(&mut self, reclaimed_stale: bool) {
        let metadata = self.ensure_metadata_object();
        metadata.insert("runner_pid".to_string(), json!(std::process::id()));
        metadata.insert("runner_started_at".to_string(), json!(now_timestamp()));
        if reclaimed_stale {
            metadata.insert(
                METADATA_KEY_RECLAIMED_STALE_RUNNING.to_string(),
                json!(true),
            );
        } else {
            metadata.remove(METADATA_KEY_RECLAIMED_STALE_RUNNING);
        }
        metadata.remove(METADATA_KEY_STALE_RUNNING);
        metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
    }

    pub(crate) fn annotate_stale_running(&mut self) {
        if self.state != AgentTaskRunState::Running || self.owner_process_is_running() {
            return;
        }

        if self.runner_job_id().is_some() && self.has_fresh_update() {
            return;
        }

        let reason = if self.runner_job_id().is_some() {
            "runner_job_unverified_after_daemon_restart"
        } else if self.owner_pid().is_some() {
            "owner_process_not_running"
        } else {
            "missing_runner_pid"
        };
        let provider_handle_count = self.provider_handles.len();
        let metadata = self.ensure_metadata_object();
        metadata.insert(METADATA_KEY_STALE_RUNNING.to_string(), json!(true));
        metadata.insert(METADATA_KEY_STALE_RUNNING_REASON.to_string(), json!(reason));
        metadata.insert(
            "provider_boundary".to_string(),
            json!({
                "status": if provider_handle_count == 0 { "absent" } else { "recorded" },
                "provider_handle_count": provider_handle_count,
            }),
        );
        metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    }

    /// A controller cannot infer progress from a runner connection that is no
    /// longer available. Preserve the last confirmed heartbeat as evidence,
    /// while making its liveness explicitly unknown.
    pub(crate) fn annotate_runner_disconnected(&mut self) {
        if self.state != AgentTaskRunState::Running || !self.is_runner_backed() {
            return;
        }
        let metadata = self.ensure_metadata_object();
        metadata.insert(
            METADATA_KEY_RUNNER_LIVENESS.to_string(),
            json!("disconnected"),
        );
        metadata.insert(METADATA_KEY_STALE_RUNNING.to_string(), json!(true));
        metadata.insert(
            METADATA_KEY_STALE_RUNNING_REASON.to_string(),
            json!("runner_disconnected"),
        );
        metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    }

    /// A reachable runner job is authoritative liveness evidence. Clear only
    /// the controller's disconnected/stale projection, not unrelated metadata.
    pub(crate) fn record_runner_reachable(&mut self) {
        let metadata = self.ensure_metadata_object();
        metadata.insert(METADATA_KEY_RUNNER_LIVENESS.to_string(), json!("reachable"));
        metadata.remove(METADATA_KEY_STALE_RUNNING);
        metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
        metadata.remove(METADATA_KEY_RETRYABLE);
    }

    pub(crate) fn owner_process_is_running(&self) -> bool {
        self.owner_pid()
            .is_some_and(homeboy_core::process::pid_is_running)
    }

    pub(crate) fn owner_pid(&self) -> Option<u32> {
        self.metadata
            .get("runner_pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
    }

    pub fn runner_job_id(&self) -> Option<&str> {
        if let Some(handoff) = self.lab_handoff.as_ref() {
            if let Some(job_id) = handoff
                .is_valid()
                .then_some(handoff)
                .filter(|handoff| handoff.state == AgentTaskLabHandoffState::Accepted)
                .and_then(|handoff| handoff.runner_job_id.as_deref())
            {
                return Some(job_id);
            }
        }
        self.metadata
            .get("runner_job_id")
            .or_else(|| self.metadata.get("job_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn runner_id(&self) -> Option<&str> {
        if let Some(handoff) = self.lab_handoff.as_ref() {
            if handoff.is_valid() {
                return Some(handoff.runner_id.as_str());
            }
        }
        self.metadata
            .get("runner_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    /// Whether the controller has projected this run as stale (its runner
    /// backing can no longer be confirmed live). Reads the centralized untyped
    /// metadata flag.
    pub(crate) fn is_stale_running(&self) -> bool {
        self.metadata
            .get(METADATA_KEY_STALE_RUNNING)
            .and_then(Value::as_bool)
            == Some(true)
    }

    /// The recorded reason a run was projected stale, if any.
    pub(crate) fn stale_running_reason(&self) -> Option<&str> {
        self.metadata
            .get(METADATA_KEY_STALE_RUNNING_REASON)
            .and_then(Value::as_str)
    }

    /// Whether the authoritative agent-task run state (`self.state`) and its
    /// generic projection onto the durable lifecycle record
    /// (`self.lifecycle.execution.state`) agree.
    ///
    /// `set_run_state` is the single writer that keeps these in lockstep; this
    /// predicate is the same condition the record-health check uses to flag
    /// `ConflictingProjections`. Exposed so callers (and a `debug_assert` in the
    /// setter) can assert the invariant instead of silently diverging.
    pub(crate) fn run_state_projections_agree(&self) -> bool {
        RunExecutionState::from(self.state) == self.lifecycle.execution.state
    }

    fn has_fresh_update(&self) -> bool {
        self.updated_at
            .as_deref()
            .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
            .is_some_and(|updated_at| {
                chrono::Utc::now()
                    .signed_duration_since(updated_at.with_timezone(&chrono::Utc))
                    .num_minutes()
                    < 30
            })
    }

    /// A run is runner-backed when its durable record carries a runner id or
    /// runner job id. For these, the recorded owner pid lives on the *runner*
    /// host, not this controller, so local pid signalling cannot reach the
    /// provider process tree.
    pub(crate) fn is_runner_backed(&self) -> bool {
        self.runner_id().is_some() || self.runner_job_id().is_some()
    }

    pub(crate) fn ensure_metadata_object(&mut self) -> &mut serde_json::Map<String, Value> {
        if !self.metadata.is_object() {
            self.metadata = json!({});
        }
        self.metadata.as_object_mut().expect("metadata object")
    }
}

impl AgentTaskLabHandoff {
    pub(crate) fn pending(
        runner_id: &str,
        submitted_at: String,
        acceptance_deadline_at: String,
    ) -> Self {
        Self {
            state: AgentTaskLabHandoffState::Pending,
            authority: AgentTaskLabHandoffAuthority::Controller,
            runner_id: runner_id.to_string(),
            submission_key: None,
            payload_fingerprint: None,
            runner_job_id: None,
            submitted_at: Some(submitted_at),
            acceptance_deadline_at: Some(acceptance_deadline_at),
            accepted_at: None,
            expired_at: None,
        }
    }

    pub(crate) fn accepted(&self, runner_job_id: &str, accepted_at: String) -> Self {
        Self {
            state: AgentTaskLabHandoffState::Accepted,
            authority: AgentTaskLabHandoffAuthority::RunnerDaemon,
            runner_id: self.runner_id.clone(),
            submission_key: self.submission_key.clone(),
            payload_fingerprint: self.payload_fingerprint.clone(),
            runner_job_id: Some(runner_job_id.to_string()),
            submitted_at: self.submitted_at.clone(),
            acceptance_deadline_at: self.acceptance_deadline_at.clone(),
            accepted_at: Some(accepted_at),
            expired_at: None,
        }
    }

    pub(crate) fn expired(&self, expired_at: String) -> Self {
        Self {
            state: AgentTaskLabHandoffState::Expired,
            authority: AgentTaskLabHandoffAuthority::Controller,
            runner_id: self.runner_id.clone(),
            submission_key: self.submission_key.clone(),
            payload_fingerprint: self.payload_fingerprint.clone(),
            runner_job_id: None,
            submitted_at: self.submitted_at.clone(),
            acceptance_deadline_at: self.acceptance_deadline_at.clone(),
            accepted_at: None,
            expired_at: Some(expired_at),
        }
    }

    fn is_valid(&self) -> bool {
        self.validation_error().is_none()
    }

    pub(crate) fn validation_error(&self) -> Option<&'static str> {
        if self.runner_id.trim().is_empty() {
            return Some("Lab handoff runner_id is blank");
        }
        let valid_optional_timestamp = |timestamp: &Option<String>| {
            timestamp
                .as_deref()
                .is_none_or(|timestamp| parse_rfc3339(timestamp).is_some())
        };
        if !valid_optional_timestamp(&self.submitted_at)
            || !valid_optional_timestamp(&self.acceptance_deadline_at)
            || !valid_optional_timestamp(&self.accepted_at)
            || !valid_optional_timestamp(&self.expired_at)
        {
            return Some("Lab handoff contains an invalid timestamp");
        }
        match self.state {
            AgentTaskLabHandoffState::Pending
                if self.authority != AgentTaskLabHandoffAuthority::Controller
                    || self.runner_job_id.is_some()
                    || self.submitted_at.is_none()
                    || self.acceptance_deadline_at.is_none()
                    || self.accepted_at.is_some()
                    || self.expired_at.is_some() =>
            {
                Some("pending Lab handoff has invalid authority or timestamps")
            }
            AgentTaskLabHandoffState::Accepted
                if self.authority != AgentTaskLabHandoffAuthority::RunnerDaemon
                    || self
                        .runner_job_id
                        .as_deref()
                        .is_none_or(|job_id| job_id.trim().is_empty())
                    || self.accepted_at.is_none()
                    || self.expired_at.is_some() =>
            {
                Some("accepted Lab handoff has invalid authority, job identity, or timestamp")
            }
            AgentTaskLabHandoffState::Expired
                if self.authority != AgentTaskLabHandoffAuthority::Controller
                    || self.runner_job_id.is_some()
                    || self.expired_at.is_none()
                    || self.accepted_at.is_some() =>
            {
                Some("expired Lab handoff has invalid authority, job identity, or timestamp")
            }
            _ => None,
        }
    }

    fn from_legacy_metadata(metadata: &Value) -> Option<Self> {
        let object = metadata.as_object()?;
        let runner_id = required_string(object.get("runner_id"))?;
        let acceptance = object.get("handoff_acceptance")?.as_object()?;
        match acceptance.get("state")?.as_str()? {
            "pending"
                if object.get("lifecycle_store_owner").and_then(Value::as_str)
                    == Some("controller") =>
            {
                Some(Self::pending(
                    &runner_id,
                    required_timestamp(acceptance.get("started_at"))?,
                    required_timestamp(acceptance.get("deadline_at"))?,
                ))
            }
            "accepted" => {
                let accepted_at = required_timestamp(acceptance.get("accepted_at"))?;
                let runner_job_id = required_string(acceptance.get("runner_job_id"))?;
                let handoff = object.get("runner_handoff")?.as_object()?;
                let identity = handoff.get("identity")?.as_object()?;
                if handoff.get("authority").and_then(Value::as_str) != Some("runner_daemon")
                    || identity.get("runner_id").and_then(Value::as_str) != Some(runner_id.as_str())
                    || identity.get("runner_job_id").and_then(Value::as_str)
                        != Some(runner_job_id.as_str())
                    || required_string(object.get("runner_job_id")).as_deref()
                        != Some(runner_job_id.as_str())
                {
                    return None;
                }
                Some(Self {
                    state: AgentTaskLabHandoffState::Accepted,
                    authority: AgentTaskLabHandoffAuthority::RunnerDaemon,
                    runner_id,
                    submission_key: None,
                    payload_fingerprint: None,
                    runner_job_id: Some(runner_job_id),
                    submitted_at: acceptance.get("started_at").and_then(optional_timestamp),
                    acceptance_deadline_at: acceptance
                        .get("deadline_at")
                        .and_then(optional_timestamp),
                    accepted_at: Some(accepted_at),
                    expired_at: None,
                })
            }
            "expired"
                if object.get("lifecycle_store_owner").and_then(Value::as_str)
                    == Some("controller") =>
            {
                Some(Self {
                    state: AgentTaskLabHandoffState::Expired,
                    authority: AgentTaskLabHandoffAuthority::Controller,
                    runner_id,
                    submission_key: None,
                    payload_fingerprint: None,
                    runner_job_id: None,
                    submitted_at: None,
                    acceptance_deadline_at: None,
                    accepted_at: None,
                    expired_at: required_timestamp(acceptance.get("expired_at")),
                })
            }
            _ => None,
        }
    }
}

fn required_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
}

fn required_timestamp(value: Option<&Value>) -> Option<String> {
    let timestamp = required_string(value)?;
    parse_rfc3339(&timestamp)?;
    Some(timestamp)
}

fn optional_timestamp(value: &Value) -> Option<String> {
    required_timestamp(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn legacy_record(metadata: Value) -> AgentTaskRunRecord {
        serde_json::from_value(json!({
            "schema": schemas::RUN,
            "run_id": "legacy-handoff",
            "plan_id": "plan",
            "state": "queued",
            "submitted_at": "2026-01-01T00:00:00+00:00",
            "plan_path": "plan.json",
            "metadata": metadata,
        }))
        .expect("backward-compatible record parses")
    }

    #[test]
    fn legacy_pending_handoff_hydrates_and_round_trips() {
        let mut record = legacy_record(json!({
            "lifecycle_store_owner": "controller",
            "runner_id": "homeboy-lab",
            "handoff_acceptance": {
                "state": "pending",
                "started_at": "2026-01-01T00:00:00+00:00",
                "deadline_at": "2026-01-01T00:02:00+00:00"
            }
        }));
        assert!(record.lab_handoff.is_none());
        record.hydrate_legacy_lab_handoff();
        assert_eq!(
            record.lab_handoff.as_ref().map(|handoff| handoff.state),
            Some(AgentTaskLabHandoffState::Pending)
        );
        assert!(record.has_expired_pending_lab_handoff(
            chrono::DateTime::parse_from_rfc3339("2026-01-01T00:02:00+00:00")
                .expect("timestamp")
                .with_timezone(&chrono::Utc)
        ));
        let round_trip: AgentTaskRunRecord =
            serde_json::from_value(serde_json::to_value(&record).expect("serialize"))
                .expect("deserialize");
        assert_eq!(round_trip.lab_handoff, record.lab_handoff);
    }

    #[test]
    fn legacy_accepted_handoff_hydrates_but_inconsistent_metadata_fails_closed() {
        let metadata = json!({
            "runner_id": "homeboy-lab",
            "runner_job_id": "job-1",
            "handoff_acceptance": {
                "state": "accepted",
                "accepted_at": "2026-01-01T00:01:00+00:00",
                "runner_job_id": "job-1"
            },
            "runner_handoff": {
                "authority": "runner_daemon",
                "identity": { "runner_id": "homeboy-lab", "runner_job_id": "job-1" }
            }
        });
        let mut accepted = legacy_record(metadata.clone());
        accepted.hydrate_legacy_lab_handoff();
        assert!(accepted.has_accepted_lab_handoff());

        let mut inconsistent = legacy_record(metadata);
        inconsistent.metadata["runner_handoff"]["identity"]["runner_job_id"] = json!("other-job");
        inconsistent.hydrate_legacy_lab_handoff();
        assert!(inconsistent.lab_handoff.is_none());
        assert!(!inconsistent.has_accepted_lab_handoff());
    }

    #[test]
    fn malformed_typed_handoff_variants_fail_closed() {
        let pending = AgentTaskLabHandoff::pending(
            "homeboy-lab",
            "2026-01-01T00:00:00+00:00".to_string(),
            "2026-01-01T00:02:00+00:00".to_string(),
        );
        let accepted = pending.accepted("job-1", "2026-01-01T00:01:00+00:00".to_string());
        let expired = pending.expired("2026-01-01T00:03:00+00:00".to_string());
        let malformed = vec![
            AgentTaskLabHandoff {
                runner_id: " ".to_string(),
                ..pending.clone()
            },
            AgentTaskLabHandoff {
                authority: AgentTaskLabHandoffAuthority::RunnerDaemon,
                submitted_at: Some("invalid".to_string()),
                ..pending
            },
            AgentTaskLabHandoff {
                runner_job_id: Some(" ".to_string()),
                ..accepted.clone()
            },
            AgentTaskLabHandoff {
                accepted_at: None,
                ..accepted
            },
            AgentTaskLabHandoff {
                authority: AgentTaskLabHandoffAuthority::RunnerDaemon,
                ..expired.clone()
            },
            AgentTaskLabHandoff {
                expired_at: Some("invalid".to_string()),
                ..expired
            },
        ];
        for handoff in malformed {
            let mut record = legacy_record(Value::Null);
            record.lab_handoff = Some(handoff);
            assert!(record.lab_handoff_validation_error().is_some());
            assert!(!record.has_accepted_lab_handoff());
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskRunState {
    Queued,
    Running,
    Succeeded,
    /// Backward-compatible projection for older durable records.
    CandidateRecoverable,
    PartialRecoverable,
    PartialFailure,
    Failed,
    Cancelled,
}

impl AgentTaskRunState {
    /// Whether this is a terminal run state — the run has finished (successfully,
    /// with a recoverable candidate, partially, or by failure/cancellation) and
    /// will not transition further on its own.
    ///
    /// The single definition of run terminality. Every place that used to
    /// enumerate this set inline (`matches!(state, Succeeded | CandidateRecoverable
    /// | ...)`) delegates here, so adding or reclassifying a run state cannot
    /// leave a stale copy behind.
    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentTaskRunState::Succeeded
                | AgentTaskRunState::CandidateRecoverable
                | AgentTaskRunState::PartialRecoverable
                | AgentTaskRunState::PartialFailure
                | AgentTaskRunState::Failed
                | AgentTaskRunState::Cancelled
        )
    }
}

/// Canonical projection of a run's aggregate state onto the generic
/// `RunExecutionState` carried by `RunLifecycleRecord`. The two enums share
/// every agent-task variant 1:1 (`RunExecutionState` additionally models the
/// generic `Unknown` default), so this single `From` keeps `record.state` and
/// `record.lifecycle.execution.state` in lockstep instead of a hand-synced map.
impl From<AgentTaskRunState> for RunExecutionState {
    fn from(state: AgentTaskRunState) -> Self {
        match state {
            AgentTaskRunState::Queued => RunExecutionState::Queued,
            AgentTaskRunState::Running => RunExecutionState::Running,
            AgentTaskRunState::Succeeded => RunExecutionState::Succeeded,
            AgentTaskRunState::CandidateRecoverable => RunExecutionState::PartialFailure,
            AgentTaskRunState::PartialRecoverable => RunExecutionState::PartialFailure,
            AgentTaskRunState::PartialFailure => RunExecutionState::PartialFailure,
            AgentTaskRunState::Failed => RunExecutionState::Failed,
            AgentTaskRunState::Cancelled => RunExecutionState::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRunTask {
    pub task_id: String,
    pub state: AgentTaskState,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AgentTaskArtifactRef {
    pub task_id: String,
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunProviderHandle {
    #[serde(
        default,
        skip_serializing_if = "AgentTaskExecutionHandleKind::is_provider_run"
    )]
    pub kind: AgentTaskExecutionHandleKind,
    pub task_id: String,
    pub backend: String,
    pub provider_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentTaskState>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunLog {
    pub schema: String,
    pub run_id: String,
    /// The canonical consumer event stream. v1 exposed the same information
    /// twice as `events` and `normalized_events`.
    pub events: Vec<AgentTaskEventEnvelope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_events: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskEventEnvelope {
    pub schema: String,
    pub run_id: String,
    pub task_id: String,
    pub sequence: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub status: AgentTaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub progress: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunStatus {
    pub schema: String,
    pub run_id: String,
    pub plan_id: String,
    pub state: AgentTaskRunState,
    pub submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub totals: AgentTaskAggregateTotals,
    pub latest_event_cursor: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normalized_events: Vec<AgentTaskEventEnvelope>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskRunArtifacts {
    pub schema: String,
    pub run_id: String,
    pub artifacts: Vec<AgentTaskArtifact>,
    pub evidence_refs: Vec<AgentTaskEvidenceRef>,
}

//! Agent-task configuration and policy data types.
//!
//! Plain, self-contained config/policy structs for agent-task scheduling
//! (schedule options, retry/rotation policy, resolved provider policy, output
//! bindings, execution budgets, adoption records). These carry no dependency on
//! the scheduler runtime or the wider `plan` machinery, so they live in this
//! leaf module — the shared contract surface consumed by the scheduler,
//! dispatch service, and the lab-contract type layer alike.
//!
//! Re-exported from `agent_task_schedule` (`pub use agent_task_config::*`) to
//! keep existing `agent_task_schedule::*` call sites stable.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task_outcome::{AgentTaskFailureClassification, AgentTaskOutcomeStatus};

fn default_max_concurrency() -> usize {
    1
}

fn default_adaptive_min_concurrency() -> usize {
    1
}

fn default_task_resource_units() -> u32 {
    1
}

fn default_required_output() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskOutputDependencies {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub bindings: HashMap<String, AgentTaskOutputBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskOutputBinding {
    pub task_id: String,
    /// JSON Pointer into the prior `homeboy/agent-task-outcome/v1` object.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<AgentTaskArtifactBinding>,
    #[serde(default = "default_required_output")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub default: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskArtifactOutputDeclaration {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskArtifactBinding {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskScheduleOptions {
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tasks: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_queue_depth: Option<usize>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_executor_concurrency: HashMap<String, usize>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_model_concurrency: HashMap<String, usize>,
    #[serde(default)]
    pub resource_budget: AgentTaskResourceBudget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_concurrency: Option<AgentTaskAdaptiveConcurrencyPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub retry: AgentTaskRetryPolicy,
    /// Truthful operator budget for all provider executions of one task.
    /// This is independent from deterministic cook gate attempts.
    #[serde(default = "legacy_execution_budget")]
    pub execution_budget: AgentTaskExecutionBudget,
    /// Per-plan provider rotation policy. Takes precedence over the global
    /// Homeboy config `agent_task.rotation`; a per-task
    /// `metadata.provider_rotation` object overrides both (#6978).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<AgentTaskProviderRotationPolicy>,
}

impl Default for AgentTaskScheduleOptions {
    fn default() -> Self {
        Self {
            max_concurrency: default_max_concurrency(),
            max_tasks: None,
            max_queue_depth: None,
            per_executor_concurrency: HashMap::new(),
            per_model_concurrency: HashMap::new(),
            resource_budget: AgentTaskResourceBudget::default(),
            adaptive_concurrency: None,
            timeout_ms: None,
            retry: AgentTaskRetryPolicy::default(),
            execution_budget: AgentTaskExecutionBudget::default(),
            rotation: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAdaptiveConcurrencyPolicy {
    #[serde(default = "default_adaptive_min_concurrency")]
    pub min_concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_capacity: Option<usize>,
    #[serde(default)]
    pub active_leases: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_depth: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_pressure: Option<AgentTaskResourcePressure>,
    #[serde(default)]
    pub recent_failures: usize,
    #[serde(default)]
    pub recent_timeouts: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_on_pressure: Option<AgentTaskResourcePressure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_recent_failures: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_recent_timeouts: Option<usize>,
}

impl Default for AgentTaskAdaptiveConcurrencyPolicy {
    fn default() -> Self {
        Self {
            min_concurrency: default_adaptive_min_concurrency(),
            max_concurrency: None,
            runner_capacity: None,
            active_leases: 0,
            queue_depth: None,
            resource_pressure: None,
            recent_failures: 0,
            recent_timeouts: 0,
            pause_on_pressure: None,
            pause_after_recent_failures: None,
            pause_after_recent_timeouts: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskResourcePressure {
    Low,
    Normal,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskResourceBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_units: Option<u32>,
    #[serde(default = "default_task_resource_units")]
    pub default_task_units: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_executor_task_units: HashMap<String, u32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub per_model_task_units: HashMap<String, u32>,
}

impl Default for AgentTaskResourceBudget {
    fn default() -> Self {
        Self {
            max_active_units: None,
            default_task_units: default_task_resource_units(),
            per_executor_task_units: HashMap::new(),
            per_model_task_units: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskRetryPolicy {
    #[serde(default)]
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retryable_failure_classifications: Vec<AgentTaskFailureClassification>,
}

/// Limits for provider process executions per task. The total is authoritative
/// across same-provider retries and cross-provider rotations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutionBudget {
    #[serde(default)]
    pub version: u32,
    /// Absolute UTC Unix timestamp in milliseconds at which the entire task
    /// lifecycle must stop. Unlike `timeout_ms`, this is not reset for retries,
    /// provider rotation, or a remote runner handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_unix_ms: Option<u64>,
    pub max_provider_executions: u32,
    pub max_same_provider_retries: u32,
    pub max_provider_rotations: u32,
}

impl Default for AgentTaskExecutionBudget {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: u32::MAX,
            max_same_provider_retries: u32::MAX,
            max_provider_rotations: u32::MAX,
        }
    }
}

fn legacy_execution_budget() -> AgentTaskExecutionBudget {
    AgentTaskExecutionBudget {
        version: 0,
        ..AgentTaskExecutionBudget::default()
    }
}

impl AgentTaskExecutionBudget {
    pub const VERSION: u32 = 1;

    pub fn new(
        max_provider_executions: u32,
        max_same_provider_retries: u32,
        max_provider_rotations: u32,
    ) -> Self {
        Self {
            version: Self::VERSION,
            deadline_unix_ms: None,
            max_provider_executions,
            max_same_provider_retries,
            max_provider_rotations,
        }
    }

    /// Remaining total lifecycle budget at `now_unix_ms`. `Some(0)` means the
    /// absolute deadline has expired; `None` preserves legacy unbounded plans.
    pub fn remaining_deadline_ms(&self, now_unix_ms: u64) -> Option<u64> {
        self.deadline_unix_ms
            .map(|deadline| deadline.saturating_sub(now_unix_ms))
    }

    pub fn migrate_legacy(&mut self) -> std::result::Result<bool, String> {
        match self.version {
            Self::VERSION => Ok(false),
            0 => {
                self.version = Self::VERSION;
                Ok(true)
            }
            version => Err(format!(
                "unsupported agent-task execution budget version {version}; this Homeboy build supports version {}",
                Self::VERSION
            )),
        }
    }
}

/// Operator-configured provider rotation policy for agent-task execution.
///
/// On a rotation-eligible failure (provider capacity classifications only:
/// `provider`, `transient`, `timeout`, `stalled`, `rate_limited`), the
/// scheduler re-dispatches the same task contract with the next entry in the
/// chain until entries are exhausted or the attempt bound is reached.
/// Task-level failures (`execution_failed`, `policy_denied`,
/// `invalid_input`, `capability_missing`) never rotate so a provider swap
/// cannot mask a real task failure or policy denial. The policy is pure
/// operator data — core hardcodes no provider or model names (#6978).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRotationPolicy {
    /// Ordered rotation chain. The first attempt always uses the task's own
    /// executor; entry N handles the (N+1)-th attempt after a
    /// rotation-eligible failure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<AgentTaskProviderRotationEntry>,
    /// Bound on total dispatch attempts per task (including the first).
    /// Defaults to `entries.len() + 1` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    /// Per-attempt liveness deadline: if the provider process produces no
    /// stdout/stderr progress within this window, the attempt is killed and
    /// treated as a rotation-eligible stall. When unset, attempts only obey
    /// the wall-clock `timeout_ms` / `max_runtime_ms` limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness_timeout_ms: Option<u64>,
}

impl AgentTaskProviderRotationPolicy {
    /// Total dispatch attempts allowed per task, including the first
    /// attempt on the task's own executor.
    pub fn max_total_attempts(&self) -> u32 {
        self.max_attempts
            .unwrap_or_else(|| self.entries.len() as u32 + 1)
            .max(1)
    }
}

/// Controller-compiled provider execution policy carried across a Lab handoff.
/// Runner-local configuration may satisfy this policy's capabilities and secrets,
/// but must not select a different provider policy.
///
/// Lives beside its `AgentTaskProviderRotationPolicy` / `AgentTaskRetryPolicy`
/// fields in this leaf module (rather than in `agent_task_dispatch_service`) so
/// the lab-contract type layer can depend on it without pulling in the dispatch
/// service machinery. `agent_task_dispatch_service` re-exports it for stability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedAgentTaskProviderPolicy {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<AgentTaskProviderRotationPolicy>,
    /// Whether the resolved rotation's first entry is the initial provider
    /// attempt, rather than a fallback after the request's executor.
    #[serde(default)]
    pub rotation_starts_with_first_entry: bool,
    pub retry: AgentTaskRetryPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness_timeout_ms: Option<u64>,
    /// Controller-selected runtime identity. Lab consumes this opaque identity
    /// instead of reapplying local extension discovery precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_identity: Option<ResolvedAgentTaskRuntimeIdentity>,
}

/// Immutable runtime identity selected by the controller for an agent-task
/// provider. `source_selector` is deliberately explicit so a collision is
/// observable in handoff evidence rather than hidden by local precedence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedAgentTaskRuntimeIdentity {
    pub runtime_id: String,
    pub provider_id: String,
    pub source_selector: String,
    pub source_revision: String,
    pub freshness: ResolvedAgentTaskRuntimeFreshness,
    /// Controller-resolved provider definition. This opaque payload prevents a
    /// Lab runner from applying its own extension discovery precedence.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provider: Value,
    /// Controller-resolved materialization contract for the selected provider.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub materialization_plan: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedAgentTaskRuntimeFreshness {
    Pinned,
    Unverifiable,
}

/// One rotation target: executor selector overrides and/or nested provider
/// config/model, mirroring the dispatch config layer shapes
/// (`--dispatch-selector` / `--dispatch-provider-config`). Unset fields
/// inherit the values from the failing attempt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRotationEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// JSON object merged over the executor's provider config (same shape
    /// as `--dispatch-provider-config` / `--provider-config`).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provider_config: Value,
    /// Explicitly opt a later provider into continuing one verified patch
    /// candidate. The scheduler never adopts a candidate implicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adoption: Option<AgentTaskCandidateAdoption>,
}

/// Immutable identity for a patch handed from one provider attempt to the
/// next. Content is resolved from the selected run artifact at dispatch
/// time, rather than persisted in rotation policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCandidateAdoption {
    #[serde(skip)]
    pub source_run_id: String,
    #[serde(skip)]
    pub source_task_id: String,
    #[serde(skip)]
    pub source_attempt: u32,
    #[serde(skip)]
    pub provider_backend: String,
    #[serde(skip)]
    pub provider_selector: Option<String>,
    #[serde(skip)]
    pub provider_model: Option<String>,
    #[serde(skip)]
    pub task_base_sha: String,
    #[serde(skip)]
    pub repository_identity: String,
    #[serde(skip)]
    pub workspace_identity: String,
    #[serde(skip)]
    pub artifact_id: String,
    #[serde(skip)]
    pub sha256: String,
    pub decision: AgentTaskCandidateAdoptionDecision,
    /// Scheduler-populated content resolved from the selected artifact.
    /// This is never accepted from or serialized into rotation policy.
    #[serde(skip)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskCandidateAdoptionDecision {
    /// At rotation time, select exactly one actionable patch from the
    /// immediately preceding provider attempt. No future artifact ID or
    /// digest appears in static policy.
    AdoptPreviousCandidate,
}

/// Durable evidence for one dispatch attempt of a task under a provider
/// rotation policy. Recorded in order on the final outcome under
/// `metadata.provider_rotation.attempts` so run records and
/// `agent-task status|logs` show which provider/model handled each attempt
/// and why failed attempts rotated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRotationAttempt {
    pub attempt: u32,
    /// Number of rotation entries consumed before this attempt
    /// (0 = the task's original executor).
    pub rotation_index: usize,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: AgentTaskOutcomeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<AgentTaskFailureClassification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

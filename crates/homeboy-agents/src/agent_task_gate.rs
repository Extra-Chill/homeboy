use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use homeboy_core::gate::{
    HomeboyGateKind, HomeboyGateResult, HomeboyGateRevealPolicy, HomeboyGateStatus,
    HomeboyGateVisibility,
};
use homeboy_core::plan::{PlanStep, PlanStepStatus, PlanValues};
use homeboy_core::{Error, Result};

// `Skipped` is a new durable terminal state with a structured blocker. Keep it
// out of v1 so typed consumers can distinguish the expanded state machine.
pub const AGENT_TASK_GATE_REPORT_SCHEMA: &str = "homeboy/agent-task-gate-report/v2";
const XDG_ENV_VARS: &[&str] = &[
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
];

/// Temp-dir variables pinned to the invocation temp dir so every gate process
/// — preflight probes included — shares one temporary root.
const TMPDIR_ENV_VARS: &[&str] = &["TMPDIR", "TEMP", "TMP"];

pub type AgentTaskGateVisibility = HomeboyGateVisibility;
pub type AgentTaskGateRevealPolicy = HomeboyGateRevealPolicy;

pub(crate) struct GateSupervision {
    pub timeout: Duration,
    pub heartbeat_interval: Duration,
    pub on_spawn: Arc<dyn Fn(u32, &str) -> Result<()> + Send + Sync>,
    pub on_heartbeat: Arc<dyn Fn(&str) -> Result<()> + Send + Sync>,
    pub is_cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// Shared deterministic-gate verification fields used by every agent-task
/// options/report type that runs `--verify` / `--private-verify` gates. Embed
/// this via `#[serde(flatten)]` so the serialized JSON keeps the historical
/// flat `verify` / `private_verify` / `private_gate_reveal` shape while the
/// field group lives in exactly one place.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyGateOptions {
    /// Deterministic verification commands run after each promotion/apply.
    #[serde(default)]
    pub verify: Vec<String>,
    /// Private deterministic verification commands whose full failure detail is
    /// gated from follow-up agents.
    #[serde(default)]
    pub private_verify: Vec<String>,
    /// Feedback policy for failed private gates.
    #[serde(default = "default_private_gate_reveal")]
    pub private_gate_reveal: AgentTaskGateRevealPolicy,
    /// Ordered gates stop after the first failure unless exhaustive verification
    /// is explicitly requested. Persist this policy with Cook recipes.
    #[serde(default)]
    pub execution_policy: AgentTaskGateExecutionPolicy,
    /// Maximum duration for each deterministic gate. Persisted in cook recipes
    /// so adoption never silently changes its historical verification policy.
    #[serde(default = "default_gate_timeout_seconds")]
    pub gate_timeout_seconds: u64,
    /// Cadence for durable liveness and bounded output-tail updates.
    #[serde(default = "default_gate_heartbeat_interval_seconds")]
    pub gate_heartbeat_interval_seconds: u64,
    /// Completed adoption gates are reused by default; a recipe must opt in to
    /// rerunning them after restart.
    #[serde(default)]
    pub rerun_completed_gates: bool,
    /// Declarative, non-secret process environment policy for every gate.
    #[serde(default)]
    pub gate_environment: AgentTaskGateEnvironmentPolicy,
    /// Required tools initialized in the final isolated environment before a
    /// provider can spend an execution budget.
    #[serde(default)]
    pub gate_toolchains: Vec<AgentTaskGateToolchainRequirement>,
    /// Explicit mappings for producer-owned diagnostic sidecars. Homeboy only
    /// consumes declared schemas and paths; producer semantics remain opaque.
    #[serde(default)]
    pub gate_diagnostic_sidecars: Vec<AgentTaskGateDiagnosticSidecarMapping>,
}

/// Scheduling policy for a declared sequence of deterministic gates.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateExecutionPolicy {
    /// Each gate is a prerequisite for subsequent gates.
    #[default]
    OrderedFailFast,
    /// Run every gate even after failures, for independent or exhaustive suites.
    ContinueAll,
}

/// Whether a gate starts with the caller's environment or an empty one.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateEnvironmentMode {
    #[default]
    Inherit,
    Replace,
}

/// A portable gate environment contract. `variables` are deliberate non-secret
/// inputs; secrets remain outside reports and this policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateEnvironmentPolicy {
    #[serde(default)]
    pub mode: AgentTaskGateEnvironmentMode,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Explicit host environment sources retained for a required toolchain.
    /// A source may include a relative suffix, for example `HOME/.toolchain`.
    #[serde(default)]
    pub preserve: BTreeMap<String, String>,
    #[serde(default)]
    pub isolate_home: bool,
    #[serde(default)]
    pub isolate_xdg: bool,
}

/// A required executable and its non-mutating initialization probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateToolchainRequirement {
    pub command: String,
    #[serde(default = "default_toolchain_probe_arguments")]
    pub probe_arguments: Vec<String>,
}

fn default_toolchain_probe_arguments() -> Vec<String> {
    vec!["--version".to_string()]
}

impl Default for AgentTaskGateEnvironmentPolicy {
    fn default() -> Self {
        Self {
            mode: AgentTaskGateEnvironmentMode::Inherit,
            variables: BTreeMap::new(),
            preserve: BTreeMap::new(),
            isolate_home: true,
            isolate_xdg: true,
        }
    }
}

fn default_private_gate_reveal() -> AgentTaskGateRevealPolicy {
    AgentTaskGateRevealPolicy::SummaryOnly
}

fn default_gate_timeout_seconds() -> u64 {
    30 * 60
}
fn default_gate_heartbeat_interval_seconds() -> u64 {
    5
}

impl VerifyGateOptions {
    pub fn gate_timeout(&self) -> Duration {
        Duration::from_secs(self.gate_timeout_seconds.max(1))
    }

    pub fn gate_heartbeat_interval(&self) -> Duration {
        Duration::from_secs(self.gate_heartbeat_interval_seconds.max(1))
    }

    /// Commands already declared as gates are toolchain requirements too. This
    /// protects existing Cook invocations without duplicate CLI ceremony.
    pub(crate) fn required_toolchains(&self) -> Vec<AgentTaskGateToolchainRequirement> {
        let mut requirements = self.gate_toolchains.clone();
        for gate in self.verify.iter().chain(&self.private_verify) {
            let Some(command) = gate.split_whitespace().next() else {
                continue;
            };
            if !requirements
                .iter()
                .any(|requirement| requirement.command == command)
            {
                requirements.push(AgentTaskGateToolchainRequirement {
                    command: command.to_string(),
                    probe_arguments: default_toolchain_probe_arguments(),
                });
            }
        }
        requirements
    }
}

impl Default for VerifyGateOptions {
    fn default() -> Self {
        Self {
            verify: Vec::new(),
            private_verify: Vec::new(),
            private_gate_reveal: default_private_gate_reveal(),
            execution_policy: AgentTaskGateExecutionPolicy::OrderedFailFast,
            gate_timeout_seconds: default_gate_timeout_seconds(),
            gate_heartbeat_interval_seconds: default_gate_heartbeat_interval_seconds(),
            rerun_completed_gates: false,
            gate_environment: AgentTaskGateEnvironmentPolicy::default(),
            gate_toolchains: Vec::new(),
            gate_diagnostic_sidecars: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskGateReport {
    #[serde(default = "gate_report_schema")]
    pub schema: String,
    #[serde(skip, default = "default_gate_step")]
    pub step: PlanStep,
    pub id: String,
    #[serde(default)]
    pub visibility: AgentTaskGateVisibility,
    #[serde(default)]
    pub reveal_policy: AgentTaskGateRevealPolicy,
    pub status: AgentTaskGateStatus,
    pub command: Vec<String>,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_evidence: Option<AgentTaskGateFailureEvidence>,
    /// Why this declared gate was not invoked. This remains durable evidence so
    /// downstream consumers do not have to infer skipped work from a missing row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<AgentTaskGateSkipReason>,
    /// The same command's result against the immutable base when candidate
    /// adoption needs to distinguish inherited failures from regressions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<AgentTaskGateBaselineComparison>,
    /// Immutable checkout identity used for this gate when verification runs
    /// against a materialized candidate rather than the promotion destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_checkout: Option<AgentTaskGateCandidateCheckout>,
    #[serde(default, skip_serializing_if = "AgentTaskGateEnvironment::is_empty")]
    pub environment: AgentTaskGateEnvironment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCandidateCheckout {
    pub schema: String,
    pub commit: String,
    pub tree: String,
    pub candidate_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateBaselineComparison {
    pub base_ref: String,
    pub exit_code: i32,
    pub failure_fingerprint: String,
    pub matches_candidate_failure: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateEnvironment {
    #[serde(default)]
    pub mode: AgentTaskGateEnvironmentMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited: Vec<AgentTaskGateEnvironmentVariable>,
    /// Explicit source mappings retained from the host for required tools.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub preserved: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sanitized: Vec<AgentTaskGateEnvironmentVariable>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateEnvironmentVariable {
    pub name: String,
    pub value: String,
}

impl AgentTaskGateEnvironment {
    fn is_empty(&self) -> bool {
        self.mode == AgentTaskGateEnvironmentMode::Inherit
            && self.inherited.is_empty()
            && self.preserved.is_empty()
            && self.sanitized.is_empty()
    }

    pub(crate) fn replay_policy(&self) -> AgentTaskGateEnvironmentPolicy {
        let variables = self
            .inherited
            .iter()
            .map(|variable| (variable.name.clone(), variable.value.clone()))
            .collect();
        AgentTaskGateEnvironmentPolicy {
            mode: self.mode,
            variables,
            preserve: self.preserved.clone(),
            isolate_home: self
                .sanitized
                .iter()
                .any(|variable| variable.name == "HOME"),
            isolate_xdg: self
                .sanitized
                .iter()
                .any(|variable| XDG_ENV_VARS.contains(&variable.name.as_str())),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateStatus {
    Succeeded,
    Failed,
    Skipped,
    /// The candidate command failed, but the identical failure was reproduced
    /// against the controller-recorded immutable baseline.
    AcceptedInheritedFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateSkipReason {
    pub blocking_gate_id: String,
    pub reason: String,
}

/// Canonical bridge from the binary agent-task gate status to the shared
/// `HomeboyGateStatus`. Both the report constructor and the
/// `HomeboyGateResult` conversion route through this single mapping so the
/// pass/fail projection cannot drift between call sites.
impl From<AgentTaskGateStatus> for HomeboyGateStatus {
    fn from(status: AgentTaskGateStatus) -> Self {
        match status {
            AgentTaskGateStatus::Succeeded => HomeboyGateStatus::Passed,
            AgentTaskGateStatus::Failed => HomeboyGateStatus::Failed,
            AgentTaskGateStatus::Skipped => HomeboyGateStatus::Skipped,
            AgentTaskGateStatus::AcceptedInheritedFailure => HomeboyGateStatus::Passed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateFailureEvidence {
    pub summary: String,
    pub command: String,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout_tail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr_tail: String,
    pub agent_feedback: String,
    /// Producer-owned structured diagnostics carried in the gate sidecar.
    /// Homeboy treats their locations and suggested actions as opaque data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskGateDiagnosticRecord>,
}

pub const AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA: &str = "homeboy/gate-diagnostic-record/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateDiagnosticRecord {
    #[serde(default = "gate_diagnostic_record_schema")]
    pub schema: String,
    pub identity: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_actions: Vec<String>,
    pub producer: AgentTaskGateDiagnosticProducer,
    pub full_evidence_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateDiagnosticProducer {
    pub id: String,
    pub schema: String,
}

/// A producer-declared sidecar contract mapped to Homeboy's normalized record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateDiagnosticSidecarMapping {
    pub source_schema: String,
    #[serde(default = "gate_diagnostic_record_schema")]
    pub target_schema: String,
    pub path: String,
    pub producer: AgentTaskGateDiagnosticProducer,
}

const MAX_GATE_DIAGNOSTICS: usize = 8;
const MAX_GATE_DIAGNOSTIC_FIELD_BYTES: usize = 512;
const MAX_GATE_DIAGNOSTIC_ACTIONS: usize = 4;
const MAX_GATE_DIAGNOSTIC_SIDECAR_BYTES: u64 = 64 * 1024;

/// Best-effort compatibility ingestion for a declared producer sidecar. Missing
/// or malformed sidecars never replace the command's authoritative gate result.
pub(crate) fn ingest_gate_diagnostic_sidecars(
    cwd: &Path,
    mappings: &[AgentTaskGateDiagnosticSidecarMapping],
    report: &mut AgentTaskGateReport,
    full_evidence_ref: &str,
) {
    let Some(evidence) = report.failure_evidence.as_mut() else {
        return;
    };
    for mapping in mappings {
        if evidence.diagnostics.len() >= MAX_GATE_DIAGNOSTICS
            || mapping.target_schema != AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA
            || mapping.source_schema.trim().is_empty()
            || mapping.producer.id.trim().is_empty()
            || mapping.producer.schema.trim().is_empty()
        {
            continue;
        }
        let Ok(path) = homeboy_core::resolve_contained_local_path(
            cwd,
            &mapping.path,
            "gate_diagnostic_sidecars.path",
        ) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_GATE_DIAGNOSTIC_SIDECAR_BYTES {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(records) = serde_json::from_str::<Vec<serde_json::Value>>(&body) else {
            continue;
        };
        for value in records {
            if evidence.diagnostics.len() >= MAX_GATE_DIAGNOSTICS {
                break;
            }
            let Some(record) = normalize_gate_diagnostic_record(value, mapping, full_evidence_ref)
            else {
                continue;
            };
            if !evidence.diagnostics.iter().any(|existing| {
                existing.identity == record.identity && existing.producer == record.producer
            }) {
                evidence.diagnostics.push(record);
            }
        }
    }
}

fn normalize_gate_diagnostic_record(
    value: serde_json::Value,
    mapping: &AgentTaskGateDiagnosticSidecarMapping,
    full_evidence_ref: &str,
) -> Option<AgentTaskGateDiagnosticRecord> {
    let object = value.as_object()?;
    if object.get("schema")?.as_str()? != mapping.source_schema {
        return None;
    }
    let identity = bounded_diagnostic_text(object.get("identity")?.as_str()?);
    let summary = bounded_diagnostic_text(object.get("summary")?.as_str()?);
    if identity.is_empty() || summary.is_empty() {
        return None;
    }
    let source_location = object
        .get("source_location")
        .and_then(serde_json::Value::as_str)
        .map(bounded_diagnostic_text)
        .filter(|value| !value.is_empty());
    let suggested_actions = object
        .get("suggested_actions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(bounded_diagnostic_text)
        .filter(|value| !value.is_empty())
        .take(MAX_GATE_DIAGNOSTIC_ACTIONS)
        .collect();
    Some(AgentTaskGateDiagnosticRecord {
        schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
        identity,
        summary,
        source_location,
        suggested_actions,
        producer: mapping.producer.clone(),
        full_evidence_ref: full_evidence_ref.to_string(),
    })
}

fn bounded_diagnostic_text(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        if result.len() + character.len_utf8() > MAX_GATE_DIAGNOSTIC_FIELD_BYTES {
            break;
        }
        result.push(character);
    }
    result
}

fn gate_diagnostic_record_schema() -> String {
    AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string()
}

impl AgentTaskGateReport {
    pub fn new(
        id: impl Into<String>,
        command: Vec<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        failure_evidence: Option<AgentTaskGateFailureEvidence>,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
        environment: AgentTaskGateEnvironment,
    ) -> Self {
        let id = id.into();
        let status = if exit_code == 0 {
            AgentTaskGateStatus::Succeeded
        } else {
            AgentTaskGateStatus::Failed
        };
        let gate_result = HomeboyGateResult::new(
            id.clone(),
            id.clone(),
            HomeboyGateKind::Command,
            HomeboyGateStatus::from(status),
        )
        .visibility(visibility)
        .reveal_policy(reveal_policy)
        .retryable(status == AgentTaskGateStatus::Failed);
        let step = PlanStep::builder(
            id.clone(),
            "agent_task.gate",
            match status {
                AgentTaskGateStatus::Succeeded | AgentTaskGateStatus::AcceptedInheritedFailure => {
                    PlanStepStatus::Success
                }
                AgentTaskGateStatus::Failed => PlanStepStatus::Failed,
                AgentTaskGateStatus::Skipped => PlanStepStatus::Skipped,
            },
        )
        .inputs(PlanValues::new().json("command", &command))
        .output_value("exit_code", serde_json::json!(exit_code))
        .gate_result(gate_result)
        .build();

        Self {
            schema: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
            step,
            id,
            visibility,
            reveal_policy,
            status,
            command,
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            failure_evidence,
            skip_reason: None,
            baseline_comparison: None,
            candidate_checkout: None,
            environment,
        }
    }

    pub fn skipped(
        id: impl Into<String>,
        command: Vec<String>,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
        blocking_gate_id: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let skip_reason = AgentTaskGateSkipReason {
            blocking_gate_id: blocking_gate_id.into(),
            reason: "ordered_fail_fast".to_string(),
        };
        let gate_result = HomeboyGateResult::new(
            id.clone(),
            id.clone(),
            HomeboyGateKind::Command,
            HomeboyGateStatus::Skipped,
        )
        .summary(format!(
            "deterministic gate skipped after prerequisite {} failed",
            skip_reason.blocking_gate_id
        ))
        .visibility(visibility)
        .reveal_policy(reveal_policy)
        .retryable(false);
        let step = PlanStep::builder(id.clone(), "agent_task.gate", PlanStepStatus::Skipped)
            .inputs(PlanValues::new().json("command", &command))
            .output_value("skip_reason", serde_json::json!(&skip_reason))
            .gate_result(gate_result)
            .build();
        Self {
            schema: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
            step,
            id,
            visibility,
            reveal_policy,
            status: AgentTaskGateStatus::Skipped,
            command,
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            failure_evidence: None,
            skip_reason: Some(skip_reason),
            baseline_comparison: None,
            candidate_checkout: None,
            environment: AgentTaskGateEnvironment::default(),
        }
    }

    pub(crate) fn accept_inherited_failure(&mut self) {
        self.status = AgentTaskGateStatus::AcceptedInheritedFailure;
        let gate_result = HomeboyGateResult::new(
            self.id.clone(),
            self.id.clone(),
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        )
        .summary(
            "candidate failure matches the immutable baseline; no candidate regression detected",
        )
        .visibility(self.visibility)
        .reveal_policy(self.reveal_policy)
        .retryable(false);
        self.step = PlanStep::builder(self.id.clone(), "agent_task.gate", PlanStepStatus::Success)
            .inputs(PlanValues::new().json("command", &self.command))
            .output_value("exit_code", serde_json::json!(self.exit_code))
            .output_value("accepted_inherited_failure", serde_json::json!(true))
            .gate_result(gate_result)
            .build();
    }
}

/// Normalize only transport-noise that cannot identify a command failure. The
/// comparison remains fail-closed: a changed substantive line is a regression.
pub(crate) fn failure_fingerprint(stdout: &str, stderr: &str) -> String {
    [stdout, stderr]
        .into_iter()
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod baseline_tests {
    use super::{
        failure_fingerprint, run_gate_command_with_timeout, AgentTaskGateEnvironmentPolicy,
        AgentTaskGateRevealPolicy, AgentTaskGateStatus, AgentTaskGateVisibility,
    };
    use std::time::Duration;

    #[test]
    fn matching_baseline_failure_is_distinct_from_a_new_failure() {
        let baseline = failure_fingerprint("test alpha ... FAILED\n", "");
        let matching_candidate = failure_fingerprint("test alpha ... FAILED\n", "");
        let regressed_candidate =
            failure_fingerprint("test alpha ... FAILED\ntest beta ... FAILED\n", "");

        assert_eq!(baseline, matching_candidate);
        assert_ne!(baseline, regressed_candidate);
    }

    #[test]
    fn bounded_baseline_gate_is_cancelled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let report = run_gate_command_with_timeout(
            temp.path(),
            1,
            "sleep 1",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            temp.path(),
            Duration::from_millis(20),
            &AgentTaskGateEnvironmentPolicy::default(),
        )
        .expect("bounded gate report");

        assert_eq!(report.status, AgentTaskGateStatus::Failed);
        assert_eq!(report.exit_code, 124);
        assert!(report.stderr.contains("was cancelled"));
    }

    #[test]
    fn bounded_baseline_gate_reaps_background_descendants_before_reader_join() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("descendant-survived");
        let report = run_gate_command_with_timeout(
            temp.path(),
            1,
            &format!(
                "(sleep 0.2; touch '{}') & while :; do sleep 1; done",
                marker.display()
            ),
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            temp.path(),
            Duration::from_millis(20),
            &AgentTaskGateEnvironmentPolicy::default(),
        )
        .expect("bounded gate report");

        assert_eq!(report.exit_code, 124);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "background descendant survived timeout");
    }
}

pub(crate) fn run_gate_command(
    cwd: &Path,
    index: usize,
    command: &str,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy(
        cwd,
        index,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
    )
}

pub(crate) fn run_gate_command_with_policy(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy_and_runtime_tmpdir(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        None,
    )
}

pub(crate) fn run_gate_command_with_policy_and_runtime_tmpdir(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        &AgentTaskGateEnvironmentPolicy::default(),
    )
}

pub(crate) fn run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_supervision(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        None,
        gate_environment,
    )
}

pub(crate) fn run_gate_command_with_supervision(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
    supervision: Option<&GateSupervision>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
) -> Result<AgentTaskGateReport> {
    let command_vec = vec!["sh".to_string(), "-lc".to_string(), command.to_string()];
    let mut process = Command::new(&command_vec[0]);
    process
        .args(&command_vec[1..])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let selected_environment = selected_gate_environment(gate_environment, runtime_tmpdir)?;
    selected_environment.apply(&mut process);
    if supervision.is_some() {
        if !homeboy_core::engine::command::supports_process_tree_isolation() {
            return Err(Error::validation_invalid_argument(
                "gate_supervision",
                "durable gate cancellation requires Unix process-group isolation on this host",
                None,
                None,
            ));
        }
        homeboy_core::engine::command::isolate_process_tree(&mut process);
    }
    let mut child = process.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run deterministic gate {command}")),
        )
    })?;
    let output = if let Some(supervision) = supervision {
        if let Err(error) = (supervision.on_spawn)(child.id(), command) {
            if let Err(cleanup_error) =
                homeboy_core::engine::command::terminate_process_tree_and_reap(&mut child)
            {
                return Err(Error::internal_io(
                    format!(
                        "durable gate registration failed ({error}); failed to terminate and reap its child: {cleanup_error}"
                    ),
                    Some(format!("supervise deterministic gate {command}")),
                ));
            }
            return Err(error);
        }
        let supervised = homeboy_core::engine::command::wait_with_bounded_output_supervised(
            &mut child,
            65_536,
            supervision.timeout,
            supervision.heartbeat_interval,
            || (supervision.is_cancelled)(),
            |_, tail| {
                // Durable status must never become a private-gate output channel.
                let tail = if visibility == AgentTaskGateVisibility::Private {
                    "private gate output withheld"
                } else {
                    tail
                };
                (supervision.on_heartbeat)(tail).map_err(|error| {
                    std::io::Error::other(format!("persist deterministic gate heartbeat: {error}"))
                })
            },
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("supervise deterministic gate {command}")),
            )
        })?;
        let mut output = supervised.output.into_output();
        if supervised.termination
            == homeboy_core::engine::command::SupervisedCommandTermination::TimedOut
        {
            output
                .stderr
                .extend_from_slice(b"\nHomeboy terminated this gate after its policy timeout.\n");
        }
        output
    } else {
        child.wait_with_output().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("run deterministic gate {command}")),
            )
        })?
    };
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let failure_evidence = (!output.status.success())
        .then(|| gate_failure_evidence(command, exit_code, &stdout, &stderr));

    Ok(AgentTaskGateReport::new(
        format!("gate-{index}"),
        command_vec,
        exit_code,
        stdout,
        stderr,
        failure_evidence,
        visibility,
        reveal_policy,
        selected_environment.report,
    ))
}

/// Run a comparison gate with a hard wall-clock limit. The bounded path keeps
/// candidate adoption inspectable instead of allowing a known-red baseline to
/// consume an unbounded second broad-suite run.
pub(crate) fn run_gate_command_with_timeout(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: &Path,
    timeout: Duration,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
) -> Result<AgentTaskGateReport> {
    let command_vec = vec!["sh".to_string(), "-lc".to_string(), command.to_string()];
    let mut process = Command::new("sh");
    process
        .args(["-lc", command])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let selected_environment = selected_gate_environment(gate_environment, Some(runtime_tmpdir))?;
    selected_environment.apply(&mut process);
    homeboy_core::engine::command::isolate_process_tree(&mut process);
    let mut child = process.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run bounded deterministic gate {command}")),
        )
    })?;
    let started = std::time::Instant::now();
    let mut timed_out = false;
    let output = homeboy_core::engine::command::wait_with_bounded_output_until_cancelled(
        &mut child,
        1024 * 1024,
        || {
            timed_out = started.elapsed() >= timeout;
            timed_out
        },
    )
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run bounded deterministic gate {command}")),
        )
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = if timed_out {
        stderr.push_str(&format!(
            "\nbaseline gate exceeded {} ms and was cancelled",
            timeout.as_millis()
        ));
        124
    } else {
        output.status.code().unwrap_or(1)
    };
    let failure_evidence =
        (exit_code != 0).then(|| gate_failure_evidence(command, exit_code, &stdout, &stderr));
    Ok(AgentTaskGateReport::new(
        format!("gate-{index}"),
        command_vec,
        exit_code,
        stdout,
        stderr,
        failure_evidence,
        visibility,
        reveal_policy,
        selected_environment.report,
    ))
}

struct SelectedGateEnvironment {
    report: AgentTaskGateEnvironment,
    values: BTreeMap<String, String>,
    _scratch: Option<tempfile::TempDir>,
}

impl SelectedGateEnvironment {
    fn apply(&self, process: &mut Command) {
        if self.report.mode == AgentTaskGateEnvironmentMode::Replace {
            process.env_clear();
        }
        for variable in &self.report.sanitized {
            process.env_remove(&variable.name);
        }
        process.envs(&self.values);
    }
}

fn selected_gate_environment(
    policy: &AgentTaskGateEnvironmentPolicy,
    runtime_tmpdir: Option<&Path>,
) -> Result<SelectedGateEnvironment> {
    let mut report = AgentTaskGateEnvironment {
        mode: policy.mode,
        ..AgentTaskGateEnvironment::default()
    };
    let mut values = policy.variables.clone();
    for (name, value) in &policy.variables {
        report.inherited.push(AgentTaskGateEnvironmentVariable {
            name: name.clone(),
            value: value.clone(),
        });
    }
    for (name, source) in &policy.preserve {
        let value = preserved_environment_value(source)?;
        values.insert(name.clone(), value);
        report.preserved.insert(name.clone(), source.clone());
    }

    // The invocation temp dir belongs to the shared environment definition, not
    // to individual call sites. Preflight and execution must agree on it, or
    // preflight validates a different directory than the gate will use — and,
    // under the default inherit mode, silently falls back to the ambient TMPDIR
    // or fails outright when the host has none. (#10245)
    if let Some(runtime_tmpdir) = runtime_tmpdir {
        let runtime_tmpdir = runtime_tmpdir.display().to_string();
        for name in TMPDIR_ENV_VARS {
            values.insert((*name).to_string(), runtime_tmpdir.clone());
        }
    }

    let needs_scratch = policy.isolate_home || policy.isolate_xdg;
    let scratch = if needs_scratch && runtime_tmpdir.is_none() {
        Some(tempfile::tempdir().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create isolated gate environment".to_string()),
            )
        })?)
    } else {
        None
    };
    let root = runtime_tmpdir.or_else(|| scratch.as_ref().map(|dir| dir.path()));
    if policy.isolate_home {
        let root = root.expect("isolated gate environment scratch root");
        let home = root.join("gate-home");
        fs::create_dir_all(&home).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", home.display())),
            )
        })?;
        set_isolated_environment_variable(&mut report, &mut values, "HOME", home);
    }
    if policy.isolate_xdg {
        let root = root.expect("isolated gate environment scratch root");
        for name in XDG_ENV_VARS {
            let path = root.join("gate-xdg").join(name.to_ascii_lowercase());
            fs::create_dir_all(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create {}", path.display())),
                )
            })?;
            set_isolated_environment_variable(&mut report, &mut values, name, path);
        }
    }
    Ok(SelectedGateEnvironment {
        report,
        values,
        _scratch: scratch,
    })
}

fn preserved_environment_value(source: &str) -> Result<String> {
    let (source_name, suffix) = source.split_once('/').unwrap_or((source, ""));
    let value = std::env::var(source_name).map_err(|_| {
        Error::validation_invalid_argument(
            "gate_environment.preserve",
            format!("declared gate environment source {source_name} is unavailable"),
            Some(source.to_string()),
            Some(vec![format!(
                "Set {source_name} before Cook or replace this mapping with an available toolchain home."
            )]),
        )
    })?;
    Ok(if suffix.is_empty() {
        value
    } else {
        PathBuf::from(value).join(suffix).display().to_string()
    })
}

/// Validate declared tools in the exact environment candidate gates will use.
/// This is deliberately generic: callers declare executables and environment
/// mappings while extensions own language-specific discovery.
pub(crate) fn preflight_gate_toolchains(
    cwd: &Path,
    policy: &AgentTaskGateEnvironmentPolicy,
    requirements: &[AgentTaskGateToolchainRequirement],
    runtime_tmpdir: Option<&Path>,
) -> Result<()> {
    let selected_environment = selected_gate_environment(policy, runtime_tmpdir)?;
    for requirement in requirements {
        let mut process = Command::new(&requirement.command);
        process
            .args(&requirement.probe_arguments)
            .current_dir(cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        selected_environment.apply(&mut process);
        let output = process.output().map_err(|error| {
            Error::validation_invalid_argument(
                "gate_toolchains",
                format!(
                    "gate toolchain preflight could not resolve or initialize `{}`: {error}",
                    requirement.command
                ),
                Some(requirement.command.clone()),
                Some(vec!["Declare the required toolchain environment with --gate-env-from NAME=SOURCE[/suffix], then retry Cook.".to_string()]),
            )
        })?;
        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "gate_toolchains",
                format!(
                    "gate toolchain preflight could not initialize `{}` (exit {}): {}",
                    requirement.command,
                    output.status.code().unwrap_or(1),
                    text_tail(&String::from_utf8_lossy(&output.stderr), 5),
                ),
                Some(requirement.command.clone()),
                Some(vec!["Declare the required toolchain environment with --gate-env-from NAME=SOURCE[/suffix], then retry Cook.".to_string()]),
            ));
        }
    }
    Ok(())
}

fn set_isolated_environment_variable(
    report: &mut AgentTaskGateEnvironment,
    values: &mut BTreeMap<String, String>,
    name: &str,
    path: PathBuf,
) {
    let value = path.display().to_string();
    values.insert(name.to_string(), value.clone());
    report.sanitized.push(AgentTaskGateEnvironmentVariable {
        name: name.to_string(),
        value,
    });
}

fn gate_report_schema() -> String {
    AGENT_TASK_GATE_REPORT_SCHEMA.to_string()
}

fn default_gate_step() -> PlanStep {
    PlanStep::builder("gate", "agent_task.gate", PlanStepStatus::Skipped).build()
}

fn gate_failure_evidence(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> AgentTaskGateFailureEvidence {
    let stdout_tail = text_tail(stdout, 20);
    let stderr_tail = text_tail(stderr, 20);
    let summary = format!("deterministic gate failed with exit code {exit_code}: {command}");
    let agent_feedback = format!(
        "A deterministic verification gate failed after the candidate patch was applied. Fix the code so `{command}` passes, using the captured stdout/stderr tails as the primary failure evidence."
    );

    AgentTaskGateFailureEvidence {
        summary,
        command: command.to_string(),
        exit_code,
        stdout_tail,
        stderr_tail,
        agent_feedback,
        diagnostics: Vec::new(),
    }
}

pub(crate) fn text_tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

impl From<AgentTaskGateReport> for HomeboyGateResult {
    fn from(report: AgentTaskGateReport) -> Self {
        let status = HomeboyGateStatus::from(report.status);
        let command = report.command.join(" ");
        let summary = gate_result_summary(&report, &command);
        let agent_feedback = gate_result_agent_feedback(&report);
        let evidence = gate_result_evidence(&report);

        HomeboyGateResult::new(
            report.id.clone(),
            report.id.clone(),
            HomeboyGateKind::Command,
            status,
        )
        .summary(summary)
        .evidence(evidence)
        .visibility(report.visibility)
        .reveal_policy(report.reveal_policy)
        .retryable(status == HomeboyGateStatus::Failed)
        .agent_feedback(agent_feedback)
        .provenance(json!({
            "source_schema": report.schema,
            "source_type": "AgentTaskGateReport",
        }))
    }
}

fn gate_result_summary(report: &AgentTaskGateReport, command: &str) -> String {
    if report.status == AgentTaskGateStatus::Skipped {
        let blocker = report
            .skip_reason
            .as_ref()
            .map(|reason| reason.blocking_gate_id.as_str())
            .unwrap_or("an earlier gate");
        return format!("deterministic gate skipped after prerequisite {blocker} failed");
    }
    if report.status == AgentTaskGateStatus::Failed
        && report.visibility == AgentTaskGateVisibility::Private
    {
        match report.reveal_policy {
            AgentTaskGateRevealPolicy::SummaryOnly => {
                return format!(
                    "private deterministic gate {} failed; detailed evidence is withheld by policy",
                    report.id
                );
            }
            AgentTaskGateRevealPolicy::Redacted => {
                return "private deterministic gate failed; evidence redacted".to_string();
            }
            AgentTaskGateRevealPolicy::NoDetail => {
                return "private deterministic gate failed".to_string();
            }
            AgentTaskGateRevealPolicy::FullEvidence => {}
        }
    }

    report
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.summary.clone())
        .unwrap_or_else(|| format!("deterministic gate passed: {command}"))
}

fn gate_result_agent_feedback(report: &AgentTaskGateReport) -> String {
    if report.status == AgentTaskGateStatus::Skipped {
        return String::new();
    }
    if report.status == AgentTaskGateStatus::Failed
        && report.visibility == AgentTaskGateVisibility::Private
    {
        match report.reveal_policy {
            AgentTaskGateRevealPolicy::SummaryOnly => {
                return "A private deterministic verification gate failed. Generalize the fix against the public objective and visible evidence; hidden evaluator details are withheld.".to_string();
            }
            AgentTaskGateRevealPolicy::Redacted => {
                return "A private deterministic verification gate failed. Details are redacted; continue from the public task objective and visible gate evidence.".to_string();
            }
            AgentTaskGateRevealPolicy::NoDetail => {
                return "A private deterministic verification gate failed.".to_string();
            }
            AgentTaskGateRevealPolicy::FullEvidence => {}
        }
    }

    report
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.agent_feedback.clone())
        .unwrap_or_default()
}

fn gate_result_evidence(report: &AgentTaskGateReport) -> serde_json::Value {
    if report.status == AgentTaskGateStatus::Skipped {
        return json!({ "skipped": true, "skip_reason": report.skip_reason });
    }
    if report.visibility == AgentTaskGateVisibility::Private {
        match report.reveal_policy {
            AgentTaskGateRevealPolicy::SummaryOnly => {
                return json!({
                    "exit_code": report.exit_code,
                    "withheld": true,
                    "reason": "summary_only",
                });
            }
            AgentTaskGateRevealPolicy::Redacted => {
                return json!({
                    "exit_code": report.exit_code,
                    "redacted": true,
                });
            }
            AgentTaskGateRevealPolicy::NoDetail => {
                return json!({
                    "withheld": true,
                    "reason": "no_detail",
                });
            }
            AgentTaskGateRevealPolicy::FullEvidence => {}
        }
    }

    json!({
        "command": report.command,
        "exit_code": report.exit_code,
        "stdout": report.stdout,
        "stderr": report.stderr,
        "failure_evidence": report.failure_evidence,
        "environment": report.environment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    #[test]
    fn failed_durable_spawn_registration_reaps_the_isolated_gate_group() {
        let worktree = tempfile::tempdir().expect("worktree");
        let child_pid = Arc::new(Mutex::new(None));
        let supervision = GateSupervision {
            timeout: Duration::from_secs(1),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new({
                let child_pid = Arc::clone(&child_pid);
                move |pid, _| {
                    *child_pid.lock().expect("child pid") = Some(pid);
                    Err(Error::internal_unexpected("durable write failed"))
                }
            }),
            on_heartbeat: Arc::new(|_| Ok(())),
            is_cancelled: Arc::new(|| false),
        };
        assert!(run_gate_command_with_supervision(
            worktree.path(),
            1,
            "sleep 30",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&supervision),
            &AgentTaskGateEnvironmentPolicy::default(),
        )
        .is_err());
        let pid = child_pid
            .lock()
            .expect("child pid")
            .expect("durable registration received child pid");
        assert!(!homeboy_core::process::pid_is_running(pid));
    }

    #[cfg(unix)]
    #[test]
    fn supervised_gate_heartbeats_with_a_bounded_live_output_tail() {
        let worktree = tempfile::tempdir().expect("worktree");
        let tails = Arc::new(Mutex::new(Vec::new()));
        let supervision = GateSupervision {
            timeout: Duration::from_secs(1),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new(|_, _| Ok(())),
            on_heartbeat: Arc::new({
                let tails = Arc::clone(&tails);
                move |tail| {
                    tails.lock().expect("tails").push(tail.to_string());
                    Ok(())
                }
            }),
            is_cancelled: Arc::new(|| false),
        };
        run_gate_command_with_supervision(
            worktree.path(),
            1,
            "printf stdout; printf stderr >&2; sleep 0.5",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&supervision),
            &AgentTaskGateEnvironmentPolicy::default(),
        )
        .expect("gate");
        assert!(tails
            .lock()
            .expect("tails")
            .iter()
            .any(|tail| tail.contains("stdout") && tail.contains("stderr")));
    }

    #[cfg(unix)]
    #[test]
    fn supervised_private_gate_never_persists_live_output() {
        let worktree = tempfile::tempdir().expect("worktree");
        let tails = Arc::new(Mutex::new(Vec::new()));
        let supervision = GateSupervision {
            timeout: Duration::from_secs(1),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new(|_, _| Ok(())),
            on_heartbeat: Arc::new({
                let tails = Arc::clone(&tails);
                move |tail| {
                    tails.lock().expect("tails").push(tail.to_string());
                    Ok(())
                }
            }),
            is_cancelled: Arc::new(|| false),
        };
        run_gate_command_with_supervision(
            worktree.path(),
            1,
            "printf secret-stdout; printf secret-stderr >&2; sleep 0.5",
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&supervision),
            &AgentTaskGateEnvironmentPolicy::default(),
        )
        .expect("private gate");
        let tails = tails.lock().expect("tails");
        assert!(!tails.is_empty());
        assert!(tails
            .iter()
            .all(|tail| tail == "private gate output withheld"));
    }

    #[test]
    fn agent_task_gate_status_bridges_to_homeboy_gate_status() {
        assert_eq!(
            HomeboyGateStatus::from(AgentTaskGateStatus::Succeeded),
            HomeboyGateStatus::Passed
        );
        assert_eq!(
            HomeboyGateStatus::from(AgentTaskGateStatus::Failed),
            HomeboyGateStatus::Failed
        );
        assert_eq!(
            HomeboyGateStatus::from(AgentTaskGateStatus::Skipped),
            HomeboyGateStatus::Skipped
        );
    }

    #[test]
    fn declared_opaque_sidecar_is_normalized_with_durable_evidence_reference() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fixture = include_str!(
            "../../../tests/fixtures/agent_task_gate_feedback/opaque-producer-diagnostics.json"
        );
        fs::write(temp.path().join("diagnostics.json"), fixture).expect("write sidecar");
        let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");
        ingest_gate_diagnostic_sidecars(
            temp.path(),
            &[AgentTaskGateDiagnosticSidecarMapping {
                source_schema: "example/producer-diagnostic/v1".to_string(),
                target_schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
                path: "diagnostics.json".to_string(),
                producer: AgentTaskGateDiagnosticProducer {
                    id: "opaque-producer".to_string(),
                    schema: "example/producer-output/v1".to_string(),
                },
            }],
            &mut report,
            "homeboy://agent-task/run/run-1/promotion/gates/gate-1",
        );
        let diagnostic = &report.failure_evidence.unwrap().diagnostics[0];
        assert_eq!(diagnostic.schema, AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA);
        assert_eq!(diagnostic.identity, "opaque:stable-identity");
        assert_eq!(diagnostic.producer.id, "opaque-producer");
        assert_eq!(
            diagnostic.full_evidence_ref,
            "homeboy://agent-task/run/run-1/promotion/gates/gate-1"
        );
    }

    #[test]
    fn absent_or_malformed_declared_sidecars_preserve_gate_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");
        let mapping = AgentTaskGateDiagnosticSidecarMapping {
            source_schema: "example/producer-diagnostic/v1".to_string(),
            target_schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
            path: "diagnostics.json".to_string(),
            producer: AgentTaskGateDiagnosticProducer {
                id: "opaque-producer".to_string(),
                schema: "example/producer-output/v1".to_string(),
            },
        };
        ingest_gate_diagnostic_sidecars(temp.path(), &[mapping.clone()], &mut report, "evidence");
        fs::write(temp.path().join("diagnostics.json"), "not json")
            .expect("write malformed sidecar");
        ingest_gate_diagnostic_sidecars(temp.path(), &[mapping], &mut report, "evidence");
        assert!(report.failure_evidence.unwrap().diagnostics.is_empty());
    }

    #[test]
    fn verify_gate_policy_defaults_are_durable_and_backward_compatible() {
        let defaults = VerifyGateOptions::default();
        assert_eq!(defaults.gate_timeout(), Duration::from_secs(30 * 60));
        assert_eq!(defaults.gate_heartbeat_interval(), Duration::from_secs(5));
        assert!(!defaults.rerun_completed_gates);
        assert_eq!(
            defaults.execution_policy,
            AgentTaskGateExecutionPolicy::OrderedFailFast
        );

        let legacy: VerifyGateOptions = serde_json::from_value(serde_json::json!({
            "verify": ["cargo test"],
            "private_verify": [],
            "private_gate_reveal": "summary_only"
        }))
        .expect("deserialize legacy gate policy");
        assert_eq!(
            legacy,
            VerifyGateOptions {
                verify: vec!["cargo test".to_string()],
                ..VerifyGateOptions::default()
            }
        );
    }

    #[test]
    fn gate_command_reports_success_without_failure_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command(temp.path(), 1, "printf 'ok'").expect("gate report");

        assert_eq!(report.schema, AGENT_TASK_GATE_REPORT_SCHEMA);
        assert_eq!(report.id, "gate-1");
        assert_eq!(report.visibility, AgentTaskGateVisibility::Visible);
        assert_eq!(
            report.reveal_policy,
            AgentTaskGateRevealPolicy::FullEvidence
        );
        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(report.exit_code, 0);
        assert_eq!(report.stdout, "ok");
        assert!(report.failure_evidence.is_none());
    }

    #[test]
    fn accepted_inherited_failure_rebuilds_the_embedded_plan_step() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");

        report.accept_inherited_failure();

        assert_eq!(report.status, AgentTaskGateStatus::AcceptedInheritedFailure);
        let step = serde_json::to_value(&report.step).expect("serialize step");
        assert_eq!(step["status"], "success");
        assert_eq!(step["outputs"]["accepted_inherited_failure"], true);
    }

    #[test]
    fn gate_command_uses_the_declared_runtime_scratch_root() {
        let worktree = tempfile::tempdir().expect("worktree");
        let scratch = tempfile::tempdir().expect("scratch");

        let report = run_gate_command_with_policy_and_runtime_tmpdir(
            worktree.path(),
            8,
            "printf '%s:%s:%s' \"$TMPDIR\" \"$TEMP\" \"$TMP\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            Some(scratch.path()),
        )
        .expect("gate report");

        let expected = scratch.path().display().to_string();
        assert_eq!(report.stdout, format!("{expected}:{expected}:{expected}"));
    }

    #[test]
    fn gate_command_normalizes_failure_for_agent_feedback() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command(
            temp.path(),
            2,
            "printf 'line one\nline two\n'; printf 'boom\n' >&2; exit 42",
        )
        .expect("gate report");
        let evidence = report.failure_evidence.as_ref().expect("failure evidence");

        assert_eq!(report.status, AgentTaskGateStatus::Failed);
        assert_eq!(report.exit_code, 42);
        assert_eq!(
            evidence.command,
            "printf 'line one\nline two\n'; printf 'boom\n' >&2; exit 42"
        );
        assert_eq!(evidence.stdout_tail, "line one\nline two");
        assert_eq!(evidence.stderr_tail, "boom");
        assert!(evidence.summary.contains("deterministic gate failed"));
        assert!(evidence.agent_feedback.contains("Fix the code"));
    }

    #[test]
    fn gate_command_records_private_visibility_and_reveal_policy() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command_with_policy(
            temp.path(),
            3,
            "printf 'hidden failure'; exit 1",
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::SummaryOnly,
        )
        .expect("gate report");

        assert_eq!(report.status, AgentTaskGateStatus::Failed);
        assert_eq!(report.visibility, AgentTaskGateVisibility::Private);
        assert_eq!(report.reveal_policy, AgentTaskGateRevealPolicy::SummaryOnly);
        assert_eq!(report.stdout, "hidden failure");
    }

    #[test]
    fn agent_task_gate_report_normalizes_to_homeboy_gate_result() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command_with_policy(
            temp.path(),
            4,
            "printf 'hidden failure'; exit 1",
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::SummaryOnly,
        )
        .expect("gate report");
        let result: HomeboyGateResult = report.into();

        assert_eq!(
            result.schema,
            homeboy_core::gate::HOMEBOY_GATE_RESULT_SCHEMA
        );
        assert_eq!(result.id, "gate-4");
        assert_eq!(result.kind, HomeboyGateKind::Command);
        assert_eq!(result.status, HomeboyGateStatus::Failed);
        assert_eq!(result.visibility, HomeboyGateVisibility::Private);
        assert_eq!(result.reveal_policy, HomeboyGateRevealPolicy::SummaryOnly);
        assert_eq!(result.retryable, Some(true));
        assert!(result.summary.contains("detailed evidence is withheld"));
        assert!(result
            .agent_feedback
            .contains("hidden evaluator details are withheld"));
        assert_eq!(result.evidence["exit_code"], 1);
        assert_eq!(result.evidence["withheld"], true);
        assert_eq!(result.evidence.get("stdout"), None);
        assert_eq!(result.evidence.get("stderr"), None);
        assert_eq!(result.provenance["source_type"], "AgentTaskGateReport");
    }

    #[test]
    fn skipped_private_gate_preserves_blocker_evidence_without_command_disclosure() {
        let report = AgentTaskGateReport::skipped(
            "gate-2",
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "private-command --secret".to_string(),
            ],
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::Redacted,
            "gate-1",
        );

        assert_eq!(report.schema, AGENT_TASK_GATE_REPORT_SCHEMA);
        assert_eq!(report.step.status, PlanStepStatus::Skipped);
        assert_eq!(
            report
                .skip_reason
                .as_ref()
                .map(|reason| reason.blocking_gate_id.as_str()),
            Some("gate-1")
        );

        let result: HomeboyGateResult = report.into();
        assert_eq!(result.status, HomeboyGateStatus::Skipped);
        assert_eq!(result.visibility, HomeboyGateVisibility::Private);
        assert_eq!(result.evidence["skip_reason"]["blocking_gate_id"], "gate-1");
        assert_eq!(result.evidence.get("command"), None);
        assert!(!result.summary.contains("private-command"));
        assert!(result.agent_feedback.is_empty());
    }

    #[test]
    fn private_redacted_agent_task_gate_result_omits_command_and_output_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command_with_policy(
            temp.path(),
            6,
            "printf 'secret stdout'; printf 'secret stderr' >&2; exit 1",
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::Redacted,
        )
        .expect("gate report");
        let result: HomeboyGateResult = report.into();

        assert_eq!(result.status, HomeboyGateStatus::Failed);
        assert_eq!(result.reveal_policy, HomeboyGateRevealPolicy::Redacted);
        assert_eq!(result.evidence["redacted"], true);
        assert_eq!(result.evidence.get("command"), None);
        assert_eq!(result.evidence.get("stdout"), None);
        assert_eq!(result.evidence.get("stderr"), None);
        assert!(result.summary.contains("evidence redacted"));
    }

    #[test]
    fn successful_agent_task_gate_report_normalizes_to_passed_gate_result() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command(temp.path(), 5, "printf 'ok'").expect("gate report");
        let result: HomeboyGateResult = report.into();

        assert_eq!(result.id, "gate-5");
        assert_eq!(result.kind, HomeboyGateKind::Command);
        assert_eq!(result.status, HomeboyGateStatus::Passed);
        assert_eq!(result.retryable, Some(false));
        assert_eq!(result.evidence["exit_code"], 0);
        assert_eq!(result.evidence["stdout"], "ok");
        assert!(result.agent_feedback.is_empty());
        assert!(result.summary.contains("deterministic gate passed"));
    }

    #[test]
    fn isolated_gate_does_not_observe_ambient_durable_recipes_runs_or_runtime_liveness() {
        let _guard = ENV_MUTEX.lock().expect("env lock");
        let worktree = tempfile::tempdir().expect("worktree");
        let ambient = tempfile::tempdir().expect("ambient state");
        let home = ambient.path().join("home");
        let xdg_state = ambient.path().join("state");
        let xdg_runtime = ambient.path().join("runtime");
        fs::create_dir_all(home.join(".homeboy/recipes")).expect("ambient recipe directory");
        fs::create_dir_all(xdg_state.join("runs/active")).expect("ambient run directory");
        fs::create_dir_all(xdg_runtime.join("homeboy-live")).expect("ambient runtime directory");
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_STATE_HOME", &xdg_state);
        std::env::set_var("XDG_RUNTIME_DIR", &xdg_runtime);

        let report = run_gate_command(
            worktree.path(),
            7,
            "test ! -e \"$HOME/.homeboy/recipes\" && test ! -e \"$XDG_STATE_HOME/runs/active\" && test ! -e \"$XDG_RUNTIME_DIR/homeboy-live\"",
        )
        .expect("isolated gate report");

        std::env::remove_var("HOME");
        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("XDG_RUNTIME_DIR");

        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert!(report
            .environment
            .sanitized
            .iter()
            .any(|variable| variable.name == "HOME"));
        assert!(report
            .environment
            .sanitized
            .iter()
            .any(|variable| variable.name == "XDG_STATE_HOME"));
        assert!(report
            .environment
            .sanitized
            .iter()
            .any(|variable| variable.name == "XDG_RUNTIME_DIR"));
    }

    #[test]
    fn replacing_gate_environment_preserves_declared_variables_and_reports_policy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let policy = AgentTaskGateEnvironmentPolicy {
            mode: AgentTaskGateEnvironmentMode::Replace,
            variables: BTreeMap::from([("DECLARED_INPUT".to_string(), "kept".to_string())]),
            preserve: BTreeMap::new(),
            isolate_home: false,
            isolate_xdg: false,
        };
        let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            temp.path(),
            8,
            "test \"$DECLARED_INPUT\" = kept && test -z \"${HOME:-}\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &policy,
        )
        .expect("gate report");

        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(
            report.environment.mode,
            AgentTaskGateEnvironmentMode::Replace
        );
        assert_eq!(report.environment.inherited.len(), 1);
        assert_eq!(report.environment.inherited[0].name, "DECLARED_INPUT");
        assert_eq!(report.environment.inherited[0].value, "kept");
    }

    #[test]
    fn existing_gate_commands_are_automatic_toolchain_requirements() {
        let options = VerifyGateOptions {
            verify: vec!["cargo test --lib".to_string()],
            private_verify: vec!["npm test".to_string()],
            gate_toolchains: vec![AgentTaskGateToolchainRequirement {
                command: "cargo".to_string(),
                probe_arguments: vec!["metadata".to_string()],
            }],
            ..VerifyGateOptions::default()
        };

        assert_eq!(
            options.required_toolchains(),
            vec![
                AgentTaskGateToolchainRequirement {
                    command: "cargo".to_string(),
                    probe_arguments: vec!["metadata".to_string()],
                },
                AgentTaskGateToolchainRequirement {
                    command: "npm".to_string(),
                    probe_arguments: vec!["--version".to_string()],
                },
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_preserves_only_declared_homes_for_cargo_on_path() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_MUTEX.lock().expect("env lock");
        let workspace = tempfile::tempdir().expect("workspace");
        let original_home = tempfile::tempdir().expect("original home");
        let cargo_bin = original_home.path().join(".cargo/bin");
        fs::create_dir_all(&cargo_bin).expect("cargo bin");
        let cargo = cargo_bin.join("cargo");
        fs::write(
            &cargo,
            format!(
                "#!/bin/sh\ntest \"$RUSTUP_HOME\" = \"{}/.rustup\" && test \"$CARGO_HOME\" = \"{}/.cargo\"\n",
                original_home.path().display(),
                original_home.path().display(),
            ),
        )
        .expect("cargo probe");
        let mut permissions = fs::metadata(&cargo).expect("cargo metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&cargo, permissions).expect("cargo executable");
        let prior_home = std::env::var_os("HOME");
        let prior_path = std::env::var_os("PATH");
        std::env::set_var("HOME", original_home.path());
        std::env::set_var("PATH", &cargo_bin);

        let policy = AgentTaskGateEnvironmentPolicy {
            preserve: BTreeMap::from([
                ("RUSTUP_HOME".to_string(), "HOME/.rustup".to_string()),
                ("CARGO_HOME".to_string(), "HOME/.cargo".to_string()),
            ]),
            ..AgentTaskGateEnvironmentPolicy::default()
        };
        let result = preflight_gate_toolchains(
            workspace.path(),
            &policy,
            &[AgentTaskGateToolchainRequirement {
                command: "cargo".to_string(),
                probe_arguments: vec!["--version".to_string()],
            }],
            None,
        );

        match prior_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match prior_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
        result.expect("declared cargo homes initialize the toolchain");
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_reports_generic_initialization_failures_without_code_feedback() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_MUTEX.lock().expect("env lock");
        let workspace = tempfile::tempdir().expect("workspace");
        let bin = tempfile::tempdir().expect("bin");
        let tool = bin.path().join("other-tool");
        fs::write(&tool, "#!/bin/sh\necho unavailable >&2\nexit 7\n").expect("tool");
        let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("tool executable");
        let prior_path = std::env::var_os("PATH");
        std::env::set_var("PATH", bin.path());

        let error = preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[AgentTaskGateToolchainRequirement {
                command: "other-tool".to_string(),
                probe_arguments: vec!["initialize".to_string()],
            }],
            None,
        )
        .expect_err("unusable declared toolchain fails preflight");

        match prior_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
        assert!(error.message.contains("gate toolchain preflight"));
        assert!(!error.message.contains("Fix the code"));
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_runs_in_the_invocation_tmpdir() {
        use std::os::unix::fs::PermissionsExt;

        // Deliberately hermetic: the probe is addressed by absolute path so the
        // test never mutates PATH. Sibling tests spawn `sh` without holding
        // ENV_MUTEX, so clobbering PATH here would break them under the test
        // harness's parallelism.
        let workspace = tempfile::tempdir().expect("workspace");
        let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
        let bin = tempfile::tempdir().expect("bin");

        // Fails unless every temp-dir variable points at the invocation temp
        // dir. An unset variable compares as empty and exits non-zero too.
        let tool = bin.path().join("record-tmpdir");
        fs::write(
            &tool,
            format!(
                "#!/bin/sh\n[ \"$TMPDIR\" = '{dir}' ] || exit 3\n[ \"$TEMP\" = '{dir}' ] || exit 4\n[ \"$TMP\" = '{dir}' ] || exit 5\nexit 0\n",
                dir = runtime_tmpdir.path().display()
            ),
        )
        .expect("tool");
        let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("tool executable");

        preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[AgentTaskGateToolchainRequirement {
                command: tool.display().to_string(),
                probe_arguments: vec!["initialize".to_string()],
            }],
            Some(runtime_tmpdir.path()),
        )
        .expect("preflight probes run in the invocation temp dir");
    }

    #[test]
    fn gate_environment_pins_temp_dir_variables_to_the_invocation_tmpdir() {
        let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
        let selected = selected_gate_environment(
            &AgentTaskGateEnvironmentPolicy::default(),
            Some(runtime_tmpdir.path()),
        )
        .expect("gate environment");

        let expected = runtime_tmpdir.path().display().to_string();
        for name in TMPDIR_ENV_VARS {
            assert_eq!(
                selected.values.get(*name),
                Some(&expected),
                "{name} must resolve to the invocation temp dir"
            );
        }
    }
}

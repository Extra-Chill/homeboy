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

pub const AGENT_TASK_GATE_REPORT_SCHEMA: &str = "homeboy/agent-task-gate-report/v1";
const XDG_ENV_VARS: &[&str] = &[
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
];

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
    #[serde(default)]
    pub isolate_home: bool,
    #[serde(default)]
    pub isolate_xdg: bool,
}

impl Default for AgentTaskGateEnvironmentPolicy {
    fn default() -> Self {
        Self {
            mode: AgentTaskGateEnvironmentMode::Inherit,
            variables: BTreeMap::new(),
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
}

impl Default for VerifyGateOptions {
    fn default() -> Self {
        Self {
            verify: Vec::new(),
            private_verify: Vec::new(),
            private_gate_reveal: default_private_gate_reveal(),
            gate_timeout_seconds: default_gate_timeout_seconds(),
            gate_heartbeat_interval_seconds: default_gate_heartbeat_interval_seconds(),
            rerun_completed_gates: false,
            gate_environment: AgentTaskGateEnvironmentPolicy::default(),
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
    /// The same command's result against the immutable base when candidate
    /// adoption needs to distinguish inherited failures from regressions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<AgentTaskGateBaselineComparison>,
    #[serde(default, skip_serializing_if = "AgentTaskGateEnvironment::is_empty")]
    pub environment: AgentTaskGateEnvironment,
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
    /// The candidate command failed, but the identical failure was reproduced
    /// against the controller-recorded immutable baseline.
    AcceptedInheritedFailure,
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
            baseline_comparison: None,
            environment,
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
    if let Some(runtime_tmpdir) = runtime_tmpdir {
        process
            .env("TMPDIR", runtime_tmpdir)
            .env("TEMP", runtime_tmpdir)
            .env("TMP", runtime_tmpdir);
    }
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
    process
        .env("TMPDIR", runtime_tmpdir)
        .env("TEMP", runtime_tmpdir)
        .env("TMP", runtime_tmpdir);
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
    }

    #[test]
    fn verify_gate_policy_defaults_are_durable_and_backward_compatible() {
        let defaults = VerifyGateOptions::default();
        assert_eq!(defaults.gate_timeout(), Duration::from_secs(30 * 60));
        assert_eq!(defaults.gate_heartbeat_interval(), Duration::from_secs(5));
        assert!(!defaults.rerun_completed_gates);

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
}

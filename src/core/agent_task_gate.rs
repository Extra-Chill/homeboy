use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::gate::{
    HomeboyGateKind, HomeboyGateResult, HomeboyGateRevealPolicy, HomeboyGateStatus,
    HomeboyGateVisibility,
};
use crate::core::plan::{PlanStep, PlanStepStatus, PlanValues};
use crate::core::{Error, Result};

pub const AGENT_TASK_GATE_REPORT_SCHEMA: &str = "homeboy/agent-task-gate-report/v1";
const TASK_AFFECTING_ENV_VARS: &[&str] = &["STUDIO_RUNTIME"];

pub type AgentTaskGateVisibility = HomeboyGateVisibility;
pub type AgentTaskGateRevealPolicy = HomeboyGateRevealPolicy;

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
    #[serde(default, skip_serializing_if = "AgentTaskGateEnvironment::is_empty")]
    pub environment: AgentTaskGateEnvironment,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateEnvironment {
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
        self.inherited.is_empty() && self.sanitized.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateStatus {
    Succeeded,
    Failed,
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
            match status {
                AgentTaskGateStatus::Succeeded => HomeboyGateStatus::Passed,
                AgentTaskGateStatus::Failed => HomeboyGateStatus::Failed,
            },
        )
        .visibility(visibility)
        .reveal_policy(reveal_policy)
        .retryable(status == AgentTaskGateStatus::Failed);
        let step = PlanStep::builder(
            id.clone(),
            "agent_task.gate",
            match status {
                AgentTaskGateStatus::Succeeded => PlanStepStatus::Success,
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
            environment,
        }
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
    let command_vec = vec!["sh".to_string(), "-lc".to_string(), command.to_string()];
    let mut process = Command::new(&command_vec[0]);
    process.args(&command_vec[1..]).current_dir(cwd);
    let environment = selected_gate_environment(command);
    for variable in &environment.sanitized {
        process.env_remove(&variable.name);
    }
    let output = process.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run deterministic gate {command}")),
        )
    })?;
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
        environment,
    ))
}

fn selected_gate_environment(command: &str) -> AgentTaskGateEnvironment {
    let mut environment = AgentTaskGateEnvironment::default();
    for name in TASK_AFFECTING_ENV_VARS {
        let Ok(value) = std::env::var(name) else {
            continue;
        };
        let variable = AgentTaskGateEnvironmentVariable {
            name: (*name).to_string(),
            value,
        };
        if command_mentions_env(command, name) {
            environment.inherited.push(variable);
        } else {
            environment.sanitized.push(variable);
        }
    }
    environment
}

fn command_mentions_env(command: &str, name: &str) -> bool {
    command.contains(&format!("{name}="))
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
        let status = match report.status {
            AgentTaskGateStatus::Succeeded => HomeboyGateStatus::Passed,
            AgentTaskGateStatus::Failed => HomeboyGateStatus::Failed,
        };
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
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

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

        assert_eq!(result.schema, crate::core::gate::HOMEBOY_GATE_RESULT_SCHEMA);
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
    fn gate_command_sanitizes_inherited_runtime_selectors_by_default() {
        let _guard = ENV_MUTEX.lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("STUDIO_RUNTIME", "runtime-a");

        let report = run_gate_command(temp.path(), 7, "printf \"%s\" \"${STUDIO_RUNTIME:-unset}\"")
            .expect("gate report");

        std::env::remove_var("STUDIO_RUNTIME");

        assert_eq!(report.stdout, "unset");
        assert_eq!(report.environment.sanitized.len(), 1);
        assert_eq!(report.environment.sanitized[0].name, "STUDIO_RUNTIME");
        assert_eq!(report.environment.sanitized[0].value, "runtime-a");
    }

    #[test]
    fn gate_command_preserves_runtime_selector_when_command_requests_it() {
        let _guard = ENV_MUTEX.lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("STUDIO_RUNTIME", "runtime-a");

        let report = run_gate_command(
            temp.path(),
            8,
            "STUDIO_RUNTIME=runtime-a; printf \"%s\" \"$STUDIO_RUNTIME\"",
        )
        .expect("gate report");

        std::env::remove_var("STUDIO_RUNTIME");

        assert_eq!(report.stdout, "runtime-a");
        assert_eq!(report.environment.inherited.len(), 1);
        assert_eq!(report.environment.inherited[0].name, "STUDIO_RUNTIME");
    }
}

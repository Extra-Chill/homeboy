use std::path::Path;
use std::process::Command;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::gate::{
    HomeboyGateKind, HomeboyGateResult, HomeboyGateRevealPolicy, HomeboyGateStatus,
    HomeboyGateVisibility,
};
use crate::core::{Error, Result};

pub const AGENT_TASK_GATE_REPORT_SCHEMA: &str = "homeboy/agent-task-gate-report/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateReport {
    #[serde(default = "gate_report_schema")]
    pub schema: String,
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
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateVisibility {
    #[default]
    Visible,
    Private,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateRevealPolicy {
    #[default]
    FullEvidence,
    SummaryOnly,
    Redacted,
    NoDetail,
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
    let output = process.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run deterministic gate {command}")),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let status = if output.status.success() {
        AgentTaskGateStatus::Succeeded
    } else {
        AgentTaskGateStatus::Failed
    };
    let failure_evidence = (status == AgentTaskGateStatus::Failed)
        .then(|| gate_failure_evidence(command, exit_code, &stdout, &stderr));

    Ok(AgentTaskGateReport {
        schema: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
        id: format!("gate-{index}"),
        visibility,
        reveal_policy,
        status,
        command: command_vec,
        exit_code,
        stdout,
        stderr,
        failure_evidence,
    })
}

fn gate_report_schema() -> String {
    AGENT_TASK_GATE_REPORT_SCHEMA.to_string()
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
        let summary = report
            .failure_evidence
            .as_ref()
            .map(|evidence| evidence.summary.clone())
            .unwrap_or_else(|| format!("deterministic gate passed: {command}"));
        let agent_feedback = report
            .failure_evidence
            .as_ref()
            .map(|evidence| evidence.agent_feedback.clone())
            .unwrap_or_default();

        HomeboyGateResult::new(
            report.id.clone(),
            report.id.clone(),
            HomeboyGateKind::Command,
            status,
        )
        .summary(summary)
        .evidence(json!({
            "command": report.command,
            "exit_code": report.exit_code,
            "stdout": report.stdout,
            "stderr": report.stderr,
            "failure_evidence": report.failure_evidence,
        }))
        .visibility(report.visibility.into())
        .reveal_policy(report.reveal_policy.into())
        .retryable(status == HomeboyGateStatus::Failed)
        .agent_feedback(agent_feedback)
        .provenance(json!({
            "source_schema": report.schema,
            "source_type": "AgentTaskGateReport",
        }))
    }
}

impl From<AgentTaskGateVisibility> for HomeboyGateVisibility {
    fn from(visibility: AgentTaskGateVisibility) -> Self {
        match visibility {
            AgentTaskGateVisibility::Visible => Self::Visible,
            AgentTaskGateVisibility::Private => Self::Private,
        }
    }
}

impl From<AgentTaskGateRevealPolicy> for HomeboyGateRevealPolicy {
    fn from(reveal_policy: AgentTaskGateRevealPolicy) -> Self {
        match reveal_policy {
            AgentTaskGateRevealPolicy::FullEvidence => Self::FullEvidence,
            AgentTaskGateRevealPolicy::SummaryOnly => Self::SummaryOnly,
            AgentTaskGateRevealPolicy::Redacted => Self::Redacted,
            AgentTaskGateRevealPolicy::NoDetail => Self::NoDetail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let evidence = report.failure_evidence.expect("failure evidence");

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
        assert!(result.summary.contains("deterministic gate failed"));
        assert!(result.agent_feedback.contains("Fix the code"));
        assert_eq!(result.evidence["exit_code"], 1);
        assert_eq!(result.provenance["source_type"], "AgentTaskGateReport");
    }
}

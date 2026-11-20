use super::*;

impl From<AgentTaskGateReport> for HomeboyGateResult {
    fn from(report: AgentTaskGateReport) -> Self {
        let status = HomeboyGateStatus::from(report.status);
        let command = report.command.join(" ");
        let summary = gate_result_summary(&report, &command);
        let agent_feedback = gate_result_agent_feedback(&report);
        let mut evidence = gate_result_evidence(&report);
        if let Some(verdict) = &report.verdict {
            evidence["verdict"] = json!(verdict);
        }

        HomeboyGateResult::new(
            report.id.clone(),
            report.label.clone().unwrap_or_else(|| report.id.clone()),
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

/// Concrete next step for a gate that never executed: it has no verdict, so
/// it is re-run, never repaired by the provider (#16932).
pub const NON_EXECUTION_NEXT_STEP: &str =
    "the candidate was not judged; re-run its gates with `homeboy agent-task recover <cook-id> --regate`, adding `--verify '<COMMAND>'` to replace the declared verify commands";

/// Typed cause of a Lab gate that never executed, read from its
/// non-execution disposition. Codes, stages and job ids are not gate output,
/// so this is safe for private gates (#16932).
pub fn lab_non_execution_cause(receipt: &serde_json::Value) -> Option<String> {
    if receipt.get("schema").and_then(serde_json::Value::as_str)
        != Some(placement::LAB_GATE_DISPOSITION_SCHEMA)
        || receipt.get("executed").and_then(serde_json::Value::as_bool) != Some(false)
    {
        return None;
    }
    let text = |key: &str| {
        receipt
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let mut cause = format!(
        "Lab gate did not execute: [{}]",
        text("error_code").unwrap_or("unknown")
    );
    let located = [
        text("stage").map(|stage| format!("stage {stage}")),
        text("job_id").map(|job| format!("job {job}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !located.is_empty() {
        cause.push(' ');
        cause.push_str(&located.join(", "));
    }
    match receipt
        .get("retryable")
        .and_then(serde_json::Value::as_bool)
    {
        Some(true) => cause.push_str("; retryable"),
        Some(false) => cause.push_str("; not retryable"),
        None => {}
    }
    if let Some(artifact) = text("artifact_ref") {
        cause.push_str(&format!("; diagnostic {artifact}"));
    }
    Some(cause)
}

impl AgentTaskGateReport {
    /// Typed cause when this is a Lab gate that never executed.
    pub fn non_execution_cause(&self) -> Option<String> {
        self.lab_receipt.as_ref().and_then(lab_non_execution_cause)
    }
}

pub(super) fn gate_result_summary(report: &AgentTaskGateReport, command: &str) -> String {
    if report.status == AgentTaskGateStatus::Skipped {
        let blocker = report
            .skip_reason
            .as_ref()
            .map(|reason| reason.blocking_gate_id.as_str())
            .unwrap_or("an earlier gate");
        return format!("deterministic gate skipped after prerequisite {blocker} failed");
    }
    // A gate that never ran has no verdict: never "passed", never "failed".
    if matches!(
        report.status,
        AgentTaskGateStatus::Deferred | AgentTaskGateStatus::Unavailable
    ) {
        // A supervision stop ran but reached no verdict: name the limit, not
        // the (possibly private) command.
        let limit = match report.termination {
            AgentTaskGateTermination::TimedOut => Some("wall-clock"),
            AgentTaskGateTermination::NoProgress => Some("no-progress"),
            _ => None,
        };
        if let Some(limit) = limit {
            let regated = if report.supervision_regate.is_some() {
                " again after one automatic re-gate"
            } else {
                ""
            };
            return format!(
                "deterministic gate {} was stopped at its {limit} limit{regated} without reaching a verdict; {NON_EXECUTION_NEXT_STEP}",
                report.id
            );
        }
        let cause = report
            .non_execution_cause()
            .unwrap_or_else(|| format!("deterministic gate {} did not execute", report.id));
        return format!("{cause}; {NON_EXECUTION_NEXT_STEP}");
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
        .unwrap_or_else(|| match report.status {
            AgentTaskGateStatus::Succeeded => format!("deterministic gate passed: {command}"),
            _ => format!("deterministic gate failed: {command}"),
        })
}

pub(super) fn gate_result_agent_feedback(report: &AgentTaskGateReport) -> String {
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

pub(super) fn gate_result_evidence(report: &AgentTaskGateReport) -> serde_json::Value {
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
        "termination": report.termination,
        "stdout": report.stdout,
        "stderr": report.stderr,
        "capture": report.capture,
        "failure_evidence": report.failure_evidence,
        "test_result": report.test_result,
        "cargo_selection": report.cargo_selection,
        "environment": report.environment,
    })
}

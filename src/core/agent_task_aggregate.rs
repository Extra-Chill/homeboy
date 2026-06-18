use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

use super::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskFollowUp, AgentTaskOutcome, AgentTaskOutcomeStatus,
};

pub const AGENT_TASK_AGGREGATE_SCHEMA: &str = "homeboy/agent-task-aggregate/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskAggregateReport {
    #[serde(default = "aggregate_schema")]
    pub schema: String,
    pub summary: AgentTaskAggregateSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTaskReconciliationItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inventory: Vec<AgentTaskArtifactInventoryItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apply_candidates: Vec<AgentTaskDecisionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issue_report_candidates: Vec<AgentTaskDecisionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retry_plan: Vec<AgentTaskDecisionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_candidates: Vec<AgentTaskDecisionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matrix: Vec<AgentTaskMatrixRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAggregateSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub no_op: usize,
    pub timed_out: usize,
    pub provider_error: usize,
    pub unable_to_remediate: usize,
    pub follow_up_issue: usize,
    pub cancelled: usize,
    pub apply_candidates: usize,
    pub issue_report_candidates: usize,
    pub retry_candidates: usize,
    pub review_candidates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskReconciliationItem {
    pub task_id: String,
    pub status: AgentTaskOutcomeStatus,
    pub decision: AgentTaskReconciliationDecision,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<AgentTaskArtifactInventoryItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<AgentTaskEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<AgentTaskFollowUp>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskReconciliationDecision {
    ApplyCandidate,
    IssueReportCandidate,
    RetryCandidate,
    ReviewCandidate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskArtifactInventoryItem {
    pub task_id: String,
    pub artifact_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskDecisionRef {
    pub task_id: String,
    pub decision: AgentTaskReconciliationDecision,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskMatrixRow {
    pub task_id: String,
    pub status: AgentTaskOutcomeStatus,
    pub axes: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metrics: Value,
}

fn aggregate_agent_task_outcomes(outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateReport {
    let mut report = AgentTaskAggregateReport {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        summary: AgentTaskAggregateSummary {
            total: outcomes.len(),
            ..AgentTaskAggregateSummary::default()
        },
        tasks: Vec::with_capacity(outcomes.len()),
        artifact_inventory: Vec::new(),
        apply_candidates: Vec::new(),
        issue_report_candidates: Vec::new(),
        retry_plan: Vec::new(),
        review_candidates: Vec::new(),
        matrix: Vec::new(),
    };

    for outcome in outcomes {
        count_status(&mut report.summary, outcome.status);

        let artifacts: Vec<_> = outcome
            .artifacts
            .iter()
            .map(|artifact| inventory_item(&outcome.task_id, artifact))
            .collect();
        report.artifact_inventory.extend(artifacts.clone());

        let (decision, reason, artifact_ids) = reconcile_outcome(outcome);
        let decision_ref = AgentTaskDecisionRef {
            task_id: outcome.task_id.clone(),
            decision,
            reason: reason.clone(),
            artifact_ids,
        };

        match decision {
            AgentTaskReconciliationDecision::ApplyCandidate => {
                report.summary.apply_candidates += 1;
                report.apply_candidates.push(decision_ref);
            }
            AgentTaskReconciliationDecision::IssueReportCandidate => {
                report.summary.issue_report_candidates += 1;
                report.issue_report_candidates.push(decision_ref);
            }
            AgentTaskReconciliationDecision::RetryCandidate => {
                report.summary.retry_candidates += 1;
                report.retry_plan.push(decision_ref);
            }
            AgentTaskReconciliationDecision::ReviewCandidate => {
                report.summary.review_candidates += 1;
                report.review_candidates.push(decision_ref);
            }
        }

        if let Some(row) = matrix_row(outcome) {
            report.matrix.push(row);
        }

        report.tasks.push(AgentTaskReconciliationItem {
            task_id: outcome.task_id.clone(),
            status: outcome.status,
            decision,
            reason,
            summary: outcome.summary.clone(),
            artifacts,
            evidence_refs: outcome.evidence_refs.clone(),
            diagnostics: outcome.diagnostics.clone(),
            follow_up: outcome.follow_up.clone(),
        });
    }

    report
}

impl From<&[AgentTaskOutcome]> for AgentTaskAggregateReport {
    fn from(outcomes: &[AgentTaskOutcome]) -> Self {
        aggregate_agent_task_outcomes(outcomes)
    }
}

impl From<Vec<AgentTaskOutcome>> for AgentTaskAggregateReport {
    fn from(outcomes: Vec<AgentTaskOutcome>) -> Self {
        aggregate_agent_task_outcomes(&outcomes)
    }
}

impl fmt::Display for AgentTaskAggregateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut markdown = String::new();
        markdown.push_str("## Agent Task Outcomes\n\n");
        markdown.push_str(&format!(
            "- total: {}\n- succeeded: {}\n- failed: {}\n- no-op: {}\n- timed out: {}\n- provider errors: {}\n- apply candidates: {}\n- issue report candidates: {}\n- retry candidates: {}\n- review candidates: {}\n\n",
            self.summary.total,
            self.summary.succeeded,
            self.summary.failed,
            self.summary.no_op,
            self.summary.timed_out,
            self.summary.provider_error,
            self.summary.apply_candidates,
            self.summary.issue_report_candidates,
            self.summary.retry_candidates,
            self.summary.review_candidates
        ));

        markdown.push_str("| Task | Status | Decision | Reason | Artifacts |\n");
        markdown.push_str("| --- | --- | --- | --- | --- |\n");
        for task in &self.tasks {
            let artifacts = task
                .artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            markdown.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                escape_markdown_table_cell(&task.task_id),
                task.status.as_str(),
                task.decision.as_str(),
                escape_markdown_table_cell(&task.reason),
                escape_markdown_table_cell(&artifacts)
            ));
        }

        if !self.matrix.is_empty() {
            markdown.push_str("\n## Matrix\n\n");
            markdown.push_str("| Task | Status | Axes | Metrics |\n");
            markdown.push_str("| --- | --- | --- | --- |\n");
            for row in &self.matrix {
                markdown.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    escape_markdown_table_cell(&row.task_id),
                    row.status.as_str(),
                    escape_markdown_table_cell(&row.axes.to_string()),
                    escape_markdown_table_cell(&row.metrics.to_string())
                ));
            }
        }

        f.write_str(&markdown)
    }
}

impl AgentTaskOutcomeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::NoOp => "no_op",
            Self::UnableToRemediate => "unable_to_remediate",
            Self::ProviderError => "provider_error",
            Self::Timeout => "timeout",
            Self::Failed => "failed",
            Self::FollowUpIssue => "follow_up_issue",
            Self::Cancelled => "cancelled",
        }
    }
}

impl AgentTaskReconciliationDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApplyCandidate => "apply_candidate",
            Self::IssueReportCandidate => "issue_report_candidate",
            Self::RetryCandidate => "retry_candidate",
            Self::ReviewCandidate => "review_candidate",
        }
    }
}

fn count_status(summary: &mut AgentTaskAggregateSummary, status: AgentTaskOutcomeStatus) {
    match status {
        AgentTaskOutcomeStatus::Succeeded => summary.succeeded += 1,
        AgentTaskOutcomeStatus::NoOp => summary.no_op += 1,
        AgentTaskOutcomeStatus::UnableToRemediate => summary.unable_to_remediate += 1,
        AgentTaskOutcomeStatus::ProviderError => summary.provider_error += 1,
        AgentTaskOutcomeStatus::Timeout => summary.timed_out += 1,
        AgentTaskOutcomeStatus::Failed => summary.failed += 1,
        AgentTaskOutcomeStatus::FollowUpIssue => summary.follow_up_issue += 1,
        AgentTaskOutcomeStatus::Cancelled => summary.cancelled += 1,
    }
}

fn reconcile_outcome(
    outcome: &AgentTaskOutcome,
) -> (AgentTaskReconciliationDecision, String, Vec<String>) {
    let rejected_artifact_ids = outcome
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact_flag(artifact, "rejected") || artifact_flag(artifact, "false_positive")
        })
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    if !rejected_artifact_ids.is_empty() {
        return (
            AgentTaskReconciliationDecision::IssueReportCandidate,
            "artifact marked rejected or false-positive".to_string(),
            rejected_artifact_ids,
        );
    }

    if matches!(
        outcome.status,
        AgentTaskOutcomeStatus::ProviderError | AgentTaskOutcomeStatus::Timeout
    ) || matches!(
        outcome.failure_classification,
        Some(
            AgentTaskFailureClassification::Provider
                | AgentTaskFailureClassification::Transient
                | AgentTaskFailureClassification::Timeout
        )
    ) {
        return (
            AgentTaskReconciliationDecision::RetryCandidate,
            "provider error or timeout is retryable".to_string(),
            artifact_ids(outcome),
        );
    }

    if matches!(outcome.status, AgentTaskOutcomeStatus::FollowUpIssue)
        || outcome
            .follow_up
            .as_ref()
            .is_some_and(|follow_up| follow_up.kind == "issue_report")
    {
        return (
            AgentTaskReconciliationDecision::IssueReportCandidate,
            "outcome requested a follow-up issue report".to_string(),
            artifact_ids(outcome),
        );
    }

    let apply_artifact_ids = outcome
        .artifacts
        .iter()
        .filter(|artifact| is_apply_artifact(artifact))
        .map(|artifact| artifact.id.clone())
        .collect::<Vec<_>>();
    if matches!(outcome.status, AgentTaskOutcomeStatus::Succeeded) && !apply_artifact_ids.is_empty()
    {
        return (
            AgentTaskReconciliationDecision::ApplyCandidate,
            "succeeded with reviewable patch/artifact output".to_string(),
            apply_artifact_ids,
        );
    }

    (
        AgentTaskReconciliationDecision::ReviewCandidate,
        match outcome.status {
            AgentTaskOutcomeStatus::NoOp => "no-op outcome needs review".to_string(),
            AgentTaskOutcomeStatus::UnableToRemediate => {
                "unable-to-remediate outcome needs review".to_string()
            }
            AgentTaskOutcomeStatus::Cancelled => "cancelled task needs review".to_string(),
            AgentTaskOutcomeStatus::Failed => "failed task needs review".to_string(),
            AgentTaskOutcomeStatus::Succeeded => empty_or_unknown_patch_reason(outcome)
                .unwrap_or_else(|| "succeeded without apply-back artifact".to_string()),
            AgentTaskOutcomeStatus::ProviderError
            | AgentTaskOutcomeStatus::Timeout
            | AgentTaskOutcomeStatus::FollowUpIssue => unreachable!("handled above"),
        },
        artifact_ids(outcome),
    )
}

fn artifact_ids(outcome: &AgentTaskOutcome) -> Vec<String> {
    outcome
        .artifacts
        .iter()
        .map(|artifact| artifact.id.clone())
        .collect()
}

fn inventory_item(task_id: &str, artifact: &AgentTaskArtifact) -> AgentTaskArtifactInventoryItem {
    AgentTaskArtifactInventoryItem {
        task_id: task_id.to_string(),
        artifact_id: artifact.id.clone(),
        kind: artifact.kind.clone(),
        name: artifact.name.clone(),
        path: artifact.path.clone(),
        url: artifact.url.clone(),
        size_bytes: artifact.size_bytes,
        sha256: artifact.sha256.clone(),
    }
}

fn is_apply_artifact(artifact: &AgentTaskArtifact) -> bool {
    is_apply_kind_artifact(artifact) && artifact_has_nonzero_size(artifact)
}

fn is_apply_kind_artifact(artifact: &AgentTaskArtifact) -> bool {
    matches!(
        artifact.kind.as_str(),
        "patch" | "diff" | "change_artifact" | "workspace_patch" | "artifact"
    ) || artifact_flag(artifact, "approved")
}

fn artifact_has_nonzero_size(artifact: &AgentTaskArtifact) -> bool {
    // A patch counts as a promotion candidate only when there is positive
    // evidence of non-empty content. An unknown (`None`) size is treated as
    // empty/uncertain rather than as a valid candidate, so cook runs whose
    // provider only wrote 0-byte patches (or omitted size entirely) are not
    // reported as successful fixes (#4610).
    matches!(artifact.size_bytes, Some(size) if size > 0)
}

/// When a `Succeeded` outcome produced patch-shaped artifacts that were all
/// empty or unknown-size, surface a per-cell reason explaining why no apply
/// candidate was promoted. Returns `None` when there were no patch artifacts or
/// when at least one had non-empty content.
fn empty_or_unknown_patch_reason(outcome: &AgentTaskOutcome) -> Option<String> {
    let patch_artifacts: Vec<&AgentTaskArtifact> = outcome
        .artifacts
        .iter()
        .filter(|artifact| is_apply_kind_artifact(artifact))
        .collect();
    if patch_artifacts.is_empty() {
        return None;
    }
    if patch_artifacts
        .iter()
        .any(|artifact| artifact_has_nonzero_size(artifact))
    {
        return None;
    }
    if patch_artifacts
        .iter()
        .any(|artifact| artifact.size_bytes == Some(0))
    {
        Some("patch artifact was empty (0 bytes); provider produced no file changes".to_string())
    } else {
        Some("patch artifact size unknown; cannot confirm changes were produced".to_string())
    }
}

fn artifact_flag(artifact: &AgentTaskArtifact, key: &str) -> bool {
    artifact
        .metadata
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn matrix_row(outcome: &AgentTaskOutcome) -> Option<AgentTaskMatrixRow> {
    let axes = outcome.metadata.get("matrix_axes")?.clone();
    Some(AgentTaskMatrixRow {
        task_id: outcome.task_id.clone(),
        status: outcome.status,
        axes,
        metrics: outcome
            .metadata
            .get("metrics")
            .cloned()
            .unwrap_or(Value::Null),
    })
}

fn escape_markdown_table_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn aggregate_schema() -> String {
    AGENT_TASK_AGGREGATE_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aggregate_outcomes_classifies_apply_retry_issue_and_review_candidates() {
        let outcomes = vec![
            outcome(
                "apply",
                AgentTaskOutcomeStatus::Succeeded,
                vec![artifact("patch-1", "patch", json!({ "approved": true }))],
            ),
            AgentTaskOutcome {
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                ..outcome("retry", AgentTaskOutcomeStatus::ProviderError, Vec::new())
            },
            AgentTaskOutcome {
                follow_up: Some(AgentTaskFollowUp {
                    kind: "issue_report".to_string(),
                    title: "Needs issue".to_string(),
                    body: None,
                    uri: Some("https://example.test/issues/1".to_string()),
                }),
                ..outcome(
                    "issue",
                    AgentTaskOutcomeStatus::FollowUpIssue,
                    vec![artifact("report", "report", json!({}))],
                )
            },
            outcome(
                "review",
                AgentTaskOutcomeStatus::UnableToRemediate,
                Vec::new(),
            ),
        ];

        let report = aggregate_agent_task_outcomes(&outcomes);

        assert_eq!(report.schema, AGENT_TASK_AGGREGATE_SCHEMA);
        assert_eq!(report.summary.total, 4);
        assert_eq!(report.summary.succeeded, 1);
        assert_eq!(report.summary.provider_error, 1);
        assert_eq!(report.summary.unable_to_remediate, 1);
        assert_eq!(report.summary.follow_up_issue, 1);
        assert_eq!(report.summary.apply_candidates, 1);
        assert_eq!(report.summary.retry_candidates, 1);
        assert_eq!(report.summary.issue_report_candidates, 1);
        assert_eq!(report.summary.review_candidates, 1);
        assert_eq!(report.apply_candidates[0].task_id, "apply");
        assert_eq!(report.apply_candidates[0].artifact_ids, vec!["patch-1"]);
        assert_eq!(report.retry_plan[0].task_id, "retry");
        assert_eq!(report.issue_report_candidates[0].task_id, "issue");
        assert_eq!(report.review_candidates[0].task_id, "review");
    }

    #[test]
    fn aggregate_outcomes_preserves_artifacts_evidence_and_matrix_metrics() {
        let mut item = outcome(
            "matrix-task",
            AgentTaskOutcomeStatus::Succeeded,
            vec![artifact("diff", "diff", json!({}))],
        );
        item.evidence_refs = vec![AgentTaskEvidenceRef {
            kind: "log".to_string(),
            uri: "artifact://matrix-task/log".to_string(),
            label: Some("runner log".to_string()),
        }];
        item.metadata = json!({
            "matrix_axes": { "model": "fast", "scenario": "audit" },
            "metrics": { "duration_ms": 42 }
        });

        let report = aggregate_agent_task_outcomes(&[item]);

        assert_eq!(report.artifact_inventory.len(), 1);
        assert_eq!(report.artifact_inventory[0].task_id, "matrix-task");
        assert_eq!(
            report.tasks[0].evidence_refs[0].uri,
            "artifact://matrix-task/log"
        );
        assert_eq!(report.matrix.len(), 1);
        assert_eq!(report.matrix[0].axes["model"], json!("fast"));
        assert_eq!(report.matrix[0].metrics["duration_ms"], json!(42));
    }

    #[test]
    fn aggregate_outcomes_routes_rejected_artifacts_to_issue_reports() {
        let report = aggregate_agent_task_outcomes(&[outcome(
            "false-positive",
            AgentTaskOutcomeStatus::Succeeded,
            vec![artifact(
                "candidate",
                "patch",
                json!({ "false_positive": true }),
            )],
        )]);

        assert!(report.apply_candidates.is_empty());
        assert_eq!(report.issue_report_candidates[0].task_id, "false-positive");
        assert_eq!(
            report.issue_report_candidates[0].reason,
            "artifact marked rejected or false-positive"
        );
    }

    #[test]
    fn aggregate_outcomes_does_not_apply_empty_patch_artifacts() {
        let mut empty_patch = artifact("sample-patch", "patch", json!({}));
        empty_patch.size_bytes = Some(0);

        let report = aggregate_agent_task_outcomes(&[outcome(
            "empty-patch",
            AgentTaskOutcomeStatus::Succeeded,
            vec![empty_patch],
        )]);

        assert!(report.apply_candidates.is_empty());
        assert_eq!(report.summary.apply_candidates, 0);
        assert_eq!(report.summary.review_candidates, 1);
        assert_eq!(report.review_candidates[0].task_id, "empty-patch");
        assert_eq!(
            report.review_candidates[0].reason,
            "patch artifact was empty (0 bytes); provider produced no file changes"
        );
    }

    #[test]
    fn aggregate_outcomes_keeps_non_empty_patch_apply_candidate() {
        let mut non_empty_patch = artifact("sample-patch", "patch", json!({}));
        non_empty_patch.size_bytes = Some(128);

        let report = aggregate_agent_task_outcomes(&[outcome(
            "non-empty-patch",
            AgentTaskOutcomeStatus::Succeeded,
            vec![non_empty_patch],
        )]);

        assert_eq!(report.summary.apply_candidates, 1);
        assert_eq!(report.apply_candidates[0].task_id, "non-empty-patch");
        assert_eq!(
            report.apply_candidates[0].artifact_ids,
            vec!["sample-patch"]
        );
    }

    #[test]
    fn aggregate_outcomes_routes_unknown_size_patch_to_review() {
        // An unknown-size patch cannot be confirmed non-empty, so it must not be
        // auto-promoted as an apply candidate (#4610). It routes to review with a
        // reason that explains the uncertainty.
        let mut unknown_size_patch = artifact("legacy-patch", "patch", json!({}));
        unknown_size_patch.size_bytes = None;

        let report = aggregate_agent_task_outcomes(&[outcome(
            "legacy-patch",
            AgentTaskOutcomeStatus::Succeeded,
            vec![unknown_size_patch],
        )]);

        assert!(report.apply_candidates.is_empty());
        assert_eq!(report.summary.apply_candidates, 0);
        assert_eq!(report.summary.review_candidates, 1);
        assert_eq!(report.review_candidates[0].task_id, "legacy-patch");
        assert_eq!(
            report.review_candidates[0].reason,
            "patch artifact size unknown; cannot confirm changes were produced"
        );
    }

    #[test]
    fn aggregate_outcomes_reports_no_patch_produced_when_all_cells_empty() {
        // Reproduces the #4610 scenario: 3 succeeded cells, each with a 0-byte
        // patch artifact. The cook run should report zero apply candidates and no
        // succeeded apply-candidate cells.
        let mut empty_patch_a = artifact("patch-a", "patch", json!({}));
        empty_patch_a.size_bytes = Some(0);
        let mut empty_patch_b = artifact("patch-b", "patch", json!({}));
        empty_patch_b.size_bytes = Some(0);
        let mut empty_patch_c = artifact("patch-c", "patch", json!({}));
        empty_patch_c.size_bytes = Some(0);

        let report = aggregate_agent_task_outcomes(&[
            outcome(
                "cell-1",
                AgentTaskOutcomeStatus::Succeeded,
                vec![empty_patch_a],
            ),
            outcome(
                "cell-2",
                AgentTaskOutcomeStatus::Succeeded,
                vec![empty_patch_b],
            ),
            outcome(
                "cell-3",
                AgentTaskOutcomeStatus::Succeeded,
                vec![empty_patch_c],
            ),
        ]);

        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.apply_candidates, 0);
        assert_eq!(report.apply_candidates.len(), 0);
        assert_eq!(report.summary.review_candidates, 3);
        assert!(report.review_candidates.iter().all(|candidate| candidate
            .reason
            .starts_with("patch artifact was empty (0 bytes)")));
    }

    #[test]
    fn aggregate_report_renders_pr_comment_markdown() {
        let report = aggregate_agent_task_outcomes(&[outcome(
            "task|one",
            AgentTaskOutcomeStatus::NoOp,
            Vec::new(),
        )]);

        let markdown = report.to_string();

        assert!(markdown.contains("## Agent Task Outcomes"));
        assert!(markdown.contains("- total: 1"));
        assert!(markdown.contains("| Task | Status | Decision | Reason | Artifacts |"));
        assert!(markdown.contains("| task\\|one | no_op | review_candidate |"));
        assert!(markdown.contains("no-op outcome needs review"));
    }

    #[test]
    fn aggregate_report_renders_matrix_markdown() {
        let mut item = outcome("matrix", AgentTaskOutcomeStatus::Succeeded, Vec::new());
        item.metadata = json!({
            "matrix_axes": { "model": "fast" },
            "metrics": { "duration_ms": 42 }
        });
        let report = aggregate_agent_task_outcomes(&[item]);

        let markdown = report.to_string();

        assert!(markdown.contains("## Matrix"));
        assert!(markdown.contains("| Task | Status | Axes | Metrics |"));
        assert!(markdown.contains("matrix | succeeded"));
        assert!(markdown.contains("duration_ms"));
    }

    fn outcome(
        task_id: &str,
        status: AgentTaskOutcomeStatus,
        artifacts: Vec<AgentTaskArtifact>,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: super::super::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            status,
            summary: Some(format!("{task_id} summary")),
            failure_classification: None,
            artifacts,
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }

    fn artifact(id: &str, kind: &str, metadata: Value) -> AgentTaskArtifact {
        AgentTaskArtifact {
            schema: super::super::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: id.to_string(),
            kind: kind.to_string(),
            name: Some(format!("{id}.txt")),
            path: Some(format!("artifacts/{id}.txt")),
            url: None,
            mime: None,
            size_bytes: Some(12),
            sha256: Some(format!("sha256:{id}")),
            metadata,
        }
    }
}

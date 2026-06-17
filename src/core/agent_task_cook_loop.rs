use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_task::{
    AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::agent_task_gate::{
    text_tail, AgentTaskGateReport, AgentTaskGateRevealPolicy, AgentTaskGateStatus,
    AgentTaskGateVisibility,
};
use crate::core::agent_task_promotion::{AgentTaskPromotionReport, AgentTaskPromotionStatus};
use crate::core::gate::{HomeboyGateResult, HomeboyGateStatus};

pub const AGENT_TASK_COOK_LOOP_REPORT_SCHEMA: &str = "homeboy/agent-task-cook-loop-report/v1";
const RISKY_CHANGED_FILE_THRESHOLD: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskCookLoopOptions {
    pub source_request: AgentTaskRequest,
    pub promotion_report: AgentTaskPromotionReport,
    pub attempt: u32,
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub current_diff: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskCookLoopReport {
    #[serde(default = "cook_loop_report_schema")]
    pub schema: String,
    pub status: AgentTaskCookLoopStatus,
    pub attempt: u32,
    pub max_attempts: u32,
    pub retry_budget_remaining: u32,
    pub source_task_id: String,
    pub source_run_id: Option<String>,
    pub promotion_status: AgentTaskPromotionStatus,
    pub quality: AgentTaskCookLoopQualityReport,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_gates: Vec<AgentTaskCookLoopGateFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_gate_results: Vec<HomeboyGateResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_request: Option<AgentTaskRequest>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskCookLoopStatus {
    GreenCompleted,
    NoChanges,
    RetryRequested,
    RetriesExhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookLoopQualityReport {
    pub classification: AgentTaskCookLoopQualityClassification,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskCookLoopQualityClassification {
    NoChanges,
    PatchProduced,
    LargeOrRiskyPatch,
    VerifiedPatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookLoopGateFailure {
    pub gate_id: String,
    #[serde(default)]
    pub visibility: AgentTaskGateVisibility,
    #[serde(default)]
    pub reveal_policy: AgentTaskGateRevealPolicy,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout_tail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr_tail: String,
    pub summary: String,
    pub agent_feedback: String,
}

pub fn evaluate_cook_loop(options: AgentTaskCookLoopOptions) -> AgentTaskCookLoopReport {
    let failed_gates: Vec<AgentTaskCookLoopGateFailure> = options
        .promotion_report
        .deterministic_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskGateStatus::Failed)
        .map(gate_failure)
        .collect();
    let failed_gate_results: Vec<HomeboyGateResult> =
        normalized_promotion_gate_results(&options.promotion_report)
            .filter(|gate| gate.status == HomeboyGateStatus::Failed)
            .collect();
    let retry_budget_remaining = options.max_attempts.saturating_sub(options.attempt);
    let quality = classify_cook_loop_quality(&options.promotion_report);
    let should_retry = options.promotion_report.status == AgentTaskPromotionStatus::GateFailed
        && !failed_gates.is_empty()
        && retry_budget_remaining > 0;
    let follow_up_request = should_retry.then(|| build_follow_up_request(&options, &failed_gates));
    let status = if follow_up_request.is_some() {
        AgentTaskCookLoopStatus::RetryRequested
    } else if quality.classification == AgentTaskCookLoopQualityClassification::NoChanges {
        AgentTaskCookLoopStatus::NoChanges
    } else if failed_gates.is_empty() {
        AgentTaskCookLoopStatus::GreenCompleted
    } else {
        AgentTaskCookLoopStatus::RetriesExhausted
    };

    AgentTaskCookLoopReport {
        schema: AGENT_TASK_COOK_LOOP_REPORT_SCHEMA.to_string(),
        status,
        attempt: options.attempt,
        max_attempts: options.max_attempts,
        retry_budget_remaining,
        source_task_id: options.source_request.task_id.clone(),
        source_run_id: options.source_run_id.clone(),
        promotion_status: options.promotion_report.status,
        quality,
        failed_gates,
        failed_gate_results,
        follow_up_request,
        metadata: options.metadata,
    }
}

fn classify_cook_loop_quality(report: &AgentTaskPromotionReport) -> AgentTaskCookLoopQualityReport {
    let changed_file_count = report.changed_files.len();
    if changed_file_count == 0 {
        return AgentTaskCookLoopQualityReport {
            classification: AgentTaskCookLoopQualityClassification::NoChanges,
            summary: "cook produced no changed files; task likely still requires review or retry"
                .to_string(),
            signals: vec!["changed_files=0".to_string()],
        };
    }

    let mut signals = vec![format!("changed_files={changed_file_count}")];
    if changed_file_count > RISKY_CHANGED_FILE_THRESHOLD {
        signals.push(format!("changed_files>{RISKY_CHANGED_FILE_THRESHOLD}"));
        return AgentTaskCookLoopQualityReport {
            classification: AgentTaskCookLoopQualityClassification::LargeOrRiskyPatch,
            summary: format!(
                "cook changed {changed_file_count} files; review patch shape before treating it as ready"
            ),
            signals,
        };
    }

    let has_passed_gate = normalized_promotion_gate_results(report)
        .any(|gate| gate.status == HomeboyGateStatus::Passed);
    if report.status == AgentTaskPromotionStatus::Applied && has_passed_gate {
        return AgentTaskCookLoopQualityReport {
            classification: AgentTaskCookLoopQualityClassification::VerifiedPatch,
            summary: "cook produced a patch and deterministic gates passed".to_string(),
            signals,
        };
    }

    AgentTaskCookLoopQualityReport {
        classification: AgentTaskCookLoopQualityClassification::PatchProduced,
        summary: "cook produced a patch that needs promotion or verification".to_string(),
        signals,
    }
}

fn normalized_promotion_gate_results(
    report: &AgentTaskPromotionReport,
) -> impl Iterator<Item = HomeboyGateResult> + '_ {
    if report.gate_results.is_empty() {
        EitherGateResults::Legacy(
            report
                .deterministic_gates
                .iter()
                .cloned()
                .map(HomeboyGateResult::from),
        )
    } else {
        EitherGateResults::Normalized(report.gate_results.iter().cloned())
    }
}

enum EitherGateResults<L, R> {
    Legacy(L),
    Normalized(R),
}

impl<T, L, R> Iterator for EitherGateResults<L, R>
where
    L: Iterator<Item = T>,
    R: Iterator<Item = T>,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Legacy(iter) => iter.next(),
            Self::Normalized(iter) => iter.next(),
        }
    }
}

fn build_follow_up_request(
    options: &AgentTaskCookLoopOptions,
    failed_gates: &[AgentTaskCookLoopGateFailure],
) -> AgentTaskRequest {
    let mut request = options.source_request.clone();
    let next_attempt = options.attempt.saturating_add(1);
    let agent_visible_failed_gates = agent_visible_gate_failures(failed_gates);
    request.schema = AGENT_TASK_REQUEST_SCHEMA.to_string();
    request.task_id = format!("{}-gate-fix-{}", request.task_id, next_attempt);
    request.parent_plan_id = request
        .parent_plan_id
        .clone()
        .or_else(|| options.source_run_id.clone());
    request.instructions = follow_up_instructions(options, &agent_visible_failed_gates);
    request.inputs = json!({
        "cook_loop": {
            "source_run_id": options.source_run_id,
            "source_task_id": options.source_request.task_id,
            "source_patch_task_id": options.promotion_report.source.task_id,
            "promotion_status": options.promotion_report.status,
            "attempt": options.attempt,
            "next_attempt": next_attempt,
            "max_attempts": options.max_attempts,
            "retry_budget_remaining_after_dispatch": options.max_attempts.saturating_sub(next_attempt),
            "to_worktree": options.promotion_report.to_worktree,
            "changed_files": options.promotion_report.changed_files,
            "patch_artifact": options.promotion_report.patch_artifact,
            "failed_gates": agent_visible_failed_gates,
            "current_diff": options.current_diff,
        }
    });
    request.source_refs.push(AgentTaskSourceRef {
        kind: "agent-task-run".to_string(),
        uri: options
            .source_run_id
            .as_ref()
            .map(|run_id| format!("homeboy://agent-task/run/{run_id}"))
            .unwrap_or_else(|| {
                format!(
                    "homeboy://agent-task/task/{}",
                    options.source_request.task_id
                )
            }),
        revision: None,
    });
    request.source_refs.push(AgentTaskSourceRef {
        kind: "agent-task-promotion".to_string(),
        uri: format!(
            "homeboy://agent-task/promotion/{}/{}",
            options.promotion_report.source.task_id, options.promotion_report.patch_artifact.id
        ),
        revision: None,
    });
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
    request.workspace.root = request
        .workspace
        .root
        .clone()
        .or_else(|| worktree_root_hint(&options.promotion_report));
    request.metadata = json!({
        "cook_loop": {
            "kind": "deterministic-gate-feedback",
            "attempt": next_attempt,
            "previous_attempt": options.attempt,
            "max_attempts": options.max_attempts,
            "source_task_id": options.source_request.task_id,
            "source_run_id": options.source_run_id,
            "failed_gate_count": failed_gates.len(),
            "private_failed_gate_count": failed_gates.iter().filter(|gate| gate.visibility == AgentTaskGateVisibility::Private).count(),
        }
    });
    request
}

fn gate_failure(gate: &AgentTaskGateReport) -> AgentTaskCookLoopGateFailure {
    let command = gate
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.command.clone())
        .unwrap_or_else(|| gate.command.join(" "));
    let stdout_tail = gate
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.stdout_tail.clone())
        .unwrap_or_else(|| text_tail(&gate.stdout, 20));
    let stderr_tail = gate
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.stderr_tail.clone())
        .unwrap_or_else(|| text_tail(&gate.stderr, 20));
    let summary = gate
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.summary.clone())
        .unwrap_or_else(|| {
            format!(
                "deterministic gate failed with exit code {}: {command}",
                gate.exit_code
            )
        });
    let agent_feedback = gate
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.agent_feedback.clone())
        .unwrap_or_else(|| {
            format!(
                "Use the deterministic gate evidence to update the candidate patch so `{command}` passes."
            )
        });

    AgentTaskCookLoopGateFailure {
        gate_id: gate.id.clone(),
        visibility: gate.visibility,
        reveal_policy: gate.reveal_policy,
        command,
        exit_code: gate.exit_code,
        stdout_tail,
        stderr_tail,
        summary,
        agent_feedback,
    }
}

fn agent_visible_gate_failures(
    failed_gates: &[AgentTaskCookLoopGateFailure],
) -> Vec<AgentTaskCookLoopGateFailure> {
    failed_gates
        .iter()
        .map(agent_visible_gate_failure)
        .collect()
}

fn agent_visible_gate_failure(
    failure: &AgentTaskCookLoopGateFailure,
) -> AgentTaskCookLoopGateFailure {
    if failure.visibility == AgentTaskGateVisibility::Visible {
        return failure.clone();
    }

    match failure.reveal_policy {
        AgentTaskGateRevealPolicy::FullEvidence => failure.clone(),
        AgentTaskGateRevealPolicy::SummaryOnly => AgentTaskCookLoopGateFailure {
            gate_id: failure.gate_id.clone(),
            visibility: failure.visibility,
            reveal_policy: failure.reveal_policy,
            command: String::new(),
            exit_code: failure.exit_code,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            summary: format!(
                "private deterministic gate {} failed; detailed evidence is withheld by policy",
                failure.gate_id
            ),
            agent_feedback: "A private deterministic verification gate failed. Generalize the fix against the public objective and visible evidence; hidden evaluator details are withheld.".to_string(),
        },
        AgentTaskGateRevealPolicy::Redacted => AgentTaskCookLoopGateFailure {
            gate_id: failure.gate_id.clone(),
            visibility: failure.visibility,
            reveal_policy: failure.reveal_policy,
            command: String::new(),
            exit_code: failure.exit_code,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            summary: "private deterministic gate failed; evidence redacted".to_string(),
            agent_feedback: "A private deterministic verification gate failed. Details are redacted; continue from the public task objective and visible gate evidence.".to_string(),
        },
        AgentTaskGateRevealPolicy::NoDetail => AgentTaskCookLoopGateFailure {
            gate_id: failure.gate_id.clone(),
            visibility: failure.visibility,
            reveal_policy: failure.reveal_policy,
            command: String::new(),
            exit_code: failure.exit_code,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            summary: "private deterministic gate failed".to_string(),
            agent_feedback: "A private deterministic verification gate failed.".to_string(),
        },
    }
}

fn follow_up_instructions(
    options: &AgentTaskCookLoopOptions,
    failed_gates: &[AgentTaskCookLoopGateFailure],
) -> String {
    let gate_list = failed_gates
        .iter()
        .map(|failure| {
            let gate_label = if failure.command.is_empty() {
                failure.gate_id.as_str()
            } else {
                failure.command.as_str()
            };
            format!(
                "- `{}` exited {}: {}",
                gate_label, failure.exit_code, failure.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let changed_files = if options.promotion_report.changed_files.is_empty() {
        "none reported".to_string()
    } else {
        options.promotion_report.changed_files.join(", ")
    };

    format!(
        "Continue the Homeboy cook loop from the current candidate worktree state.\n\nDeterministic gates failed after Homeboy applied the previous candidate patch. Produce a focused follow-up patch that makes the failed gates pass while preserving the candidate intent.\n\nFailed gates:\n{gate_list}\n\nChanged files in the candidate patch: {changed_files}\n\nUse the structured `inputs.cook_loop` evidence as the primary context. Return an updated patch artifact and concise summary of the fix."
    )
}

fn worktree_root_hint(report: &AgentTaskPromotionReport) -> Option<String> {
    report
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn cook_loop_report_schema() -> String {
    AGENT_TASK_COOK_LOOP_REPORT_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_gate::{AgentTaskGateEnvironment, AgentTaskGateFailureEvidence};
    use crate::core::agent_task_promotion::{
        AgentTaskPromotionArtifactRef, AgentTaskPromotionSource, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
    };

    #[test]
    fn red_gate_creates_follow_up_request_with_failure_evidence_and_diff() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-3676".to_string()),
            current_diff: "diff --git a/src/lib.rs b/src/lib.rs".to_string(),
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::RetryRequested);
        assert_eq!(report.retry_budget_remaining, 2);
        assert_eq!(report.failed_gates.len(), 1);
        let request = report.follow_up_request.expect("follow-up request");
        assert_eq!(request.task_id, "cook-homeboy-gate-fix-2");
        assert!(request.instructions.contains("Deterministic gates failed"));
        assert!(request.instructions.contains("cargo test agent_task_gate"));
        assert_eq!(
            request.inputs["cook_loop"]["failed_gates"][0]["exit_code"],
            101
        );
        assert_eq!(
            request.inputs["cook_loop"]["current_diff"],
            "diff --git a/src/lib.rs b/src/lib.rs"
        );
        assert_eq!(
            request.source_refs[0].uri,
            "homeboy://agent-task/run/run-3676"
        );
        assert_eq!(request.workspace.mode, AgentTaskWorkspaceMode::Existing);
    }

    #[test]
    fn exhausted_retry_budget_stops_without_follow_up_request() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate()],
            ),
            attempt: 2,
            max_attempts: 2,
            source_run_id: Some("run-3676".to_string()),
            current_diff: String::new(),
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::RetriesExhausted);
        assert_eq!(report.retry_budget_remaining, 0);
        assert!(report.follow_up_request.is_none());
        assert_eq!(report.failed_gates[0].stderr_tail, "boom");
    }

    #[test]
    fn green_completion_does_not_create_follow_up_request() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: None,
            current_diff: String::new(),
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::GreenCompleted);
        assert!(report.failed_gates.is_empty());
        assert!(report.follow_up_request.is_none());
        assert_eq!(
            report.quality.classification,
            AgentTaskCookLoopQualityClassification::VerifiedPatch
        );
    }

    #[test]
    fn no_changed_files_are_terminal_noop_feedback() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report_with_changed_files(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
                Vec::new(),
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-4324".to_string()),
            current_diff: String::new(),
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::NoChanges);
        assert_eq!(
            report.quality.classification,
            AgentTaskCookLoopQualityClassification::NoChanges
        );
        assert!(report.quality.summary.contains("no changed files"));
        assert!(report.follow_up_request.is_none());
    }

    #[test]
    fn large_patch_shape_is_flagged_before_success() {
        let changed_files = (0..=20)
            .map(|index| format!("src/generated/file_{index}.rs"))
            .collect();
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report_with_changed_files(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
                changed_files,
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-4327".to_string()),
            current_diff: String::new(),
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::GreenCompleted);
        assert_eq!(
            report.quality.classification,
            AgentTaskCookLoopQualityClassification::LargeOrRiskyPatch
        );
        assert!(report
            .quality
            .signals
            .contains(&"changed_files=21".to_string()));
    }

    #[test]
    fn visible_gate_failure_keeps_full_agent_feedback_evidence() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-3688".to_string()),
            current_diff: String::new(),
            metadata: Value::Null,
        });

        let request = report.follow_up_request.expect("follow-up request");
        let feedback = request.inputs.to_string();
        assert!(feedback.contains("cargo test agent_task_gate"));
        assert!(feedback.contains("boom"));
        assert!(request.instructions.contains("cargo test agent_task_gate"));
    }

    #[test]
    fn private_summary_only_gate_does_not_leak_command_or_output_to_follow_up_request() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![private_failed_gate(AgentTaskGateRevealPolicy::SummaryOnly)],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-3688".to_string()),
            current_diff: String::new(),
            metadata: Value::Null,
        });

        assert_eq!(
            report.failed_gates[0].command,
            "./hidden-heldout-check --fixture secret"
        );
        assert_eq!(
            report.failed_gates[0].stdout_tail,
            "secret fixture mismatch"
        );
        let request = report.follow_up_request.expect("follow-up request");
        let agent_context = format!("{}\n{}", request.instructions, request.inputs);

        assert!(agent_context.contains("private deterministic gate gate-1 failed"));
        assert!(agent_context.contains("hidden evaluator details are withheld"));
        assert!(!agent_context.contains("./hidden-heldout-check"));
        assert!(!agent_context.contains("secret fixture mismatch"));
        assert!(!agent_context.contains("private evaluator stack trace"));
    }

    #[test]
    fn private_full_evidence_policy_can_reveal_agent_feedback_details() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![private_failed_gate(AgentTaskGateRevealPolicy::FullEvidence)],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-3688".to_string()),
            current_diff: String::new(),
            metadata: Value::Null,
        });

        let request = report.follow_up_request.expect("follow-up request");
        let agent_context = format!("{}\n{}", request.instructions, request.inputs);

        assert!(agent_context.contains("./hidden-heldout-check"));
        assert!(agent_context.contains("secret fixture mismatch"));
    }

    fn source_request() -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "cook-homeboy".to_string(),
            group_key: Some("cook".to_string()),
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("fixture".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "Cook the issue".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: vec!["patch".to_string()],
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }
    }

    fn promotion_report(
        status: AgentTaskPromotionStatus,
        deterministic_gates: Vec<AgentTaskGateReport>,
    ) -> AgentTaskPromotionReport {
        promotion_report_with_changed_files(
            status,
            deterministic_gates,
            vec!["src/core/agent_task_gate.rs".to_string()],
        )
    }

    fn promotion_report_with_changed_files(
        status: AgentTaskPromotionStatus,
        deterministic_gates: Vec<AgentTaskGateReport>,
        changed_files: Vec<String>,
    ) -> AgentTaskPromotionReport {
        AgentTaskPromotionReport {
            schema: AGENT_TASK_PROMOTION_REPORT_SCHEMA.to_string(),
            status,
            source: AgentTaskPromotionSource {
                kind: "aggregate".to_string(),
                task_id: "cook-homeboy".to_string(),
                path: Some("aggregate.json".to_string()),
            },
            to_worktree: "homeboy@fix-3676".to_string(),
            patch_artifact: AgentTaskPromotionArtifactRef {
                id: "patch".to_string(),
                kind: "patch".to_string(),
                path: "changes.patch".to_string(),
                sha256: Some("abc123".to_string()),
            },
            changed_files,
            command_evidence: Vec::new(),
            deterministic_gates,
            gate_results: Vec::new(),
            provenance: json!({ "worktree_path": "/tmp/homeboy@fix-3676" }),
        }
    }

    fn failed_gate() -> AgentTaskGateReport {
        AgentTaskGateReport::new(
            "gate-1",
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test agent_task_gate".to_string(),
            ],
            101,
            "running tests",
            "boom",
            Some(AgentTaskGateFailureEvidence {
                summary: "agent_task_gate failed".to_string(),
                command: "cargo test agent_task_gate".to_string(),
                exit_code: 101,
                stdout_tail: "running tests".to_string(),
                stderr_tail: "boom".to_string(),
                agent_feedback: "Update the patch so cargo test agent_task_gate passes."
                    .to_string(),
            }),
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            AgentTaskGateEnvironment::default(),
        )
    }

    fn green_gate() -> AgentTaskGateReport {
        AgentTaskGateReport::new(
            "gate-1",
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test".to_string(),
            ],
            0,
            "ok",
            String::new(),
            None,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            AgentTaskGateEnvironment::default(),
        )
    }

    fn private_failed_gate(reveal_policy: AgentTaskGateRevealPolicy) -> AgentTaskGateReport {
        AgentTaskGateReport::new(
            "gate-1",
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "./hidden-heldout-check --fixture secret".to_string(),
            ],
            7,
            "secret fixture mismatch",
            "private evaluator stack trace",
            Some(AgentTaskGateFailureEvidence {
                summary: "secret fixture mismatch on randomized private corpus".to_string(),
                command: "./hidden-heldout-check --fixture secret".to_string(),
                exit_code: 7,
                stdout_tail: "secret fixture mismatch".to_string(),
                stderr_tail: "private evaluator stack trace".to_string(),
                agent_feedback: "Fix the randomized secret fixture mismatch.".to_string(),
            }),
            AgentTaskGateVisibility::Private,
            reveal_policy,
            AgentTaskGateEnvironment::default(),
        )
    }
}

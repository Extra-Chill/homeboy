use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

use crate::agent_task::{
    AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkspaceMode, AGENT_TASK_REQUEST_SCHEMA,
};
use crate::agent_task_gate::{
    text_tail, AgentTaskGateDiagnosticProducer, AgentTaskGateDiagnosticRecord,
    AgentTaskGateFailureClassification, AgentTaskGateReport, AgentTaskGateRevealPolicy,
    AgentTaskGateStatus, AgentTaskGateVisibility,
};
use crate::agent_task_promotion::{AgentTaskPromotionReport, AgentTaskPromotionStatus};
use crate::agent_task_review_dossier::{review_form_output_declaration, AiFilledReviewForm};
use homeboy_core::gate::{HomeboyGateResult, HomeboyGateStatus};

pub const AGENT_TASK_COOK_FEEDBACK_REPORT_SCHEMA: &str =
    "homeboy/agent-task-cook-feedback-report/v1";
const RISKY_CHANGED_FILE_THRESHOLD: usize = 20;
const MAX_FAILURE_DIAGNOSTICS: usize = 8;
const MAX_FAILURE_DELTA_IDENTITIES: usize = 8;
const MAX_DIAGNOSTIC_FIELD_BYTES: usize = 512;
const MAX_DIAGNOSTIC_ACTIONS: usize = 4;
/// A review form is metadata, not a coding attempt. Callers may lower this via
/// `metadata.cook_loop.review_form_timeout_ms`; the cap keeps it bounded.
const DEFAULT_REVIEW_FORM_TIMEOUT_MS: u64 = 60_000;
const MAX_REVIEW_FORM_TIMEOUT_MS: u64 = 120_000;

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
    /// Whether the AI-authored review form is a required gate for this
    /// evaluation. Cook finalization enables it (a green change is not "done"
    /// until a valid form exists); standalone/diagnostic evaluations leave it
    /// off so they never demand a form the caller isn't producing.
    #[serde(default)]
    pub require_review_form: bool,
    /// The AI-authored review form parsed off the terminal attempt outcome, if
    /// the agent emitted one. A green cook is not "done" until this is present
    /// and valid — a missing/invalid form nudges another attempt exactly like a
    /// red deterministic gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_form: Option<AiFilledReviewForm>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskCookLoopReport {
    #[serde(default = "cook_feedback_report_schema")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intentional_no_change: Option<AgentTaskIntentionalNoChange>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskCookLoopStatus {
    GreenCompleted,
    /// The candidate and immutable baseline failed identically. The candidate
    /// is not a regression, but required verification is still red.
    BaselineRed,
    IntentionalNoChange,
    NoChanges,
    NoOpGateFailed,
    RetryRequested,
    RetriesExhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookLoopQualityReport {
    pub classification: AgentTaskCookLoopQualityClassification,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_progression: Option<AgentTaskCookLoopFailureProgression>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskCookLoopQualityClassification {
    NoChanges,
    PatchProduced,
    LargeOrRiskyPatch,
    VerifiedPatch,
    VerifiedNoOp,
    IntentionalNoChange,
    Regressing,
    Stagnating,
}

/// Provider-declared disposition for a review that intentionally produced no
/// patch. The declaration is accepted only after provider normalization binds
/// it to a clean, inspected workspace revision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskIntentionalNoChange {
    pub schema: String,
    pub verdict: AgentTaskIntentionalNoChangeVerdict,
    pub inspected_revision: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next_action: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskIntentionalNoChangeVerdict {
    Blocked,
    AlreadySatisfied,
    /// Legacy `no_change` declarations deserialize as this unambiguous review
    /// outcome, preserving existing providers while exposing the typed contract.
    #[serde(alias = "no_change")]
    InvestigationOnly,
}

impl std::fmt::Display for AgentTaskIntentionalNoChangeVerdict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Blocked => "blocked",
            Self::AlreadySatisfied => "already_satisfied",
            Self::InvestigationOnly => "investigation_only",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookLoopFailureProgression {
    pub status: AgentTaskCookLoopFailureProgressionStatus,
    pub previous_count: usize,
    pub current_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unchanged: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskCookLoopFailureProgressionStatus {
    Initial,
    Improving,
    Regressing,
    Stagnating,
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
    #[serde(default)]
    pub classification: AgentTaskGateFailureClassification,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout_tail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr_tail: String,
    pub summary: String,
    pub agent_feedback: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskCookLoopFailureDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_evidence_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookLoopFailureDiagnostic {
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

pub fn evaluate_cook_loop(options: AgentTaskCookLoopOptions) -> AgentTaskCookLoopReport {
    let failed_gates: Vec<AgentTaskCookLoopGateFailure> = options
        .promotion_report
        .deterministic_gates
        .iter()
        .filter(|gate| {
            matches!(
                gate.status,
                AgentTaskGateStatus::Failed | AgentTaskGateStatus::AcceptedInheritedFailure
            )
        })
        .map(|gate| gate_failure(gate, options.source_run_id.as_deref()))
        .collect();
    let failed_gate_results: Vec<HomeboyGateResult> = options
        .promotion_report
        .gate_outcome()
        .gate_results
        .into_iter()
        .filter(|gate| gate.status == HomeboyGateStatus::Failed)
        .collect();
    let retry_budget_remaining = options.max_attempts.saturating_sub(options.attempt);
    let baseline_red = options
        .promotion_report
        .deterministic_gates
        .iter()
        .any(|gate| {
            gate.status == AgentTaskGateStatus::AcceptedInheritedFailure
                && gate.baseline_comparison.as_ref().is_some_and(|comparison| {
                    comparison.result
                        == crate::agent_task_gate::AgentTaskGateDifferentialResult::BaselineRed
                        && comparison.matches_candidate_failure
                })
        });
    let intentional_no_change = (options.promotion_report.status
        == AgentTaskPromotionStatus::VerifiedNoChanges)
        .then(|| {
            options
                .metadata
                .get("intentional_no_change")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
        })
        .flatten();
    let mut quality =
        classify_cook_loop_quality(&options.promotion_report, intentional_no_change.is_some());
    let failure_progression = failure_progression(&options, &failed_gates);
    apply_failure_progression_to_quality(&mut quality, &failure_progression);
    let should_retry = options.promotion_report.status == AgentTaskPromotionStatus::GateFailed
        && !failed_gates.is_empty()
        && failed_gates
            .iter()
            .all(|gate| gate.classification == AgentTaskGateFailureClassification::CandidateCode)
        && !baseline_red
        && retry_budget_remaining > 0;
    // Deterministic gates take precedence: a red gate must be fixed before the
    // review form is even worth requesting. Only once the change itself is green
    // (and actually produced changes) does the AI-authored form become the last
    // outstanding "gate".
    let gates_green_with_changes = failed_gates.is_empty()
        && options.promotion_report.status == AgentTaskPromotionStatus::Applied;
    let review_form_gap = (options.require_review_form && gates_green_with_changes)
        .then(|| review_form_requirement_gap(&options.review_form));

    let follow_up_request = if should_retry {
        Some(build_follow_up_request(
            &options,
            // Ordered fail-fast guarantees this is the only failure. Continue-all
            // still repairs the first declared failure before broad follow-up work.
            &failed_gates[..1],
            &failure_progression,
        ))
    } else if let Some(Some(gap)) = &review_form_gap {
        // Gates are green but the AI form is missing/invalid: nudge another
        // attempt with actionable feedback, exactly like a red gate — unless the
        // retry budget is exhausted.
        (retry_budget_remaining > 0).then(|| build_review_form_follow_up_request(&options, gap))
    } else {
        None
    };

    let status = if follow_up_request.is_some() {
        AgentTaskCookLoopStatus::RetryRequested
    } else if options.promotion_report.status == AgentTaskPromotionStatus::NoChangesGateFailed {
        AgentTaskCookLoopStatus::NoOpGateFailed
    } else if intentional_no_change.is_some() {
        AgentTaskCookLoopStatus::IntentionalNoChange
    } else if quality.classification == AgentTaskCookLoopQualityClassification::NoChanges {
        AgentTaskCookLoopStatus::NoChanges
    } else if baseline_red {
        AgentTaskCookLoopStatus::BaselineRed
    } else if !failed_gates.is_empty() {
        AgentTaskCookLoopStatus::RetriesExhausted
    } else if matches!(&review_form_gap, Some(Some(_))) {
        // Green change, but the agent never produced a valid form and the retry
        // budget is spent. The cook does not publish a PR without the form.
        AgentTaskCookLoopStatus::RetriesExhausted
    } else {
        AgentTaskCookLoopStatus::GreenCompleted
    };

    AgentTaskCookLoopReport {
        schema: AGENT_TASK_COOK_FEEDBACK_REPORT_SCHEMA.to_string(),
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
        intentional_no_change,
        metadata: report_metadata(options.metadata, &failure_progression, baseline_red),
    }
}

fn classify_cook_loop_quality(
    report: &AgentTaskPromotionReport,
    intentional_no_change: bool,
) -> AgentTaskCookLoopQualityReport {
    let changed_file_count = report.changed_files.len();
    if changed_file_count == 0 {
        if intentional_no_change {
            return AgentTaskCookLoopQualityReport {
                classification: AgentTaskCookLoopQualityClassification::IntentionalNoChange,
                summary: "provider intentionally completed review without a candidate patch"
                    .to_string(),
                signals: vec![
                    "changed_files=0".to_string(),
                    "intentional_no_change=verified".to_string(),
                ],
                failure_progression: None,
            };
        }
        if report.status == AgentTaskPromotionStatus::VerifiedNoChanges {
            return AgentTaskCookLoopQualityReport {
                classification: AgentTaskCookLoopQualityClassification::VerifiedNoOp,
                summary:
                    "provider produced no patch and the pinned candidate passed deterministic gates"
                        .to_string(),
                signals: vec![
                    "changed_files=0".to_string(),
                    "verification=passed".to_string(),
                ],
                failure_progression: None,
            };
        }
        return AgentTaskCookLoopQualityReport {
            classification: AgentTaskCookLoopQualityClassification::NoChanges,
            summary: "cook produced no changed files; task likely still requires review or retry"
                .to_string(),
            signals: vec!["changed_files=0".to_string()],
            failure_progression: None,
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
            failure_progression: None,
        };
    }

    let outcome = report.gate_outcome();
    let has_passed_gate = outcome
        .gate_results
        .iter()
        .any(|gate| gate.status == HomeboyGateStatus::Passed);
    if outcome.status == AgentTaskPromotionStatus::Applied && has_passed_gate {
        return AgentTaskCookLoopQualityReport {
            classification: AgentTaskCookLoopQualityClassification::VerifiedPatch,
            summary: "cook produced a patch and deterministic gates passed".to_string(),
            signals,
            failure_progression: None,
        };
    }

    AgentTaskCookLoopQualityReport {
        classification: AgentTaskCookLoopQualityClassification::PatchProduced,
        summary: "cook produced a patch that needs promotion or verification".to_string(),
        signals,
        failure_progression: None,
    }
}

fn build_follow_up_request(
    options: &AgentTaskCookLoopOptions,
    failed_gates: &[AgentTaskCookLoopGateFailure],
    failure_progression: &AgentTaskCookLoopFailureProgression,
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
    request.instructions =
        follow_up_instructions(options, &agent_visible_failed_gates, failure_progression);
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
            "failure_set": failure_set(failed_gates),
            "failure_progression": failure_progression,
            "retry_policy": retry_policy(failure_progression),
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
    request.policy.grant_workspace_read_tool();
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
            "failure_progression": failure_progression,
            "retry_policy": retry_policy(failure_progression),
        }
    });
    request
}

/// Returns `Some(feedback)` describing why the AI review form is not yet
/// acceptable (absent or failing validation), or `None` when the form is valid.
fn review_form_requirement_gap(form: &Option<AiFilledReviewForm>) -> Option<String> {
    match form {
        None => Some(format!(
            "The change passed deterministic gates but no review form was emitted. {}",
            AiFilledReviewForm::requirement_feedback()
        )),
        Some(form) => form.validate().err().map(|error| error.message),
    }
}

/// Build a follow-up attempt request that nudges the agent to emit a valid
/// review form. Mirrors `build_follow_up_request` but the outstanding work is
/// authoring the review form, not fixing a red gate.
fn build_review_form_follow_up_request(
    options: &AgentTaskCookLoopOptions,
    gap_feedback: &str,
) -> AgentTaskRequest {
    let mut request = options.source_request.clone();
    let next_attempt = options.attempt.saturating_add(1);
    request.schema = AGENT_TASK_REQUEST_SCHEMA.to_string();
    request.task_id = format!("{}-review-form-{}", request.task_id, next_attempt);
    request.parent_plan_id = request
        .parent_plan_id
        .clone()
        .or_else(|| options.source_run_id.clone());
    request.instructions = format!(
        "Continue the Homeboy cook loop from the current candidate worktree state.\n\nThe change is complete and deterministic gates passed. This attempt supplies the reviewer-facing review form that completes the pull request dossier.\n\n{gap_feedback}\n\nPreserve the candidate code and return the complete `review_form` object in your task outputs.",
    );
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
            "review_form_required": true,
            "review_form_feedback": gap_feedback,
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
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
    request.workspace.root = request
        .workspace
        .root
        .clone()
        .or_else(|| worktree_root_hint(&options.promotion_report));
    // The promoted candidate is authoritative. This task may inspect it but
    // cannot produce or replace code artifacts.
    let review_form_timeout_ms = review_form_timeout_ms(&options.source_request);
    request.expected_artifacts.clear();
    request.artifact_declarations.clear();
    request.output_declarations = vec![review_form_output_declaration()];
    request.limits.timeout_ms = Some(review_form_timeout_ms);
    request.component_contracts.clear();
    request.runtime_tools.clear();
    request.executor.runtime_selection = None;
    request.policy.tools = Default::default();
    request.policy.grant_workspace_read_tool();
    request.executor.required_capabilities = vec!["structured_outcome".to_string()];
    request.executor.secret_env.clear();
    // Provider configuration is an opaque, provider-owned dispatch contract.
    // Keeping it avoids dropping routing or credential-source configuration
    // while the explicit secret attachments and runtime authority stay removed.
    request.policy.write = "none".to_string();
    request.policy.apply = "none".to_string();
    request.metadata = json!({
        "cook_loop": {
            "kind": "review_form_only",
            "attempt": next_attempt,
            "previous_attempt": options.attempt,
            "max_attempts": options.max_attempts,
            "source_task_id": options.source_request.task_id,
            "source_run_id": options.source_run_id,
            "review_form_timeout_ms": review_form_timeout_ms,
        }
    });
    request
}

fn review_form_timeout_ms(source_request: &AgentTaskRequest) -> u64 {
    source_request.metadata["cook_loop"]["review_form_timeout_ms"]
        .as_u64()
        .filter(|timeout_ms| *timeout_ms > 0)
        .map(|timeout_ms| timeout_ms.min(MAX_REVIEW_FORM_TIMEOUT_MS))
        .unwrap_or(DEFAULT_REVIEW_FORM_TIMEOUT_MS)
}

fn gate_failure(
    gate: &AgentTaskGateReport,
    source_run_id: Option<&str>,
) -> AgentTaskCookLoopGateFailure {
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

    let diagnostics = gate
        .failure_evidence
        .as_ref()
        .map(|evidence| bounded_diagnostics(&evidence.diagnostics))
        .unwrap_or_default();
    let full_evidence_ref = source_run_id.map(|run_id| gate_evidence_ref(run_id, &gate.id));
    AgentTaskCookLoopGateFailure {
        gate_id: gate.id.clone(),
        visibility: gate.visibility,
        reveal_policy: gate.reveal_policy,
        command,
        exit_code: gate.exit_code,
        classification: gate
            .failure_evidence
            .as_ref()
            .map(|evidence| evidence.classification)
            .unwrap_or_default(),
        stdout_tail,
        stderr_tail,
        summary,
        agent_feedback,
        diagnostics,
        full_evidence_ref,
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
            classification: failure.classification,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            summary: format!(
                "private deterministic gate {} failed; detailed evidence is withheld by policy",
                failure.gate_id
            ),
            agent_feedback: "A private deterministic verification gate failed. Generalize the fix against the public objective and visible evidence; hidden evaluator details are withheld.".to_string(),
            diagnostics: Vec::new(),
            full_evidence_ref: None,
        },
        AgentTaskGateRevealPolicy::Redacted => AgentTaskCookLoopGateFailure {
            gate_id: failure.gate_id.clone(),
            visibility: failure.visibility,
            reveal_policy: failure.reveal_policy,
            command: String::new(),
            exit_code: failure.exit_code,
            classification: failure.classification,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            summary: "private deterministic gate failed; evidence redacted".to_string(),
            agent_feedback: "A private deterministic verification gate failed. Details are redacted; continue from the public task objective and visible gate evidence.".to_string(),
            diagnostics: Vec::new(),
            full_evidence_ref: None,
        },
        AgentTaskGateRevealPolicy::NoDetail => AgentTaskCookLoopGateFailure {
            gate_id: failure.gate_id.clone(),
            visibility: failure.visibility,
            reveal_policy: failure.reveal_policy,
            command: String::new(),
            exit_code: failure.exit_code,
            classification: failure.classification,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            summary: "private deterministic gate failed".to_string(),
            agent_feedback: "A private deterministic verification gate failed.".to_string(),
            diagnostics: Vec::new(),
            full_evidence_ref: None,
        },
    }
}

fn follow_up_instructions(
    options: &AgentTaskCookLoopOptions,
    failed_gates: &[AgentTaskCookLoopGateFailure],
    failure_progression: &AgentTaskCookLoopFailureProgression,
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

    let priority = match failure_progression.status {
        AgentTaskCookLoopFailureProgressionStatus::Regressing => format!(
            "The failure set regressed. Fix these newly introduced failures first: {}.",
            failure_progression.added.join(", ")
        ),
        AgentTaskCookLoopFailureProgressionStatus::Stagnating => format!(
            "The failure set is unchanged. Focus on shared root causes across: {}.",
            failure_progression.unchanged.join(", ")
        ),
        AgentTaskCookLoopFailureProgressionStatus::Improving => {
            "The failure set improved. Preserve resolved failures while fixing the remaining shared failures.".to_string()
        }
        AgentTaskCookLoopFailureProgressionStatus::Initial => {
            "Start with the producer-provided diagnostics and suggested actions.".to_string()
        }
    };

    format!(
        "Continue the Homeboy cook loop from the current candidate worktree state.\n\nDeterministic gates failed after Homeboy applied the previous candidate patch. Produce a focused follow-up patch that makes the failed gates pass while preserving the candidate intent. {priority}\n\nFailed gates:\n{gate_list}\n\nChanged files in the candidate patch: {changed_files}\n\nThe structured `inputs.cook_loop` object already contains the gate evidence and current diff. Inspect repository files relative to the current workspace root, then return an updated patch artifact and concise summary of the fix."
    )
}

fn report_metadata(
    metadata: Value,
    progression: &AgentTaskCookLoopFailureProgression,
    baseline_red: bool,
) -> Value {
    let mut metadata = metadata.as_object().cloned().unwrap_or_default();
    metadata.insert("failure_progression".to_string(), json!(progression));
    if baseline_red {
        metadata.insert("baseline_red".to_string(), Value::Bool(true));
        metadata.insert(
            "failure_origin".to_string(),
            Value::String("inherited_infrastructure".to_string()),
        );
    }
    Value::Object(metadata)
}

fn retry_policy(progression: &AgentTaskCookLoopFailureProgression) -> &'static str {
    match progression.status {
        AgentTaskCookLoopFailureProgressionStatus::Regressing => "new_failures_first",
        AgentTaskCookLoopFailureProgressionStatus::Stagnating => "shared_root_cause",
        AgentTaskCookLoopFailureProgressionStatus::Improving => "preserve_resolved_failures",
        AgentTaskCookLoopFailureProgressionStatus::Initial => "producer_diagnostics",
    }
}

fn failure_set(failed_gates: &[AgentTaskCookLoopGateFailure]) -> Vec<String> {
    failed_gates
        .iter()
        .flat_map(|failure| {
            if failure.diagnostics.is_empty() {
                vec![failure.gate_id.clone()]
            } else {
                failure
                    .diagnostics
                    .iter()
                    .map(|diagnostic| format!("{}:{}", failure.gate_id, diagnostic.identity))
                    .collect()
            }
        })
        .collect()
}

fn previous_failure_set(options: &AgentTaskCookLoopOptions) -> Vec<String> {
    options
        .metadata
        .get("previous_failure_set")
        .or_else(|| {
            options
                .source_request
                .inputs
                .pointer("/cook_loop/failure_set")
        })
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn failure_progression(
    options: &AgentTaskCookLoopOptions,
    failed_gates: &[AgentTaskCookLoopGateFailure],
) -> AgentTaskCookLoopFailureProgression {
    let previous: BTreeSet<_> = previous_failure_set(options).into_iter().collect();
    let current: BTreeSet<_> = failure_set(failed_gates).into_iter().collect();
    let limit = |items: BTreeSet<String>| -> Vec<String> {
        items
            .into_iter()
            .take(MAX_FAILURE_DELTA_IDENTITIES)
            .collect()
    };
    let added_set: BTreeSet<_> = current.difference(&previous).cloned().collect();
    let removed_set: BTreeSet<_> = previous.difference(&current).cloned().collect();
    let has_added = !added_set.is_empty();
    let added = limit(added_set);
    let removed = limit(removed_set);
    let unchanged = limit(current.intersection(&previous).cloned().collect());
    let status = if previous.is_empty() {
        AgentTaskCookLoopFailureProgressionStatus::Initial
    } else if has_added || current.len() > previous.len() {
        AgentTaskCookLoopFailureProgressionStatus::Regressing
    } else if current == previous {
        AgentTaskCookLoopFailureProgressionStatus::Stagnating
    } else {
        AgentTaskCookLoopFailureProgressionStatus::Improving
    };
    AgentTaskCookLoopFailureProgression {
        status,
        previous_count: previous.len(),
        current_count: current.len(),
        added,
        removed,
        unchanged,
    }
}

fn apply_failure_progression_to_quality(
    quality: &mut AgentTaskCookLoopQualityReport,
    progression: &AgentTaskCookLoopFailureProgression,
) {
    if matches!(
        progression.status,
        AgentTaskCookLoopFailureProgressionStatus::Regressing
    ) {
        quality.classification = AgentTaskCookLoopQualityClassification::Regressing;
        quality.summary =
            "candidate regressed deterministic gate failures; fix newly introduced failures first"
                .to_string();
    } else if matches!(
        progression.status,
        AgentTaskCookLoopFailureProgressionStatus::Stagnating
    ) {
        quality.classification = AgentTaskCookLoopQualityClassification::Stagnating;
        quality.summary =
            "candidate did not reduce deterministic gate failures; focus on shared root causes"
                .to_string();
    }
    quality
        .signals
        .push(format!("failure_progression={:?}", progression.status).to_ascii_lowercase());
    quality.failure_progression = Some(progression.clone());
}

fn bounded_diagnostics(
    records: &[AgentTaskGateDiagnosticRecord],
) -> Vec<AgentTaskCookLoopFailureDiagnostic> {
    let mut identities = BTreeSet::new();
    records
        .iter()
        .filter(|record| record_is_complete(record) && identities.insert(record.identity.clone()))
        .take(MAX_FAILURE_DIAGNOSTICS)
        .map(|record| AgentTaskCookLoopFailureDiagnostic {
            schema: bounded_text(&record.schema, MAX_DIAGNOSTIC_FIELD_BYTES),
            identity: bounded_text(&record.identity, MAX_DIAGNOSTIC_FIELD_BYTES),
            summary: bounded_text(&record.summary, MAX_DIAGNOSTIC_FIELD_BYTES),
            source_location: record
                .source_location
                .as_deref()
                .map(|value| bounded_text(value, MAX_DIAGNOSTIC_FIELD_BYTES)),
            suggested_actions: record
                .suggested_actions
                .iter()
                .take(MAX_DIAGNOSTIC_ACTIONS)
                .map(|action| bounded_text(action, MAX_DIAGNOSTIC_FIELD_BYTES))
                .collect(),
            producer: AgentTaskGateDiagnosticProducer {
                id: bounded_text(&record.producer.id, MAX_DIAGNOSTIC_FIELD_BYTES),
                schema: bounded_text(&record.producer.schema, MAX_DIAGNOSTIC_FIELD_BYTES),
            },
            full_evidence_ref: bounded_text(&record.full_evidence_ref, MAX_DIAGNOSTIC_FIELD_BYTES),
        })
        .collect()
}

fn record_is_complete(record: &AgentTaskGateDiagnosticRecord) -> bool {
    !record.schema.is_empty()
        && !record.identity.is_empty()
        && !record.summary.is_empty()
        && !record.producer.id.is_empty()
        && !record.producer.schema.is_empty()
        && !record.full_evidence_ref.is_empty()
}

fn bounded_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn gate_evidence_ref(run_id: &str, gate_id: &str) -> String {
    format!("homeboy://agent-task/run/{run_id}/gates#gate={gate_id}")
}

fn worktree_root_hint(report: &AgentTaskPromotionReport) -> Option<String> {
    report
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn cook_feedback_report_schema() -> String {
    AGENT_TASK_COOK_FEEDBACK_REPORT_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskComponentContract, AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy,
        AgentTaskRuntimeSelection, AgentTaskWorkspace, AgentToolExecutionLocation,
        AgentToolPolicyRule, AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::agent_task_gate::{AgentTaskGateEnvironment, AgentTaskGateFailureEvidence};
    use crate::agent_task_promotion::{
        AgentTaskPromotionArtifactRef, AgentTaskPromotionNotification, AgentTaskPromotionSource,
        AgentTaskPromotionTarget, AGENT_TASK_PROMOTION_REPORT_SCHEMA,
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
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::RetryRequested);
        assert_eq!(report.retry_budget_remaining, 2);
        assert_eq!(report.failed_gates.len(), 1);
        let request = report.follow_up_request.expect("follow-up request");
        assert_eq!(request.task_id, "cook-homeboy-gate-fix-2");
        assert!(request.instructions.contains("Deterministic gates failed"));
        assert!(request.instructions.contains("opaque-gate"));
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
        assert!(request.policy.permits_workspace_read_tool());
        assert_eq!(
            request.policy.tools.execution_location_for("read"),
            AgentToolExecutionLocation::Runner
        );
        assert_eq!(request.policy.write, "artifacts_only");
    }

    #[test]
    fn gate_declaration_failure_never_creates_code_remediation() {
        let mut gate = failed_gate();
        gate.failure_evidence
            .as_mut()
            .expect("failure evidence")
            .classification = AgentTaskGateFailureClassification::GateDeclaration;
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(AgentTaskPromotionStatus::GateFailed, vec![gate]),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-declaration".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::RetriesExhausted);
        assert!(report.follow_up_request.is_none());
        assert_eq!(
            report.failed_gates[0].classification,
            AgentTaskGateFailureClassification::GateDeclaration
        );
    }

    #[test]
    fn red_gate_preserves_executor_provider_configuration() {
        let mut source = source_request();
        source.executor.model = Some("provider/model".to_string());
        source.executor.config = json!({
            "client_context": { "conversation_id": "distinct-context" },
            "provider_plugin_paths": ["/provider/plugin"],
            "runtime_env": { "OPENCODE_CONFIG_CONTENT": "distinct-policy" },
            "runtime_overlays": [{ "source": "/runtime/overlay" }],
            "workspace_root": "/source/workspace",
        });

        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source.clone(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-provider-policy".to_string()),
            current_diff: "diff --git a/src/lib.rs b/src/lib.rs".to_string(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });

        let request = report.follow_up_request.expect("follow-up request");
        assert_eq!(request.executor, source.executor);
        assert!(request
            .instructions
            .contains("already contains the gate evidence and current diff"));
        assert!(request
            .instructions
            .contains("relative to the current workspace root"));
    }

    #[test]
    fn continue_all_feedback_uses_the_first_failing_gate() {
        let mut later_failure = failed_gate();
        later_failure.id = "gate-2".to_string();
        later_failure.command[2] = "broad-gate".to_string();
        later_failure
            .failure_evidence
            .as_mut()
            .expect("evidence")
            .command = "broad-gate".to_string();
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate(), later_failure],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-first-failure".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });

        assert_eq!(report.failed_gates.len(), 2);
        let request = report.follow_up_request.expect("follow-up request");
        assert_eq!(
            request.inputs["cook_loop"]["failed_gates"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            request.inputs["cook_loop"]["failed_gates"][0]["gate_id"],
            "gate-1"
        );
        assert!(!request.instructions.contains("broad-gate"));
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
            require_review_form: false,
            review_form: None,
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
            require_review_form: false,
            review_form: None,
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
    fn baseline_comparison_keeps_required_gate_truthful() {
        let mut inherited = failed_gate();
        inherited.status = AgentTaskGateStatus::AcceptedInheritedFailure;
        inherited.baseline_comparison =
            Some(crate::agent_task_gate::AgentTaskGateBaselineComparison {
                base_ref: "base".to_string(),
                exit_code: 1,
                failure_fingerprint: "rustup unavailable".to_string(),
                matches_candidate_failure: true,
                result: crate::agent_task_gate::AgentTaskGateDifferentialResult::BaselineRed,
            });
        let baseline_red = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![inherited],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-inherited".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });
        assert_eq!(baseline_red.status, AgentTaskCookLoopStatus::BaselineRed);
        assert_eq!(baseline_red.failed_gates.len(), 1);
        assert!(baseline_red.follow_up_request.is_none());
        assert_eq!(
            baseline_red.metadata["failure_origin"],
            "inherited_infrastructure"
        );

        let candidate_only_failure = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-regression".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });
        assert_eq!(
            candidate_only_failure.status,
            AgentTaskCookLoopStatus::RetryRequested
        );

        // A red baseline does not taint a candidate that passed its required gate.
        let baseline_only_failure = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-baseline-only".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });
        assert_eq!(
            baseline_only_failure.status,
            AgentTaskCookLoopStatus::GreenCompleted
        );

        let true_pass = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-true-pass".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });
        assert_eq!(true_pass.status, AgentTaskCookLoopStatus::GreenCompleted);
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
            require_review_form: false,
            review_form: None,
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
    fn verified_no_op_completes_and_failed_no_op_is_terminal_failure() {
        let intentional = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report_with_changed_files(
                AgentTaskPromotionStatus::VerifiedNoChanges,
                vec![green_gate()],
                Vec::new(),
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-intentional-no-change".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: json!({
                "intentional_no_change": {
                    "schema": "homeboy/intentional-no-change/v1",
                    "verdict": "blocked",
                    "inspected_revision": "abc123",
                    "next_action": "Add the missing owning-layer contract.",
                    "source_evidence": ["homeboy://evidence/provider-review"],
                }
            }),
        });
        assert_eq!(
            intentional.status,
            AgentTaskCookLoopStatus::IntentionalNoChange
        );
        assert_eq!(
            intentional.quality.classification,
            AgentTaskCookLoopQualityClassification::IntentionalNoChange
        );
        let declaration = intentional.intentional_no_change.unwrap();
        assert_eq!(
            declaration.verdict,
            AgentTaskIntentionalNoChangeVerdict::Blocked
        );
        assert_eq!(
            declaration.next_action,
            "Add the missing owning-layer contract."
        );
        assert_eq!(
            declaration.source_evidence,
            vec!["homeboy://evidence/provider-review"]
        );
        assert!(intentional.follow_up_request.is_none());

        let verified = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report_with_changed_files(
                AgentTaskPromotionStatus::VerifiedNoChanges,
                vec![green_gate()],
                Vec::new(),
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-verified-no-op".to_string()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: None,
            metadata: Value::Null,
        });
        assert_eq!(verified.status, AgentTaskCookLoopStatus::GreenCompleted);
        assert_eq!(
            verified.quality.classification,
            AgentTaskCookLoopQualityClassification::VerifiedNoOp
        );
        assert!(verified.follow_up_request.is_none());

        let failed = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report_with_changed_files(
                AgentTaskPromotionStatus::NoChangesGateFailed,
                vec![failed_gate()],
                Vec::new(),
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-failed-no-op".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });
        assert_eq!(failed.status, AgentTaskCookLoopStatus::NoOpGateFailed);
        assert!(failed.follow_up_request.is_none());
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
            require_review_form: false,
            review_form: None,
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
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });

        let request = report.follow_up_request.expect("follow-up request");
        let feedback = request.inputs.to_string();
        assert!(feedback.contains("opaque-gate"));
        assert!(feedback.contains("boom"));
        assert!(request.instructions.contains("opaque-gate"));
    }

    #[test]
    fn producer_diagnostic_record_is_bounded_without_interpreting_its_contents() {
        let diagnostic: AgentTaskGateDiagnosticRecord = serde_json::from_str(include_str!(
            "../../../tests/fixtures/agent_task_gate_feedback/producer-diagnostic-record.json"
        ))
        .expect("producer diagnostic fixture");
        let gate = AgentTaskGateReport::new(
            "gate-1",
            vec!["opaque-gate".to_string()],
            1,
            "producer output is opaque",
            String::new(),
            Some(AgentTaskGateFailureEvidence {
                classification: AgentTaskGateFailureClassification::CandidateCode,
                summary: "producer reported a failure".to_string(),
                command: "opaque-gate".to_string(),
                exit_code: 1,
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                agent_feedback: "Use the structured diagnostic.".to_string(),
                diagnostics: vec![diagnostic],
            }),
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            AgentTaskGateEnvironment::default(),
        );

        let failure = gate_failure(&gate, Some("run-1"));

        assert_eq!(failure.diagnostics.len(), 1);
        assert_eq!(failure.diagnostics[0].identity, "rule:stable-identity");
        assert_eq!(
            failure.diagnostics[0].source_location.as_deref(),
            Some("opaque://source/42")
        );
        assert_eq!(
            failure.diagnostics[0].suggested_actions,
            vec!["Apply the producer's remediation."]
        );
        assert_eq!(
            failure.full_evidence_ref.as_deref(),
            Some("homeboy://agent-task/run/run-1/gates#gate=gate-1")
        );
    }

    #[test]
    fn unparsed_output_keeps_a_persisted_full_evidence_ref() {
        let gate = AgentTaskGateReport::new(
            "gate-1",
            vec!["sh".to_string(), "-lc".to_string(), "npm test".to_string()],
            1,
            "unstructured failure",
            "details are retained in the gate report",
            None,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            AgentTaskGateEnvironment::default(),
        );

        let failure = gate_failure(&gate, Some("run-1"));

        assert!(failure.diagnostics.is_empty());
        assert!(failure.full_evidence_ref.as_deref().is_some_and(
            |reference| reference == "homeboy://agent-task/run/run-1/gates#gate=gate-1"
        ));
    }

    #[test]
    fn retry_fixtures_classify_regressing_and_improving_failure_sets() {
        let regressing: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/agent_task_gate_feedback/regressing-retry.json"
        ))
        .expect("regressing fixture");
        let improving: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/agent_task_gate_feedback/improving-retry.json"
        ))
        .expect("improving fixture");

        let failure = |identity: &str| AgentTaskCookLoopGateFailure {
            gate_id: "gate-1".to_string(),
            visibility: AgentTaskGateVisibility::Visible,
            reveal_policy: AgentTaskGateRevealPolicy::FullEvidence,
            command: "opaque-gate".to_string(),
            exit_code: 101,
            classification: AgentTaskGateFailureClassification::CandidateCode,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            summary: String::new(),
            agent_feedback: String::new(),
            diagnostics: vec![AgentTaskCookLoopFailureDiagnostic {
                schema: "example/diagnostic/v7".to_string(),
                identity: identity.to_string(),
                summary: String::new(),
                source_location: None,
                suggested_actions: Vec::new(),
                producer: AgentTaskGateDiagnosticProducer {
                    id: "example-producer".to_string(),
                    schema: "example/producer-output/v7".to_string(),
                },
                full_evidence_ref: "homeboy://agent-task/run/run-1/gates#gate=gate-1".to_string(),
            }],
            full_evidence_ref: None,
        };
        let options = |previous_failure_set: Value| AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate()],
            ),
            attempt: 2,
            max_attempts: 3,
            source_run_id: None,
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: json!({"previous_failure_set": previous_failure_set}),
        };

        let regression = failure_progression(
            &options(regressing["previous_failure_set"].clone()),
            &[failure("policy:existing"), failure("policy:new")],
        );
        assert_eq!(
            regression.status,
            AgentTaskCookLoopFailureProgressionStatus::Regressing
        );
        assert_eq!(
            regression.added,
            regressing["current_failure_set"].as_array().unwrap()[1..]
        );

        let improvement = failure_progression(
            &options(improving["previous_failure_set"].clone()),
            &[failure("policy:remaining")],
        );
        assert_eq!(
            improvement.status,
            AgentTaskCookLoopFailureProgressionStatus::Improving
        );
        assert_eq!(
            improvement.removed,
            improving["previous_failure_set"].as_array().unwrap()[0..1]
        );

        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate()],
            ),
            attempt: 2,
            max_attempts: 3,
            source_run_id: Some("run-regression".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: json!({"previous_failure_set": ["gate-previous"]}),
        });
        let request = report.follow_up_request.expect("regression retries");
        assert_eq!(
            request.inputs["cook_loop"]["retry_policy"],
            "new_failures_first"
        );
        assert_eq!(
            request.metadata["cook_loop"]["retry_policy"],
            "new_failures_first"
        );
    }

    #[test]
    fn producer_diagnostics_are_deduplicated_and_bounded() {
        let record = |identity: &str| AgentTaskGateDiagnosticRecord {
            schema: "example/diagnostic/v7".to_string(),
            identity: identity.to_string(),
            summary: "x".repeat(MAX_DIAGNOSTIC_FIELD_BYTES * 2),
            source_location: None,
            suggested_actions: (0..MAX_DIAGNOSTIC_ACTIONS + 1)
                .map(|_| "action".to_string())
                .collect(),
            producer: AgentTaskGateDiagnosticProducer {
                id: "example-producer".to_string(),
                schema: "example/producer-output/v7".to_string(),
            },
            full_evidence_ref: "homeboy://agent-task/run/run-1/gates#gate=gate-1".to_string(),
        };
        let diagnostics = bounded_diagnostics(&[record("policy:one"), record("policy:one")]);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].identity, "policy:one");
        assert_eq!(diagnostics[0].summary.len(), MAX_DIAGNOSTIC_FIELD_BYTES);
        assert_eq!(
            diagnostics[0].suggested_actions.len(),
            MAX_DIAGNOSTIC_ACTIONS
        );
    }

    #[test]
    fn incomplete_producer_diagnostics_are_not_consumed() {
        let diagnostics = bounded_diagnostics(&[AgentTaskGateDiagnosticRecord {
            schema: "example/diagnostic/v7".to_string(),
            identity: "policy:missing-evidence".to_string(),
            summary: "The producer did not provide an evidence ref.".to_string(),
            source_location: None,
            suggested_actions: Vec::new(),
            producer: AgentTaskGateDiagnosticProducer {
                id: "example-producer".to_string(),
                schema: "example/producer-output/v7".to_string(),
            },
            full_evidence_ref: String::new(),
        }]);

        assert!(diagnostics.is_empty());
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
            require_review_form: false,
            review_form: None,
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
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });

        let request = report.follow_up_request.expect("follow-up request");
        let agent_context = format!("{}\n{}", request.instructions, request.inputs);

        assert!(agent_context.contains("./hidden-heldout-check"));
        assert!(agent_context.contains("secret fixture mismatch"));
    }

    fn valid_review_form() -> AiFilledReviewForm {
        AiFilledReviewForm {
            summary: "Fix the reload crash.".to_string(),
            what_changed: vec!["Guard the null render path.".to_string()],
            compatibility: "Internal only; no compatibility impact.".to_string(),
            verification: Vec::new(),
            used_for: "Reproduced the crash, isolated the null path, added a guard, and verified with a focused gate before finalizing.".to_string(),
        }
    }

    #[test]
    fn green_change_without_review_form_requests_another_attempt() {
        let mut source_request = source_request();
        source_request.artifact_declarations =
            vec![crate::agent_task::AgentTaskArtifactDeclaration {
                name: "transcript".to_string(),
                artifact_type: Some("text".to_string()),
                artifact_schema: None,
                path: None,
                required: true,
                description: None,
                metadata: Value::Null,
            }];
        source_request.output_declarations = vec![crate::agent_task::AgentTaskOutputDeclaration {
            name: "implementation_notes".to_string(),
            required: true,
            schema: "homeboy/test-output/v1".to_string(),
            structural_schema: Value::Null,
            max_bytes: None,
            evidence_relationship: None,
        }];
        source_request.runtime_tools = vec![serde_json::from_value(json!({
            "id": "fixture.mutation-tool",
            "command": ["fixture-tool"],
            "required_capabilities": ["workspace_write"],
            "secret_env": ["FIXTURE_TOOL_TOKEN"]
        }))
        .expect("runtime tool fixture")];
        source_request.component_contracts = vec![AgentTaskComponentContract {
            slug: Some("fixture-component".to_string()),
            path: None,
            extra: Default::default(),
        }];
        source_request.executor.required_capabilities = vec!["workspace_write".to_string()];
        source_request.executor.secret_env = vec!["FIXTURE_PROVIDER_TOKEN".to_string()];
        let provider_config = json!({
            "provider": "fixture-provider",
            "credential_source": "configured-provider-auth",
            "routing": { "endpoint": "fixture://provider" }
        });
        source_request.executor.config = provider_config.clone();
        source_request.executor.runtime_selection = Some(AgentTaskRuntimeSelection::default());
        source_request.policy.tools.tools.insert(
            "write".to_string(),
            AgentToolPolicyRule {
                execution_location: AgentToolExecutionLocation::Runner,
                timeout_ms: None,
                reason: Some("fixture mutation permission".to_string()),
            },
        );
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-form-1".to_string()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: None,
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::RetryRequested);
        let request = report
            .follow_up_request
            .expect("missing form must nudge a follow-up attempt");
        assert!(request.task_id.contains("review-form"));
        assert_eq!(request.inputs["cook_loop"]["review_form_required"], true);
        assert!(request.instructions.contains("review_form"));
        assert!(request.instructions.contains("Preserve the candidate code"));
        assert_eq!(
            request.executor.required_capabilities,
            vec!["structured_outcome"]
        );
        assert_eq!(request.policy.write, "none");
        assert_eq!(request.policy.apply, "none");
        assert!(request.policy.permits_workspace_read_tool());
        assert_eq!(request.policy.tools.tools.len(), 1);
        assert!(!request.policy.tools.tools.contains_key("write"));
        assert!(request.expected_artifacts.is_empty());
        assert!(request.artifact_declarations.is_empty());
        assert!(request.runtime_tools.is_empty());
        assert!(request.component_contracts.is_empty());
        assert!(request.executor.secret_env.is_empty());
        assert_eq!(request.executor.config, provider_config);
        assert!(request.executor.runtime_selection.is_none());
        assert_eq!(
            request.output_declarations,
            vec![review_form_output_declaration()]
        );
        assert_eq!(
            request.limits.timeout_ms,
            Some(DEFAULT_REVIEW_FORM_TIMEOUT_MS)
        );
        assert_eq!(request.metadata["cook_loop"]["kind"], "review_form_only");
    }

    #[test]
    fn review_form_retries_retain_the_normalized_timeout() {
        let mut source_request = source_request();
        source_request.metadata = json!({
            "cook_loop": { "review_form_timeout_ms": MAX_REVIEW_FORM_TIMEOUT_MS + 1 }
        });
        let first = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-form-retry-1".to_string()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: None,
            metadata: Value::Null,
        })
        .follow_up_request
        .expect("first missing form retry");
        assert_eq!(first.limits.timeout_ms, Some(MAX_REVIEW_FORM_TIMEOUT_MS));
        assert_eq!(
            first.metadata["cook_loop"]["review_form_timeout_ms"],
            MAX_REVIEW_FORM_TIMEOUT_MS
        );

        let second = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: first,
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 2,
            max_attempts: 3,
            source_run_id: Some("run-form-retry-2".to_string()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: None,
            metadata: Value::Null,
        })
        .follow_up_request
        .expect("second missing form retry");
        assert_eq!(second.limits.timeout_ms, Some(MAX_REVIEW_FORM_TIMEOUT_MS));
        assert_eq!(
            second.metadata["cook_loop"]["review_form_timeout_ms"],
            MAX_REVIEW_FORM_TIMEOUT_MS
        );
    }

    #[test]
    fn review_form_timeout_is_configurable_but_capped() {
        let mut request = source_request();
        request.metadata = json!({
            "cook_loop": { "review_form_timeout_ms": MAX_REVIEW_FORM_TIMEOUT_MS + 1 }
        });
        assert_eq!(review_form_timeout_ms(&request), MAX_REVIEW_FORM_TIMEOUT_MS);

        request.metadata["cook_loop"]["review_form_timeout_ms"] = json!(5_000);
        assert_eq!(review_form_timeout_ms(&request), 5_000);
    }

    #[test]
    fn green_change_with_valid_review_form_completes() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-form-2".to_string()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: Some(valid_review_form()),
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::GreenCompleted);
        assert!(report.follow_up_request.is_none());
    }

    #[test]
    fn green_change_with_invalid_review_form_requests_another_attempt_with_feedback() {
        let mut form = valid_review_form();
        form.used_for = form.summary.clone(); // not a distinct reflection
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-form-3".to_string()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: Some(form),
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::RetryRequested);
        let request = report.follow_up_request.expect("invalid form nudges retry");
        assert!(request.inputs["cook_loop"]["review_form_feedback"]
            .as_str()
            .unwrap_or_default()
            .contains("distinct from summary"));
    }

    #[test]
    fn deterministic_gate_feedback_precedes_review_form_recovery() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::GateFailed,
                vec![failed_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-form-gate".to_string()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: None,
            metadata: Value::Null,
        });

        let request = report.follow_up_request.expect("failed gate follow-up");
        assert_eq!(
            request.metadata["cook_loop"]["kind"],
            "deterministic-gate-feedback"
        );
        assert!(request.inputs["cook_loop"]["review_form_required"].is_null());
    }

    #[test]
    fn missing_review_form_with_exhausted_budget_does_not_complete() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 3,
            max_attempts: 3,
            source_run_id: Some("run-form-4".to_string()),
            current_diff: String::new(),
            require_review_form: true,
            review_form: None,
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::RetriesExhausted);
        assert!(report.follow_up_request.is_none());
    }

    #[test]
    fn review_form_not_required_completes_green_without_a_form() {
        let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request(),
            promotion_report: promotion_report(
                AgentTaskPromotionStatus::Applied,
                vec![green_gate()],
            ),
            attempt: 1,
            max_attempts: 3,
            source_run_id: Some("run-form-5".to_string()),
            current_diff: String::new(),
            require_review_form: false,
            review_form: None,
            metadata: Value::Null,
        });

        assert_eq!(report.status, AgentTaskCookLoopStatus::GreenCompleted);
        assert!(report.follow_up_request.is_none());
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
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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
                run_id: Some("agent-task-run-1".to_string()),
                path: Some("aggregate.json".to_string()),
            },
            to_worktree: "homeboy@fix-3676".to_string(),
            target: AgentTaskPromotionTarget {
                worktree: "homeboy@fix-3676".to_string(),
                path: Some("/tmp/homeboy@fix-3676".to_string()),
                branch: Some("fix/3676".to_string()),
                head: Some("abc123".to_string()),
                dirty: Some(true),
            },
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
            verified_base: None,
            provenance: json!({ "worktree_path": "/tmp/homeboy@fix-3676" }),
            operator_notification: AgentTaskPromotionNotification {
                status: if status == AgentTaskPromotionStatus::Applied {
                    "completed".to_string()
                } else {
                    "blocked".to_string()
                },
                message: "test promotion notification".to_string(),
                resumable_blocker: None,
                next_command: None,
            },
        }
    }

    fn failed_gate() -> AgentTaskGateReport {
        AgentTaskGateReport::new(
            "gate-1",
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "opaque-gate".to_string(),
            ],
            101,
            "running tests",
            "boom",
            Some(AgentTaskGateFailureEvidence {
                classification: AgentTaskGateFailureClassification::CandidateCode,
                summary: "opaque gate failed".to_string(),
                command: "opaque-gate".to_string(),
                exit_code: 101,
                stdout_tail: "running tests".to_string(),
                stderr_tail: "boom".to_string(),
                agent_feedback: "Update the patch so opaque-gate passes.".to_string(),
                diagnostics: Vec::new(),
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
                "opaque-gate".to_string(),
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
                classification: AgentTaskGateFailureClassification::CandidateCode,
                summary: "secret fixture mismatch on randomized private corpus".to_string(),
                command: "./hidden-heldout-check --fixture secret".to_string(),
                exit_code: 7,
                stdout_tail: "secret fixture mismatch".to_string(),
                stderr_tail: "private evaluator stack trace".to_string(),
                agent_feedback: "Fix the randomized secret fixture mismatch.".to_string(),
                diagnostics: Vec::new(),
            }),
            AgentTaskGateVisibility::Private,
            reveal_policy,
            AgentTaskGateEnvironment::default(),
        )
    }
}

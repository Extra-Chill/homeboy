use clap::Args;
use serde_json::Value;

use homeboy::core::agent_task::{AgentTaskAggregateReport, AgentTaskRequest};
use homeboy::core::agent_task_cook_loop::{evaluate_cook_loop, AgentTaskCookLoopOptions};
use homeboy::core::agent_task_finalization::{
    finalize_pr, AgentTaskGateResult, AgentTaskPrEvidence, AgentTaskPrFinalizationOptions,
};
use homeboy::core::agent_task_lifecycle;
use homeboy::core::agent_task_promotion::{
    promote, AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use homeboy::core::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_task_scheduler::AgentTaskAggregate;
use homeboy::core::config;
use homeboy::core::gate::HomeboyGateResult;

use super::super::CmdResult;
use super::{FinalizePrArgs, GateFeedbackArgs, PromoteArgs, ProvidersArgs, ReviewArgs};

#[derive(Args, Debug)]
pub struct FinalizePrEvidenceArgs {
    /// Attempt summary to include in the PR body.
    #[arg(
        long,
        default_value = "green deterministic gates completed",
        value_name = "TEXT"
    )]
    pub attempt_summary: String,

    /// Source tracker/reference URL or identifier. Repeatable.
    #[arg(long = "source-ref", value_name = "REF")]
    pub source_refs: Vec<String>,

    /// Artifact/evidence URL, path, or identifier. Repeatable.
    #[arg(long = "artifact-ref", value_name = "REF")]
    pub artifact_refs: Vec<String>,

    /// AI tool disclosure line for the PR body.
    #[arg(long, default_value = "OpenCode (GPT-5.5)", value_name = "TEXT")]
    pub ai_tool: String,
}

impl From<FinalizePrEvidenceArgs> for AgentTaskPrEvidence {
    fn from(args: FinalizePrEvidenceArgs) -> Self {
        Self {
            source_refs: args.source_refs,
            artifact_refs: args.artifact_refs,
            attempt_summary: args.attempt_summary,
            ai_tool: args.ai_tool,
        }
    }
}

pub(crate) fn review(args: ReviewArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::status(&args.run_id)?;
    let log = agent_task_lifecycle::logs(&args.run_id)?;
    let artifacts = agent_task_lifecycle::artifacts(&args.run_id)?;
    let aggregate = completed_run_aggregate(&args.run_id).transpose()?;
    let aggregate_review = aggregate
        .as_ref()
        .map(|aggregate| AgentTaskAggregateReport::from(aggregate.outcomes.clone()));
    let promotion_candidates = aggregate_review
        .as_ref()
        .map(|review| {
            let promotion_source = record.aggregate_path.as_deref().unwrap_or(&args.run_id);
            promotion_candidates(
                promotion_source,
                args.to_worktree.as_deref(),
                args.provider_command.as_deref(),
                review,
            )
        })
        .unwrap_or_default();
    let next_actions = review_next_actions(
        &record.state,
        aggregate_review.as_ref(),
        args.to_worktree.as_deref(),
    );

    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-review/v1",
            "run_id": record.run_id,
            "state": record.state,
            "plan_id": record.plan_id,
            "plan_path": record.plan_path,
            "aggregate_path": record.aggregate_path,
            "record": record,
            "logs": log,
            "artifacts": artifacts,
            "aggregate_review": aggregate_review,
            "promotion_candidates": promotion_candidates,
            "next_actions": next_actions,
            "transport": {
                "authoritative": "homeboy-agent-task-lifecycle",
                "chat_state_required": false
            }
        }),
        0,
    ))
}

pub(crate) fn promote_artifact(args: PromoteArgs) -> CmdResult<Value> {
    let (raw, source_path) = read_promotion_source(&args.source)?;
    let report = promote(AgentTaskPromotionOptions {
        source: raw,
        source_path,
        to_worktree: args.to_worktree,
        task_id: args.task_id,
        artifact_id: args.artifact_id,
        dry_run: args.dry_run,
        verify: args.verify,
        private_verify: args.private_verify,
        private_gate_reveal: args.private_gate_reveal,
        provider_command: args.provider_command,
    })?;
    let exit_code = if report.status == AgentTaskPromotionStatus::GateFailed {
        1
    } else {
        0
    };

    Ok((
        serde_json::to_value(report).unwrap_or(Value::Null),
        exit_code,
    ))
}

pub(crate) fn finalize_pull_request(args: FinalizePrArgs) -> CmdResult<Value> {
    let gate_results = parse_gate_results(&args.gate_results)?;
    let normalized_gate_results = gate_results
        .iter()
        .cloned()
        .map(HomeboyGateResult::from)
        .collect();
    let report = finalize_pr(AgentTaskPrFinalizationOptions {
        path: args.path,
        run_id: args.run_id,
        base: args.base,
        head: args.head,
        title: args.title,
        commit_message: args.commit_message,
        gate_results,
        normalized_gate_results,
        changed_files: args.changed_files,
        evidence: args.evidence.into(),
        ai_used_for: args.ai_used_for,
        protected_branches: args.protected_branches,
    })?;
    let exit_code = if report.status == "review_ready" {
        0
    } else {
        1
    };

    Ok((
        serde_json::to_value(report).unwrap_or(Value::Null),
        exit_code,
    ))
}

pub(crate) fn gate_feedback(args: GateFeedbackArgs) -> CmdResult<Value> {
    let promotion_raw = config::read_json_spec_to_string(&args.promotion)?;
    let source_task_raw = config::read_json_spec_to_string(&args.source_task)?;
    let promotion_report: AgentTaskPromotionReport =
        serde_json::from_str(&promotion_raw).map_err(|error| {
            homeboy::core::Error::validation_invalid_json(
                error,
                Some("agent-task promotion report".to_string()),
                Some(promotion_raw.clone()),
            )
        })?;
    let source_request: AgentTaskRequest =
        serde_json::from_str(&source_task_raw).map_err(|error| {
            homeboy::core::Error::validation_invalid_json(
                error,
                Some("agent-task source request".to_string()),
                Some(source_task_raw.clone()),
            )
        })?;
    let current_diff = args
        .current_diff
        .as_deref()
        .map(config::read_json_spec_to_string)
        .transpose()?
        .unwrap_or_default();
    let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
        source_request,
        promotion_report,
        attempt: args.attempt,
        max_attempts: args.max_attempts.max(1),
        source_run_id: args.source_run_id,
        current_diff,
        metadata: Value::Null,
    });

    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

pub(crate) fn providers(args: ProvidersArgs) -> CmdResult<Value> {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-providers/v1",
            "providers": executor.providers(),
            "secret_env": homeboy::core::agent_task_secrets::secret_env_status(&args.secret_env),
        }),
        0,
    ))
}

pub(crate) fn default_protected_branches() -> Vec<String> {
    vec![
        "main".to_string(),
        "master".to_string(),
        "trunk".to_string(),
    ]
}

fn completed_run_aggregate(run_id: &str) -> Option<homeboy::core::Result<AgentTaskAggregate>> {
    match agent_task_lifecycle::aggregate_source(run_id) {
        Ok((raw, _path)) => Some(serde_json::from_str(&raw).map_err(|error| {
            homeboy::core::Error::validation_invalid_json(
                error,
                Some("agent-task aggregate".to_string()),
                Some(raw),
            )
        })),
        Err(error) if error.code == homeboy::core::ErrorCode::ValidationInvalidArgument => None,
        Err(error) => Some(Err(error)),
    }
}

fn promotion_candidates(
    source: &str,
    to_worktree: Option<&str>,
    provider_command: Option<&str>,
    review: &AgentTaskAggregateReport,
) -> Vec<Value> {
    review
        .apply_candidates
        .iter()
        .flat_map(|candidate| {
            candidate.artifact_ids.iter().map(move |artifact_id| {
                let mut command = vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "promote".to_string(),
                    source.to_string(),
                    "--task-id".to_string(),
                    candidate.task_id.clone(),
                    "--artifact-id".to_string(),
                    artifact_id.clone(),
                ];
                if let Some(to_worktree) = to_worktree {
                    command.push("--to-worktree".to_string());
                    command.push(to_worktree.to_string());
                }
                if let Some(provider_command) = provider_command {
                    command.push("--provider-command".to_string());
                    command.push(provider_command.to_string());
                }

                serde_json::json!({
                    "task_id": candidate.task_id,
                    "artifact_id": artifact_id,
                    "reason": candidate.reason,
                    "command": command,
                    "ready": to_worktree.is_some()
                })
            })
        })
        .collect()
}

fn review_next_actions(
    state: &agent_task_lifecycle::AgentTaskRunState,
    aggregate_review: Option<&AgentTaskAggregateReport>,
    to_worktree: Option<&str>,
) -> Vec<String> {
    if matches!(state, agent_task_lifecycle::AgentTaskRunState::Queued) {
        return vec!["run this queued durable task with `homeboy agent-task run <run-id>` or let a daemon claim it with `homeboy agent-task run-next`".to_string()];
    }

    if matches!(state, agent_task_lifecycle::AgentTaskRunState::Running) {
        return vec!["inspect progress with `homeboy agent-task status <run-id>` and `homeboy agent-task logs <run-id>`; stale running records are annotated in status metadata".to_string()];
    }

    let Some(review) = aggregate_review else {
        return vec!["terminal run has no aggregate artifact; inspect lifecycle status for finalization errors".to_string()];
    };

    let mut actions = Vec::new();
    if review.summary.apply_candidates > 0 {
        if to_worktree.is_some() {
            actions.push("review `promotion_candidates` and run the generated `homeboy agent-task promote` command for the selected patch artifact".to_string());
        } else {
            actions.push("rerun review with `--to-worktree <handle>` to generate complete promotion commands for apply candidates".to_string());
        }
    }
    if review.summary.retry_candidates > 0 {
        actions.push(
            "retry provider-error or timeout candidates after fixing executor/preflight issues"
                .to_string(),
        );
    }
    if review.summary.issue_report_candidates > 0 {
        actions.push(
            "open or update the tracker with `issue_report_candidates` diagnostics and evidence"
                .to_string(),
        );
    }
    if review.summary.review_candidates > 0 {
        actions.push(
            "inspect `review_candidates` before deciding whether to retry, report, or ignore"
                .to_string(),
        );
    }
    if actions.is_empty() {
        actions.push("no promotion, retry, or issue-report candidates were produced; inspect task summaries for no-op completion".to_string());
    }
    actions
}

fn parse_gate_results(raw: &[String]) -> homeboy::core::Result<Vec<AgentTaskGateResult>> {
    raw.iter()
        .map(|item| {
            let (name, rest) = item.split_once('=').ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "gate-result",
                    "expected NAME=STATUS or NAME=STATUS:DETAIL",
                    None,
                    Some(vec!["cargo test=passed:targeted suite".to_string()]),
                )
            })?;
            let (status, detail) = rest
                .split_once(':')
                .map(|(status, detail)| (status, Some(detail.to_string())))
                .unwrap_or((rest, None));
            if name.trim().is_empty() || status.trim().is_empty() {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "gate-result",
                    "gate name and status must be non-empty",
                    None,
                    None,
                ));
            }

            Ok(AgentTaskGateResult {
                name: name.trim().to_string(),
                status: status.trim().to_string(),
                detail,
            })
        })
        .collect()
}

pub(crate) fn read_promotion_source(
    spec: &str,
) -> homeboy::core::Result<(String, Option<std::path::PathBuf>)> {
    if spec != "-" {
        let path = std::path::PathBuf::from(spec.strip_prefix('@').unwrap_or(spec));
        if path.is_file() {
            let raw = std::fs::read_to_string(&path).map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "read agent-task promotion source {}",
                        path.display()
                    )),
                )
            })?;
            return Ok((raw, Some(path)));
        }
    }

    if let Ok((raw, path)) = agent_task_lifecycle::aggregate_source(spec) {
        return Ok((raw, Some(path)));
    }

    Ok((
        config::read_json_spec_to_string(spec)?,
        source_spec_path(spec),
    ))
}

fn source_spec_path(spec: &str) -> Option<std::path::PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(std::path::PathBuf::from(
        spec.strip_prefix('@').unwrap_or(spec),
    ))
}

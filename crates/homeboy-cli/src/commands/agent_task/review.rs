use clap::Args;
use serde_json::Value;

use homeboy::core::agent_tasks::cook_loop::{evaluate_cook_loop, AgentTaskCookLoopOptions};
use homeboy::core::agent_tasks::finalization::{
    finalize_pr, AgentTaskGateResult, AgentTaskPrEvidence, AgentTaskPrFinalizationOptions,
    AgentTaskPrRuntimeGuardrails, AgentTaskPrSourceRelationship, AgentTaskPrVerification,
};
use homeboy::core::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::core::agent_tasks::promotion::{
    promote, resume_promoted_patch, AgentTaskPromotionOptions, AgentTaskPromotionReport,
    AgentTaskPromotionStatus,
};
use homeboy::core::agent_tasks::provider::{
    AgentTaskExecutorProvider, AgentTaskProviderCatalog, ExtensionProviderAgentTaskExecutor,
};
use homeboy::core::agent_tasks::review_dossier::{
    resolve_review_profile, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewIssueRelationship, AgentTaskReviewIssueRelationshipKind,
    AgentTaskReviewOverride, AgentTaskReviewOverrideTarget, AgentTaskReviewTestStep,
    AGENT_TASK_REVIEW_DOSSIER_SCHEMA,
};
use homeboy::core::agent_tasks::service as agent_task_service;
use homeboy::core::agent_tasks::{AgentTaskAggregate, AgentTaskAggregateReport, AgentTaskRequest};
use homeboy::core::command_invocation::CommandInvocation;
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
    #[arg(long, default_value = "AI-assisted", value_name = "TEXT")]
    pub ai_tool: String,

    /// Actual model identifier for AI disclosure. Finalization requires a recorded model.
    #[arg(long, value_name = "MODEL")]
    pub ai_model: Option<String>,

    /// Source finding id shared by sibling generated PRs.
    #[arg(long, value_name = "ID")]
    pub related_finding_id: Option<String>,

    /// Source validation packet id shared by sibling generated PRs.
    #[arg(long, value_name = "ID")]
    pub source_packet_id: Option<String>,

    /// Generated change kind, e.g. evidence-only, runtime-fix, or test-only.
    #[arg(long, value_name = "KIND")]
    pub change_kind: Option<String>,

    /// Generated PR or artifact this PR supersedes. Repeatable.
    #[arg(long, value_name = "REF")]
    pub supersedes: Vec<String>,

    /// Generated PR or artifact this PR depends on. Repeatable.
    #[arg(long, value_name = "REF")]
    pub depends_on: Vec<String>,

    /// Targeted verification command that ran before finalization. Repeatable.
    #[arg(long = "targeted-check-run", value_name = "COMMAND")]
    pub targeted_checks_run: Vec<String>,

    /// Exact backend limitation when targeted checks could not be run.
    #[arg(long, value_name = "TEXT")]
    pub targeted_checks_unavailable: Option<String>,

    /// CI check expected to run after push. Repeatable.
    #[arg(long = "ci-expected", value_name = "CHECK")]
    pub ci_expected: Vec<String>,

    /// Manual reviewer verification requested when targeted checks/CI do not cover behavior.
    #[arg(long, value_name = "TEXT")]
    pub manual_reviewer_check: Option<String>,

    /// Runtime-fix evidence bound for generated predicates/semantics.
    #[arg(long, value_name = "TEXT")]
    pub why_not_broader_than_packet: Option<String>,

    /// Evidence-specific discriminator preserved by the runtime fix. Repeatable.
    #[arg(long = "evidence-discriminator", value_name = "TEXT")]
    pub evidence_discriminators: Vec<String>,

    /// Nearby predicate/contract preserved by the runtime fix. Repeatable.
    #[arg(long = "nearby-contract-preserved", value_name = "TEXT")]
    pub nearby_contracts_preserved: Vec<String>,
}

impl From<FinalizePrEvidenceArgs> for AgentTaskPrEvidence {
    fn from(args: FinalizePrEvidenceArgs) -> Self {
        Self {
            source_refs: args.source_refs,
            artifact_refs: args.artifact_refs,
            attempt_summary: args.attempt_summary,
            ai_tool: args.ai_tool,
            ai_model: args.ai_model,
            source_relationship: AgentTaskPrSourceRelationship {
                related_finding_id: args.related_finding_id,
                source_packet_id: args.source_packet_id,
                change_kind: args.change_kind,
                supersedes: args.supersedes,
                depends_on: args.depends_on,
            },
            verification: AgentTaskPrVerification {
                targeted_checks_run: args.targeted_checks_run,
                targeted_checks_unavailable: args.targeted_checks_unavailable,
                ci_expected: args.ci_expected,
                manual_reviewer_check: args.manual_reviewer_check,
            },
            runtime_guardrails: AgentTaskPrRuntimeGuardrails {
                why_not_broader_than_packet: args.why_not_broader_than_packet,
                evidence_discriminators: args.evidence_discriminators,
                nearby_contracts_preserved: args.nearby_contracts_preserved,
            },
            lifecycle: None,
        }
    }
}

pub(crate) fn review(args: ReviewArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::status(&args.run_id)?;
    let log = agent_task_lifecycle::logs(&args.run_id)?;
    let artifacts = agent_task_lifecycle::artifacts(&args.run_id)?;
    let aggregate_source = completed_run_aggregate_source(&args.run_id).transpose()?;
    let aggregate = aggregate_source
        .as_ref()
        .map(|(aggregate, _path)| aggregate);
    let aggregate_review =
        aggregate.map(|aggregate| AgentTaskAggregateReport::from(aggregate.outcomes.clone()));
    let diagnostic_summary = aggregate.and_then(super::diagnostic_summary_from_aggregate);
    let failure_reasons = aggregate
        .map(super::status::failure_reasons_from_aggregate)
        .filter(|reasons| !reasons.is_empty());
    let promotion_candidates = aggregate_review
        .as_ref()
        .map(|review| {
            let promotion_source = aggregate_source
                .as_ref()
                .map(|(_aggregate, path)| path.as_str())
                .or(record.aggregate_path.as_deref())
                .unwrap_or(&args.run_id);
            promotion_candidates(
                promotion_source,
                args.to_worktree.as_deref(),
                args.provider_command.as_deref(),
                &args.provider_argv,
                review,
            )
        })
        .unwrap_or_default();
    let next_actions = review_next_actions(
        &record.run_id,
        &record.state,
        &record.plan_path,
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
            "diagnostic_summary": diagnostic_summary,
            "failure_reasons": failure_reasons,
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
    let to_worktree = args.to_worktree.clone();
    let (raw, source_path) = read_promotion_source(&args.source)?;
    let source_run_id = match agent_task_lifecycle::status(&args.source) {
        Ok(record) => Some(record.run_id),
        Err(_) => match source_path.as_deref() {
            Some(path) => agent_task_lifecycle::run_id_for_aggregate_path(path)?,
            None => None,
        },
    };
    let promotion_options = AgentTaskPromotionOptions {
        source: raw,
        source_run_id: source_run_id.clone(),
        source_path,
        source_worktree_path: None,
        base_ref: None,
        task_base_sha: None,
        to_worktree: args.to_worktree,
        task_id: args.task_id,
        artifact_id: args.artifact_id,
        dry_run: args.dry_run,
        gates: args.gates.into(),
        provider_command: args.provider_command,
        provider_invocation: (!args.provider_argv.is_empty()).then(|| CommandInvocation {
            argv: args.provider_argv,
            ..Default::default()
        }),
    };
    let previous_promotion = source_run_id.as_ref().and_then(|run_id| {
        agent_task_lifecycle::status(run_id)
            .ok()
            .and_then(|record| record.metadata.get("latest_promotion").cloned())
    });
    let report = if let Some(previous) = previous_promotion
        .filter(|previous| previous.get("status").and_then(Value::as_str) == Some("gate_failed"))
    {
        let target_path = previous
            .pointer("/target/path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "promotion",
                    "gate-failed promotion has no materialized target path to resume",
                    None,
                    None,
                )
            })?;
        resume_promoted_patch(
            promotion_options,
            std::path::Path::new(target_path),
            &previous,
        )?
    } else {
        promote(promotion_options)?
    };
    let exit_code = if report.status == AgentTaskPromotionStatus::GateFailed {
        1
    } else {
        0
    };
    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);
    value["handoff"] = promotion_handoff(&report, &to_worktree);
    if let Some(run_id) = source_run_id.filter(|_| !args.dry_run) {
        // Finalization consumes the complete report as its durable gate proof.
        // A status-only projection cannot prove the candidate or its gates.
        let record = agent_task_lifecycle::record_promotion(
            &run_id,
            serde_json::to_value(&report).map_err(|error| {
                homeboy::core::Error::internal_json(
                    error.to_string(),
                    Some("serialize agent-task promotion report".to_string()),
                )
            })?,
        )?;
        value["recorded_on_run"] = serde_json::json!({
            "run_id": record.run_id,
            "metadata_key": "latest_promotion",
            "status_command": format!("homeboy agent-task status {} --full", run_id)
        });
    }

    Ok((value, exit_code))
}

pub(crate) fn finalize_pull_request(args: FinalizePrArgs) -> CmdResult<Value> {
    let gate_results = parse_gate_results(&args.gate_results)?;
    let normalized_gate_results: Vec<HomeboyGateResult> = gate_results
        .iter()
        .cloned()
        .map(HomeboyGateResult::from)
        .collect();
    let evidence: AgentTaskPrEvidence = args.evidence.into();
    let how_to_test = if args.test_steps.is_empty() {
        let legacy_steps: Vec<AgentTaskReviewTestStep> = evidence
            .verification
            .targeted_checks_run
            .iter()
            .cloned()
            .map(|command| AgentTaskReviewTestStep {
                command,
                expected: "passes".to_string(),
            })
            .chain(
                evidence
                    .verification
                    .manual_reviewer_check
                    .iter()
                    .cloned()
                    .map(|command| AgentTaskReviewTestStep {
                        command,
                        expected: "observes the described behavior".to_string(),
                    }),
            )
            .collect();
        legacy_steps
    } else {
        args.test_steps
            .iter()
            .map(|step| parse_test_step(step))
            .collect::<homeboy::core::Result<Vec<_>>>()?
    };
    let mut review_dossier = AgentTaskReviewDossier {
        schema: AGENT_TASK_REVIEW_DOSSIER_SCHEMA.to_string(),
        summary: args.summary.clone().unwrap_or_else(|| args.title.clone()),
        what_changed: if args.what_changed.is_empty() {
            vec![evidence.attempt_summary.clone()]
        } else {
            args.what_changed.clone()
        },
        how_to_test,
        compatibility: args.compatibility.clone().unwrap_or_else(|| {
            "No compatibility impact was recorded by this legacy finalization invocation."
                .to_string()
        }),
        evidence: Vec::new(),
        ai_assistance: AgentTaskReviewAiAssistance {
            used: true,
            tool: evidence.ai_tool.clone(),
            model: evidence
                .ai_model
                .clone()
                .unwrap_or_else(|| "legacy caller did not record a model".to_string()),
            used_for: args.ai_used_for.clone(),
        },
        source_relationships: args
            .closes
            .iter()
            .cloned()
            .map(|reference| AgentTaskReviewIssueRelationship {
                kind: AgentTaskReviewIssueRelationshipKind::Closes,
                reference,
            })
            .chain(args.relates_to.iter().cloned().map(|reference| {
                AgentTaskReviewIssueRelationship {
                    kind: AgentTaskReviewIssueRelationshipKind::RelatesTo,
                    reference,
                }
            }))
            .collect(),
        overrides: args
            .review_overrides
            .iter()
            .map(|raw| parse_override(raw))
            .collect::<homeboy::core::Result<Vec<_>>>()?,
    };
    review_dossier.apply_overrides()?;
    let review_profile = resolve_review_profile(&args.path)?;
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
        evidence,
        ai_used_for: args.ai_used_for,
        review_dossier,
        review_profile,
        manual_finalization: args.manual_finalization,
        protected_branches: args.protected_branches,
    })?;
    let exit_code = if report.status == "review_ready" {
        0
    } else {
        1
    };

    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);
    value["handoff"] = finalization_handoff(&report.status, report.pr_url.as_deref());

    Ok((value, exit_code))
}

fn parse_test_step(raw: &str) -> homeboy::core::Result<AgentTaskReviewTestStep> {
    let (command, expected) = raw.split_once("=>").ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "test-step",
            "expected COMMAND=>EXPECTED",
            None,
            None,
        )
    })?;
    Ok(AgentTaskReviewTestStep {
        command: command.trim().to_string(),
        expected: expected.trim().to_string(),
    })
}

fn parse_override(raw: &str) -> homeboy::core::Result<AgentTaskReviewOverride> {
    let (target, value_and_provenance) = raw.split_once('=').ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "review-override",
            "expected TARGET=VALUE@PROVENANCE",
            None,
            None,
        )
    })?;
    let (value, provenance) = value_and_provenance.rsplit_once('@').ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "review-override",
            "expected TARGET=VALUE@PROVENANCE",
            None,
            None,
        )
    })?;
    let target = match target {
        "summary" => AgentTaskReviewOverrideTarget::Summary,
        "what_changed" => AgentTaskReviewOverrideTarget::WhatChanged,
        "compatibility" => AgentTaskReviewOverrideTarget::Compatibility,
        _ => {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "review-override",
                "target must be summary, what_changed, or compatibility",
                None,
                None,
            ))
        }
    };
    Ok(AgentTaskReviewOverride {
        target,
        value: value.to_string(),
        provenance: provenance.to_string(),
    })
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
    let catalog = if args.refresh {
        AgentTaskProviderCatalog::refresh()
    } else {
        AgentTaskProviderCatalog::discover()
    };
    let catalog_version = catalog.version.clone();
    let executor = ExtensionProviderAgentTaskExecutor::from_catalog(catalog);
    let providers = executor.providers();
    let fallback_sources =
        homeboy::core::agent_tasks::provider::provider_secret_sources_for_providers(providers);
    if args.validate_readiness {
        let backend = args.backend.as_deref().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "backend",
                "agent-task providers --validate-readiness requires --backend",
                None,
                Some(vec![
                    "Pass the same --backend value that the agent-task cook command will use."
                        .to_string(),
                ]),
            )
        })?;
        homeboy::core::agent_tasks::provider::validate_provider_runner_readiness_for_backend(
            backend,
            args.selector.as_deref(),
        )?;
    }
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-providers/v1",
            "catalog": {
                "refreshed": args.refresh,
                "version": catalog_version,
            },
            "dispatch_config_layers": dispatch_config_layers(providers),
            "provider_identity_catalog": provider_identity_catalog(providers),
            "capability_contract": homeboy::core::agent_tasks::provider::provider_capability_contract(),
            "providers": providers,
            "readiness_validation": {
                "validated": args.validate_readiness,
                "backend": args.backend,
                "selector": args.selector,
            },
            "diagnostics": executor.diagnostics(),
            "secret_env": homeboy::core::agent_tasks::secrets::secret_env_status_with_fallbacks(&args.secret_env, &fallback_sources),
        }),
        0,
    ))
}

fn provider_identity_catalog(providers: &[AgentTaskExecutorProvider]) -> Vec<Value> {
    providers
        .iter()
        .map(|provider| {
            let ai_provider_ids = provider.provider_defaults.keys().cloned().collect::<Vec<_>>();
            serde_json::json!({
                "executor_provider_id": provider.id,
                "executor_backend": provider.backend,
                "runtime_id": provider.runtime_id,
                "runtime_package_source": provider.runtime_package_source.as_ref().or(provider.extension_id.as_ref()),
                "runtime_path": provider.runtime_path,
                "ai_provider_ids": ai_provider_ids,
                "model": null,
            })
        })
        .collect()
}

/// Operator-facing explanation of the two distinct dispatch configuration
/// layers, surfaced in `agent-task providers` so a new operator can tell the
/// extension-provider selector apart from the nested AI runtime provider config
/// without reading runtime internals (#6122).
///
/// The confusion this prevents: `--dispatch-selector codex` fails when `codex`
/// is a nested runtime/provider config value, not a Homeboy executor provider id.
fn dispatch_config_layers(providers: &[AgentTaskExecutorProvider]) -> Value {
    let selectable_ids: Vec<String> = providers
        .iter()
        .map(|provider| provider.id.clone())
        .collect();

    // Surface a worked example using a real registered executor provider id
    // when one is available, so the operator can copy a known-good selector
    // instead of guessing.
    let example_selector = providers
        .iter()
        .find(|provider| provider.default_backend)
        .or_else(|| providers.first())
        .cloned();
    let example_selector_id = example_selector
        .as_ref()
        .map(|provider| provider.id.clone())
        .unwrap_or_else(|| "sample.executor-provider".to_string());
    let example_ai_provider = example_selector
        .as_ref()
        .and_then(|provider| provider.provider_defaults.keys().next().cloned())
        .unwrap_or_else(|| "example".to_string());

    serde_json::json!({
        "summary": "Dispatch configuration has two independent layers that are easy to confuse: the extension-provider selector picks which Homeboy executor runs the task, while the nested provider config picks which runtime/model that executor drives. Pass an executor provider id to --dispatch-selector, and pass runtime-specific provider configuration inside --dispatch-provider-config — never the other way around.",
        "layers": [
            {
                "layer": "extension_provider_selector",
                "flags": ["--dispatch-selector", "--dispatch-provider-id", "--selector", "--provider-id"],
                "selects": "Which Homeboy extension executor provider handles the task.",
                "value_is": "A registered executor provider id (see the `providers[].id` values below), NOT a model or AI provider family.",
                "registered_provider_ids": selectable_ids,
            },
            {
                "layer": "agent_model_provider_config",
                "flags": ["--dispatch-provider-config", "--provider-config", "--dispatch-model", "--model"],
                "selects": "Which runtime/provider/model the selected executor uses.",
                "value_is": "Nested provider config JSON (and/or a model override), passed to the executor.",
            }
        ],
        "common_mistake": format!("Passing runtime-specific provider configuration such as `{example_ai_provider}` to --dispatch-selector. That selects the executor, not the model/provider, so it fails with 'no extension agent-task provider ... matched selector'. Put runtime-specific values in --dispatch-provider-config instead."),
        "example": {
            "description": "Run a task with a selected executor provider driving a nested AI runtime/provider config.",
            "command": format!(
                "homeboy agent-task cook --dispatch-selector {example_selector_id} --dispatch-provider-config '{{\"provider\":\"{example_ai_provider}\"}}' --prompt @task.md"
            ),
        }
    })
}

pub(crate) fn default_protected_branches() -> Vec<String> {
    vec![
        "main".to_string(),
        "master".to_string(),
        "trunk".to_string(),
    ]
}

fn completed_run_aggregate_source(
    run_id: &str,
) -> Option<homeboy::core::Result<(AgentTaskAggregate, String)>> {
    match agent_task_lifecycle::aggregate_source(run_id) {
        Ok((raw, path)) => Some(
            serde_json::from_str(&raw)
                .map(|aggregate| (aggregate, path.display().to_string()))
                .map_err(|error| {
                    homeboy::core::Error::validation_invalid_json(
                        error,
                        Some("agent-task aggregate".to_string()),
                        Some(raw),
                    )
                }),
        ),
        Err(error) if error.code == homeboy::core::ErrorCode::ValidationInvalidArgument => None,
        Err(error) => Some(Err(error)),
    }
}

fn promotion_candidates(
    source: &str,
    to_worktree: Option<&str>,
    provider_command: Option<&str>,
    provider_argv: &[String],
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
                command.extend(
                    provider_argv
                        .iter()
                        .map(|argument| format!("--provider-argv={argument}")),
                );

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
    run_id: &str,
    state: &agent_task_lifecycle::AgentTaskRunState,
    plan_path: &str,
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
        actions.push(format!(
            "retry provider-error or timeout candidates after fixing executor/preflight issues with `homeboy agent-task retry {run_id} --run`"
        ));
        actions.push(format!(
            "rerun the persisted plan through Lab with `homeboy --runner <runner-id> agent-task run-plan --plan @{plan_path} --record-run-id <new-run-id>`"
        ));
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

fn promotion_handoff(report: &AgentTaskPromotionReport, to_worktree: &str) -> Value {
    let patch_promoted = report.status.patch_promoted();
    let finalize_path = report
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .unwrap_or(to_worktree);
    let mut next_actions = Vec::new();
    if report.status.gate_failed() {
        next_actions.push(
            "patch promoted but deterministic gates failed; use gate feedback before finalizing"
                .to_string(),
        );
    } else if patch_promoted {
        next_actions.push(
            "patch promoted into the target worktree; verify, then finalize a PR".to_string(),
        );
    } else {
        next_actions
            .push("dry run only; rerun promote without `--dry-run` before finalizing".to_string());
    }

    serde_json::json!({
        "schema": "homeboy/agent-task-promotion-handoff/v1",
        "states": {
            "patch_artifact_produced": true,
            "patch_promoted": patch_promoted,
            "pr_opened": false
        },
        "boundary": report.status.handoff_boundary(),
        "finalize_command": format!(
            "homeboy agent-task finalize-pr --run-id <run-id> --path {finalize_path} --title <title> --commit-message <message>"
        ),
        "next_actions": next_actions
    })
}

fn finalization_handoff(status: &str, pr_url: Option<&str>) -> Value {
    let pr_opened = status == "review_ready" && pr_url.is_some();
    serde_json::json!({
        "schema": "homeboy/agent-task-finalization-handoff/v1",
        "states": {
            "patch_artifact_produced": true,
            "patch_promoted": true,
            "pr_opened": pr_opened
        },
        "boundary": if pr_opened { "pr_opened" } else { "pr_not_opened" },
        "pr_url": pr_url,
        "next_actions": if pr_opened {
            vec!["PR opened or updated; continue review in GitHub".to_string()]
        } else {
            vec!["PR was not opened; inspect finalization status and git/PR errors".to_string()]
        }
    })
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
    agent_task_service::promotion_source(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::agent_tasks::promotion::{
        AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandReport,
        AgentTaskPromotionNotification, AgentTaskPromotionSource, AgentTaskPromotionTarget,
    };
    use homeboy::core::agent_tasks::{
        AgentTaskAggregateSummary, AgentTaskDecisionRef, AgentTaskReconciliationDecision,
    };

    #[test]
    fn promotion_candidates_preserve_provider_argv() {
        let review = AgentTaskAggregateReport {
            schema: "homeboy/agent-task-aggregate-report/v1".to_string(),
            summary: AgentTaskAggregateSummary::default(),
            tasks: Vec::new(),
            artifact_inventory: Vec::new(),
            apply_candidates: vec![AgentTaskDecisionRef {
                task_id: "task-1".to_string(),
                decision: AgentTaskReconciliationDecision::ApplyCandidate,
                reason: "patch available".to_string(),
                artifact_ids: vec!["patch-1".to_string()],
            }],
            issue_report_candidates: Vec::new(),
            retry_plan: Vec::new(),
            review_candidates: Vec::new(),
            matrix: Vec::new(),
        };

        let candidates = promotion_candidates(
            "aggregate.json",
            Some("fixture@target"),
            None,
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "promotion-provider".to_string(),
                "--workspace=/tmp/target".to_string(),
            ],
            &review,
        );

        assert_eq!(
            candidates[0]["command"],
            serde_json::json!([
                "homeboy",
                "agent-task",
                "promote",
                "aggregate.json",
                "--task-id",
                "task-1",
                "--artifact-id",
                "patch-1",
                "--to-worktree",
                "fixture@target",
                "--provider-argv=homeboy",
                "--provider-argv=agent-task",
                "--provider-argv=promotion-provider",
                "--provider-argv=--workspace=/tmp/target",
            ])
        );
    }

    #[test]
    fn typed_test_steps_and_overrides_have_explicit_grammar() {
        let step = parse_test_step("cargo test dossier=>all tests pass").expect("typed step");
        assert_eq!(step.command, "cargo test dossier");
        assert_eq!(step.expected, "all tests pass");
        assert!(parse_test_step("cargo test dossier").is_err());

        let override_ = parse_override("summary=Reviewed summary@operator").expect("override");
        assert!(matches!(
            override_.target,
            AgentTaskReviewOverrideTarget::Summary
        ));
        assert_eq!(override_.provenance, "operator");
        assert!(parse_override("evidence=nope@operator").is_err());
    }

    #[test]
    fn review_next_actions_include_retry_and_lab_run_plan_commands() {
        let review = AgentTaskAggregateReport {
            schema: "homeboy/agent-task-aggregate-report/v1".to_string(),
            summary: AgentTaskAggregateSummary {
                retry_candidates: 1,
                ..AgentTaskAggregateSummary::default()
            },
            tasks: Vec::new(),
            artifact_inventory: Vec::new(),
            apply_candidates: Vec::new(),
            issue_report_candidates: Vec::new(),
            retry_plan: Vec::new(),
            review_candidates: Vec::new(),
            matrix: Vec::new(),
        };

        let actions = review_next_actions(
            "agent-task-run-1",
            &agent_task_lifecycle::AgentTaskRunState::Failed,
            "/tmp/agent-task-run-1/plan.json",
            Some(&review),
            None,
        );

        assert!(actions
            .iter()
            .any(|action| action.contains("homeboy agent-task retry agent-task-run-1 --run")));
        assert!(actions.iter().any(|action| action.contains(
            "homeboy --runner <runner-id> agent-task run-plan --plan @/tmp/agent-task-run-1/plan.json --record-run-id <new-run-id>"
        )));
    }

    #[test]
    fn promotion_handoff_marks_promoted_patch_without_pr_claim() {
        let report = AgentTaskPromotionReport {
            schema: "homeboy/agent-task-promotion-report/v1".to_string(),
            status: AgentTaskPromotionStatus::Applied,
            source: AgentTaskPromotionSource {
                kind: "aggregate".to_string(),
                task_id: "cook-homeboy".to_string(),
                run_id: Some("agent-task-run-1".to_string()),
                path: Some("/tmp/aggregate.json".to_string()),
            },
            to_worktree: "homeboy@fix-runtime".to_string(),
            target: AgentTaskPromotionTarget {
                worktree: "homeboy@fix-runtime".to_string(),
                path: Some("/Users/user/Developer/homeboy@fix-runtime".to_string()),
                branch: Some("fix/runtime".to_string()),
                head: Some("abc123".to_string()),
                dirty: Some(true),
            },
            patch_artifact: AgentTaskPromotionArtifactRef {
                id: "patch-1".to_string(),
                kind: "patch".to_string(),
                path: "/tmp/changes.patch".to_string(),
                sha256: None,
            },
            changed_files: vec!["src/lib.rs".to_string()],
            command_evidence: Vec::<AgentTaskPromotionCommandReport>::new(),
            deterministic_gates: Vec::new(),
            gate_results: Vec::new(),
            provenance: serde_json::json!({ "worktree_path": "/Users/user/Developer/homeboy@fix-runtime" }),
            operator_notification: AgentTaskPromotionNotification {
                status: "completed".to_string(),
                message: "patch promoted".to_string(),
                resumable_blocker: None,
                next_command: None,
            },
        };

        let handoff = promotion_handoff(&report, "homeboy@fix-runtime");

        assert_eq!(handoff["states"]["patch_artifact_produced"], true);
        assert_eq!(handoff["states"]["patch_promoted"], true);
        assert_eq!(handoff["states"]["pr_opened"], false);
        assert_eq!(handoff["boundary"], "patch_promoted_no_pr");
        assert!(handoff["finalize_command"]
            .as_str()
            .expect("finalize command")
            .contains("--path /Users/user/Developer/homeboy@fix-runtime"));
    }

    #[test]
    fn dispatch_config_layers_distinguish_selector_from_provider_config() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "sample.executor-provider",
            "backend": "sample",
            "extension_id": "sample.extension",
            "runtime_id": "sandbox-runtime",
            "provider_defaults": {
                "codex": { "secret_env": ["CODEX_TOKEN"] }
            }
        }))
        .expect("provider fixture");

        let layers = dispatch_config_layers(std::slice::from_ref(&provider));

        // The two layers are named and kept distinct.
        let layer_names: Vec<&str> = layers["layers"]
            .as_array()
            .expect("layers array")
            .iter()
            .map(|layer| layer["layer"].as_str().expect("layer name"))
            .collect();
        assert_eq!(
            layer_names,
            vec!["extension_provider_selector", "agent_model_provider_config"]
        );

        // The selector layer surfaces the real registered provider id, not a model.
        assert_eq!(
            layers["layers"][0]["registered_provider_ids"],
            serde_json::json!(["sample.executor-provider"])
        );

        // The worked example uses the discovered sandbox selector and puts the
        // AI runtime in the nested provider config.
        let command = layers["example"]["command"]
            .as_str()
            .expect("example command");
        assert!(command.contains("--dispatch-selector sample.executor-provider"));
        assert!(command.contains("--dispatch-provider-config"));
        assert!(command.contains("codex"));

        // The common-mistake note calls out the codex-as-selector trap.
        assert!(layers["common_mistake"]
            .as_str()
            .expect("common mistake")
            .contains("codex"));
    }

    #[test]
    fn provider_identity_catalog_uses_explicit_runtime_vocabulary() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "opencode.agent-task-executor",
            "backend": "opencode",
            "extension_id": "sample-runtime",
            "runtime_package_source": "sample-runtime",
            "runtime_id": "opencode-local-runtime",
            "provider_defaults": {
                "openai": {},
                "anthropic": {}
            }
        }))
        .expect("provider fixture");

        let catalog = provider_identity_catalog(&[provider]);

        assert_eq!(
            catalog[0]["executor_provider_id"],
            "opencode.agent-task-executor"
        );
        assert_eq!(catalog[0]["executor_backend"], "opencode");
        assert_eq!(catalog[0]["runtime_id"], "opencode-local-runtime");
        assert_eq!(catalog[0]["runtime_package_source"], "sample-runtime");
        assert_eq!(
            catalog[0]["ai_provider_ids"],
            serde_json::json!(["anthropic", "openai"])
        );
        assert!(catalog[0]["model"].is_null());
    }

    #[test]
    fn dispatch_config_layers_falls_back_to_documented_selector_without_providers() {
        let layers = dispatch_config_layers(&[]);
        let command = layers["example"]["command"]
            .as_str()
            .expect("example command");
        assert!(command.contains("--dispatch-selector sample.executor-provider"));
        assert_eq!(
            layers["layers"][0]["registered_provider_ids"],
            serde_json::json!([])
        );
    }

    #[test]
    fn finalization_handoff_marks_pr_opened_when_review_ready_has_url() {
        let handoff = finalization_handoff(
            "review_ready",
            Some("https://github.com/Extra-Chill/homeboy/pull/9999"),
        );

        assert_eq!(handoff["states"]["patch_artifact_produced"], true);
        assert_eq!(handoff["states"]["patch_promoted"], true);
        assert_eq!(handoff["states"]["pr_opened"], true);
        assert_eq!(handoff["boundary"], "pr_opened");
        assert_eq!(
            handoff["pr_url"],
            "https://github.com/Extra-Chill/homeboy/pull/9999"
        );
    }
}

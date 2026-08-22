use clap::Args;
use serde_json::Value;
use std::sync::Arc;

use homeboy::agents::agent_tasks::cook_loop::{evaluate_cook_loop, AgentTaskCookLoopOptions};
use homeboy::agents::agent_tasks::dispatch_service as agent_task_dispatch_service;
use homeboy::agents::agent_tasks::finalization::{
    finalize_pr, AgentTaskGateResult, AgentTaskPrEvidence, AgentTaskPrFinalizationOptions,
    AgentTaskPrRuntimeGuardrails, AgentTaskPrSourceRelationship, AgentTaskPrVerification,
};
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::agents::agent_tasks::promotion::{
    canonical_recoverable_patch_artifacts, AgentTaskPromotionOptions, AgentTaskPromotionReport,
    AgentTaskPromotionStatus,
};
use homeboy::agents::agent_tasks::provider::{
    provider_credential_readiness, resolve_provider_for_backend, AgentTaskExecutorProvider,
    AgentTaskProviderCatalog, AgentTaskProviderCredentialReadiness,
    ExtensionProviderAgentTaskExecutor, ProviderResolution,
};
use homeboy::agents::agent_tasks::review_dossier::{
    homeboy_tool_disclosure, resolve_review_profile, validate_issue_reference,
    AgentTaskExternalUsageEvidence, AgentTaskExternalUsageStatus, AgentTaskPublicContract,
    AgentTaskPublicContractEvidence, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewIssueRelationship, AgentTaskReviewIssueRelationshipKind,
    AgentTaskReviewOverride, AgentTaskReviewOverrideTarget, AgentTaskReviewTestStep,
    AGENT_TASK_REVIEW_DOSSIER_SCHEMA,
};
use homeboy::agents::agent_tasks::service as agent_task_service;
use homeboy::agents::agent_tasks::{
    AgentTaskAggregate, AgentTaskAggregateReport, AgentTaskRequest,
};
use homeboy::core::command_invocation::CommandInvocation;
use homeboy::core::config;
use homeboy::core::gate::HomeboyGateResult;

use super::super::CmdResult;
use super::candidate::{canonical_candidate_projection, classify_candidates};
use super::{
    AdoptArgs, FinalizePrArgs, GateFeedbackArgs, PromoteArgs, ProvidersArgs,
    RecordReplacementGateProofArgs, ReviewArgs, VerifyReplacementArgs,
};

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

    /// Declared changed public contract as ID=>SUMMARY. Requires the complete compatibility/external-usage evidence bundle below.
    #[arg(long = "changed-public-contract", value_name = "ID=>SUMMARY")]
    pub changed_public_contracts: Vec<String>,

    /// Compatibility impact for declared public contracts.
    #[arg(long, value_name = "TEXT")]
    pub compatibility_impact: Option<String>,

    /// External-consumer impact for declared public contracts.
    #[arg(long, value_name = "TEXT")]
    pub external_consumer_impact: Option<String>,

    /// External usage evidence status: completed or unavailable_manual_review.
    #[arg(long, value_name = "STATUS")]
    pub external_usage_status: Option<String>,

    /// Source used for external usage evidence.
    #[arg(long, value_name = "TEXT")]
    pub external_usage_source: Option<String>,

    /// Limitations of the external usage evidence or manual review.
    #[arg(long, value_name = "TEXT")]
    pub external_usage_limitations: Option<String>,

    /// Reviewer-resolvable HTTPS URL for external usage evidence.
    #[arg(long, value_name = "URL")]
    pub external_usage_url: Option<String>,
}

impl TryFrom<FinalizePrEvidenceArgs> for AgentTaskPrEvidence {
    type Error = homeboy::core::Error;

    fn try_from(args: FinalizePrEvidenceArgs) -> homeboy::core::Result<Self> {
        let changed_public_contracts = args
            .changed_public_contracts
            .iter()
            .map(|raw| parse_public_contract(raw))
            .collect::<homeboy::core::Result<Vec<_>>>()?;
        let public_contract_evidence = public_contract_evidence(&args)?;
        Ok(Self {
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
            changed_public_contracts,
            public_contract_evidence,
            lifecycle: None,
        })
    }
}

pub(crate) fn review(args: ReviewArgs) -> CmdResult<Value> {
    let target = super::status::resolve_cook_reader_target(&args.run_id, false)?;
    let run_id = &target.run_id;
    // Review is an aggregate reader. Its durable controller projection remains
    // useful even when an unrelated runner is unavailable.
    let durable_read = agent_task_lifecycle::durable_local_read(run_id)?;
    let record = durable_read.record;
    // A review that names a target worktree is preparing a promotion handoff.
    // Materialize recovered runner artifacts before rendering its command.
    if args.to_worktree.is_some() {
        agent_task_lifecycle::materialize_recovered_patch_artifact(&record.run_id, None, None)?;
    }
    let log = agent_task_lifecycle::logs(run_id)?;
    let artifacts = agent_task_lifecycle::artifacts(run_id)?;
    let aggregate = durable_read.aggregate.as_ref();
    let aggregate_review =
        aggregate.map(|aggregate| AgentTaskAggregateReport::from(aggregate.outcomes.clone()));
    let diagnostic_summary = aggregate.and_then(super::diagnostic_summary_from_aggregate);
    let failure_reasons = aggregate
        .map(super::status::failure_reasons_from_aggregate)
        .filter(|reasons| !reasons.is_empty());
    let execution_states = aggregate.map(|aggregate| {
        super::status::execution_states_from_aggregate(
            aggregate,
            &serde_json::to_value(&record).unwrap_or(Value::Null),
        )
    });
    let cook_base = agent_task_service::load_recipe_for_attempt(&record.run_id)?
        .map(|recipe| {
            recipe
                .finalization
                .get("base")
                .and_then(Value::as_str)
                .filter(|base| !base.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    homeboy::core::Error::validation_invalid_argument(
                        "cook_recipe.finalization.base",
                        "durable Cook recipe is missing its declared promotion base",
                        Some(recipe.cook_id),
                        None,
                    )
                })
        })
        .transpose()?;
    let promotion_candidates = aggregate_review
        .as_ref()
        .map(|review| {
            aggregate
                .map(|aggregate| {
                    promotion_candidates(
                        PromotionCandidateContext {
                            source: &record.run_id,
                            source_run_id: Some(&record.run_id),
                            aggregate_path: record.aggregate_path.as_deref(),
                            to_worktree: args.to_worktree.as_deref(),
                            cook_base: cook_base.as_deref(),
                            provider_command: args.provider_command.as_deref(),
                            provider_argv: &args.provider_argv,
                            latest_promotion: record.metadata.get("latest_promotion"),
                        },
                        aggregate,
                        review,
                    )
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let next_actions = review_next_actions(
        &record.run_id,
        &record.state,
        &record.plan_path,
        aggregate_review.as_ref(),
        args.to_worktree.as_deref(),
    );

    let mut value = serde_json::json!({
            "schema": "homeboy/agent-task-review/v1",
            "run_id": record.run_id,
            "state": record.state,
            "plan_id": record.plan_id,
            "plan_path": record.plan_path,
            "aggregate_path": record.aggregate_path,
            "record": record,
            "logs": log,
            "artifacts": artifacts,
            "aggregate": aggregate,
            "aggregate_review": aggregate_review,
            "diagnostic_summary": diagnostic_summary,
            "failure_reasons": failure_reasons,
            "execution_states": execution_states,
            "promotion_candidates": promotion_candidates,
            "next_actions": next_actions,
            "transport": {
                "authoritative": "homeboy-agent-task-lifecycle",
                "chat_state_required": false
            },
            "durable_read": {
                "phase": "controller_local",
                "unavailable_sources": durable_read.unavailable_sources,
            }
    });
    value["canonical_candidate"] = canonical_candidate_projection(classify_candidates(&value));
    if let Some(selection) = target.selection {
        let latest_attempt_run_id = selection["latest_attempt_run_id"].as_str();
        if latest_attempt_run_id.is_some_and(|latest| latest != record.run_id) {
            if let Ok(latest) = agent_task_lifecycle::status(latest_attempt_run_id.unwrap()) {
                let review_form = super::status::completed_run_aggregate(&latest.run_id)
                    .transpose()?
                    .and_then(|aggregate| {
                        aggregate
                            .selected_outcome()
                            .or_else(|| {
                                (aggregate.outcomes.len() == 1)
                                    .then(|| aggregate.outcomes.first())
                                    .flatten()
                            })
                            .and_then(|outcome| outcome.outputs.get("review_form"))
                            .cloned()
                    });
                value["contributing_attempt"] = serde_json::json!({
                    "run_id": latest.run_id,
                    "review_form": review_form,
                    "verification": latest.metadata.get("latest_promotion"),
                });
            }
        }
        value["candidate_selection"] = selection;
    }
    Ok((compact_review(value, args.full), 0))
}

/// Default review output is an actionable handoff, not a second copy of every
/// lifecycle and gate record. Full evidence remains available through --full.
fn compact_review(value: Value, full: bool) -> Value {
    if full {
        return value;
    }
    let run_id = value
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let canonical_candidate = canonical_candidate_projection(classify_candidates(&value));
    let promotion = value.pointer("/record/metadata/latest_promotion");
    let selected_candidate = promotion
        .map(compact_selected_candidate)
        .or_else(|| compact_apply_candidate(&value))
        .unwrap_or(Value::Null);
    let gates = promotion
        .and_then(|promotion| promotion.get("deterministic_gates"))
        .and_then(Value::as_array)
        .map(|gates| {
            gates
                .iter()
                .map(|gate| compact_fields(gate, &["name", "status", "exit_code", "command"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    serde_json::json!({
        "schema": value.get("schema"),
        "view": "summary",
        "run_id": value.get("run_id"),
        "state": value.get("state"),
        "plan_id": value.get("plan_id"),
        "plan_path": value.get("plan_path"),
        "aggregate_path": value.get("aggregate_path"),
        "aggregate_review": { "summary": value.pointer("/aggregate_review/summary") },
        "diagnostic_summary": value.get("diagnostic_summary"),
        "failure_reasons": value.get("failure_reasons"),
        "execution_states": value.get("execution_states"),
        "canonical_candidate": canonical_candidate,
        "selected_candidate": selected_candidate,
        "gates": gates,
        "promotion_candidates": value.get("promotion_candidates"),
        "next_actions": value.get("next_actions"),
        "candidate_selection": value.get("candidate_selection"),
        "contributing_attempt": value.get("contributing_attempt"),
        "durable_read": value.get("durable_read"),
        "full_command": format!("homeboy agent-task review {run_id} --full"),
    })
}

fn compact_selected_candidate(promotion: &Value) -> Value {
    let artifact = promotion
        .get("patch_artifact")
        .or_else(|| promotion.get("patch"))
        .map(|artifact| compact_fields(artifact, &["id", "kind", "path", "sha256"]));
    let size_bytes = artifact
        .as_ref()
        .and_then(|artifact| artifact.get("path"))
        .and_then(Value::as_str)
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len());
    serde_json::json!({
        "status": promotion.get("status"),
        "task_id": promotion.pointer("/source/task_id").or_else(|| promotion.get("task_id")),
        "artifact": artifact,
        "size_bytes": size_bytes,
        "changed_files": promotion.get("changed_files"),
    })
}

fn compact_apply_candidate(value: &Value) -> Option<Value> {
    let candidate = value.pointer("/promotion_candidates/0")?;
    let task_id = candidate.get("task_id").and_then(Value::as_str)?;
    let artifact_id = candidate.get("artifact_id").and_then(Value::as_str)?;
    let artifact = value
        .pointer("/aggregate_review/artifact_inventory")?
        .as_array()?
        .iter()
        .find(|artifact| {
            artifact.get("task_id").and_then(Value::as_str) == Some(task_id)
                && artifact.get("artifact_id").and_then(Value::as_str) == Some(artifact_id)
        })?;
    Some(serde_json::json!({
        "status": "available",
        "task_id": candidate.get("task_id"),
        "artifact": compact_fields(artifact, &["artifact_id", "kind", "path", "sha256", "metadata"]),
        "size_bytes": artifact.get("size_bytes"),
        "changed_files": artifact.pointer("/metadata/changed_files"),
    }))
}

fn compact_fields(value: &Value, fields: &[&str]) -> Value {
    let mut object = serde_json::Map::new();
    for field in fields {
        if let Some(value) = value.get(*field) {
            object.insert((*field).to_string(), value.clone());
        }
    }
    Value::Object(object)
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
    let artifact_id = if let Some(run_id) = source_run_id.as_deref() {
        args.artifact_id
            .as_deref()
            .map(|artifact_id| {
                agent_task_lifecycle::resolve_promotion_patch_artifact_id(
                    run_id,
                    args.task_id.as_deref(),
                    artifact_id,
                )
            })
            .transpose()?
    } else {
        args.artifact_id.clone()
    };
    if let Some(run_id) = source_run_id.as_deref() {
        agent_task_lifecycle::materialize_recovered_patch_artifact(
            run_id,
            args.task_id.as_deref(),
            artifact_id.as_deref(),
        )?;
    }
    let promotion_request = agent_task_service::AgentTaskPromotionRequest {
        source: raw,
        source_run_id: source_run_id.clone(),
        source_path,
        source_worktree_path: None,
        base_ref: Some(args.base),
        task_base_sha: None,
        candidate_ref: None,
        to_worktree: args.to_worktree,
        task_id: args.task_id,
        artifact_id,
        dry_run: args.dry_run,
        gates: args.gates.into(),
        provider_command: args.provider_command,
        provider_invocation: (!args.provider_argv.is_empty()).then(|| CommandInvocation {
            argv: args.provider_argv,
            ..Default::default()
        }),
    };
    let report = agent_task_service::execute_promotion(promotion_request)?;
    let exit_code = if report.status == AgentTaskPromotionStatus::GateFailed {
        1
    } else {
        0
    };
    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);
    value["handoff"] = promotion_handoff(&report, &to_worktree);
    if let Some(run_id) = source_run_id.filter(|_| !args.dry_run) {
        let record = agent_task_lifecycle::status(&run_id)?;
        value["recorded_on_run"] = serde_json::json!({
            "run_id": record.run_id,
            "metadata_key": "latest_promotion",
            "status_command": format!("homeboy agent-task status {} --full", run_id)
        });
    }

    Ok((value, exit_code))
}

pub(crate) fn promotion_is_resumable(previous: &Value, rerun_completed_gates: bool) -> bool {
    agent_task_service::promotion_is_resumable(previous, rerun_completed_gates)
}

pub(crate) fn adopt_candidate(args: AdoptArgs) -> CmdResult<Value> {
    let result =
        agent_task_service::adopt_cook_candidate_with_options_dispatcher_and_executor_for_attempt(
            &args.run_or_cook_id,
            args.attempt,
            &args.candidate_ref,
            agent_task_service::AgentTaskCandidateAdoptionOptions {
                ai_model: args.ai_model.clone(),
                replace_interrupted: args.replace_interrupted,
                accept_inherited_failures: args.accept_inherited_failures,
            },
            crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
            Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
        )?;
    let exit_code = result.exit_code;
    let cook_id = result.value.cook_id.clone();
    let selected_attempt = result.value.attempts.first().ok_or_else(|| {
        homeboy::core::Error::internal_unexpected(
            "candidate adoption report is missing its resolved attempt".to_string(),
        )
    })?;
    let selected_attempt_number = selected_attempt.attempt;
    let selected_run_id = selected_attempt.run_id.clone();
    let mut value = super::status::compact_cook_report(
        serde_json::to_value(result.value).unwrap_or(Value::Null),
        args.full,
    );
    value["adoption"] =
        adoption_envelope(&args, &cook_id, selected_attempt_number, &selected_run_id);
    Ok((value, exit_code))
}

fn adoption_envelope(
    args: &AdoptArgs,
    cook_id: &str,
    selected_attempt: u32,
    selected_run_id: &str,
) -> Value {
    serde_json::json!({
        "schema": "homeboy/agent-task-candidate-adoption/v1",
        "source": args.run_or_cook_id,
        "cook_id": cook_id,
        "attempt": selected_attempt,
        "run_id": selected_run_id,
        "candidate_ref": args.candidate_ref,
        "ai_model": args.ai_model,
        "replace_interrupted": args.replace_interrupted,
        "accept_inherited_failures": args.accept_inherited_failures,
        "controller_owned": true,
    })
}

pub(crate) fn finalize_pull_request(args: FinalizePrArgs) -> CmdResult<Value> {
    validate_finalize_inputs(&args)?;
    if let Some(run_or_cook_id) = args.recover.as_deref() {
        let overrides = args
            .review_overrides
            .iter()
            .map(|raw| parse_override(raw))
            .collect::<homeboy::core::Result<Vec<_>>>()?;
        let value = agent_task_service::recover_cook_pr(run_or_cook_id, overrides, args.preflight)?;
        let success = value
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| {
                matches!(status, "review_ready" | "draft_published" | "validated")
            });
        return Ok((value, i32::from(!success)));
    }
    let mut run_id = args
        .run_id
        .expect("clap requires --run-id without --recover");
    if args.manual_finalization {
        run_id = agent_task_service::prepare_manual_finalization_identity(&run_id)?;
    }
    // Retained for the finalization handoff, which names the exact apply command
    // after a validated preflight (#9867). `run_id` itself moves into the options.
    let handoff_run_id = run_id.clone();
    let path = args.path.expect("clap requires --path without --recover");
    let title = args.title.expect("clap requires --title without --recover");
    let commit_message = args
        .commit_message
        .expect("clap requires --commit-message without --recover");
    let gate_results = parse_gate_results(&args.gate_results)?;
    let normalized_gate_results: Vec<HomeboyGateResult> = gate_results
        .iter()
        .cloned()
        .map(HomeboyGateResult::from)
        .collect();
    let evidence: AgentTaskPrEvidence = args.evidence.try_into()?;
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
        summary: args.summary.clone().unwrap_or_else(|| title.clone()),
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
        verified_commands: Vec::new(),
        changed_public_contracts: evidence.changed_public_contracts.clone(),
        public_contract_evidence: evidence.public_contract_evidence.clone(),
        ai_assistance: AgentTaskReviewAiAssistance {
            used: true,
            tool: homeboy_tool_disclosure(&evidence.ai_tool),
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
    let review_profile = resolve_review_profile(&path)?;
    let options = AgentTaskPrFinalizationOptions {
        path,
        run_id,
        base: args.base,
        verified_base_sha: args.verified_base_sha,
        head: args.head,
        title,
        commit_message,
        gate_results,
        normalized_gate_results,
        accept_inherited_failures: false,
        changed_files: args.changed_files,
        evidence,
        ai_used_for: args.ai_used_for,
        review_dossier,
        review_profile,
        manual_finalization: args.manual_finalization,
        expected_candidate_sha: None,
        protected_branches: args.protected_branches,
        draft_pr: false,
    };
    let report = if args.preflight {
        homeboy::agents::agent_tasks::finalization::preflight_pr(options)?
    } else {
        finalize_pr(options)?
    };
    let exit_code = if matches!(
        report.status.as_str(),
        "review_ready" | "draft_published" | "validated"
    ) {
        0
    } else {
        1
    };

    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);
    if should_persist_manual_preflight_intent(
        args.preflight,
        &report.status,
        report.manual_finalization,
    ) {
        agent_task_service::persist_manual_finalization_intent(&handoff_run_id, &report)?;
    }
    value["handoff"] = finalization_handoff(
        &report.status,
        report.pr_url.as_deref(),
        Some(handoff_run_id.as_str()),
    );

    Ok((value, exit_code))
}

pub(crate) fn record_replacement_gate_proof(
    args: RecordReplacementGateProofArgs,
) -> CmdResult<Value> {
    let replacement = serde_json::from_str(&config::read_json_spec_to_string(&args.promotion)?)
        .map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "promotion",
                format!("replacement gate proof is not a valid promotion report: {error}"),
                None,
                None,
            )
        })?;
    let report = agent_task_service::record_replacement_gate_proof(
        &args.run_id,
        replacement,
        args.authorize_external_proof,
    )?;
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

pub(crate) fn verify_replacement(mut args: VerifyReplacementArgs) -> CmdResult<Value> {
    args.gates.snapshot_file_inputs()?;
    let report = agent_task_service::verify_replacement_gates(
        &args.cook_or_attempt_id,
        args.gates.into(),
        args.authorize_external_proof,
    )?;
    let run_id = report.source.run_id.clone().ok_or_else(|| {
        homeboy::core::Error::internal_unexpected(
            "replacement proof report is missing its durable source run id".to_string(),
        )
    })?;
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    value["handoff"] = serde_json::json!({
        "next_command": format!("homeboy agent-task finalize-pr --recover {run_id}"),
        "status_command": format!("homeboy agent-task status {run_id} --full"),
    });
    Ok((value, 0))
}

fn should_persist_manual_preflight_intent(
    preflight: bool,
    status: &str,
    manual_finalization: bool,
) -> bool {
    preflight && status == "validated" && manual_finalization
}

fn validate_finalize_inputs(args: &FinalizePrArgs) -> homeboy::core::Result<()> {
    let mut errors = Vec::new();
    for raw in &args.gate_results {
        if let Err(error) = parse_gate_results(std::slice::from_ref(raw)) {
            errors.push(error);
        }
    }
    for raw in &args.evidence.changed_public_contracts {
        if let Err(error) = parse_public_contract(raw) {
            errors.push(error);
        }
    }
    if let Err(error) = public_contract_evidence(&args.evidence) {
        errors.push(error);
    }
    for raw in &args.test_steps {
        if let Err(error) = parse_test_step(raw) {
            errors.push(error);
        }
    }
    for raw in &args.review_overrides {
        if let Err(error) = parse_override(raw) {
            errors.push(error);
        }
    }
    for reference in args.closes.iter().chain(&args.relates_to) {
        if let Err(error) = validate_issue_reference(reference) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        return Ok(());
    }
    let diagnostics: Vec<Value> = errors
        .iter()
        .map(|error| {
            serde_json::json!({
            "code": format!("{:?}", error.code),
                "message": error.message,
                "details": error.details,
            })
        })
        .collect();
    let mut error = errors.remove(0);
    error.message = format!(
        "finalize-pr input validation failed with {} independent error(s)",
        diagnostics.len()
    );
    error.details = serde_json::json!({ "diagnostics": diagnostics });
    Err(error)
}

fn parse_public_contract(raw: &str) -> homeboy::core::Result<AgentTaskPublicContract> {
    let (id, summary) = raw.split_once("=>").ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "changed-public-contract",
            "expected ID=>SUMMARY",
            None,
            None,
        )
    })?;
    let id = id.trim();
    let summary = summary.trim();
    if id.is_empty() || summary.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "changed-public-contract",
            "contract identifier and summary must be non-empty",
            None,
            None,
        ));
    }
    Ok(AgentTaskPublicContract {
        id: id.to_string(),
        summary: summary.to_string(),
    })
}

fn public_contract_evidence(
    args: &FinalizePrEvidenceArgs,
) -> homeboy::core::Result<Option<AgentTaskPublicContractEvidence>> {
    let supplied = [
        args.compatibility_impact.as_ref(),
        args.external_consumer_impact.as_ref(),
        args.external_usage_status.as_ref(),
        args.external_usage_source.as_ref(),
        args.external_usage_limitations.as_ref(),
        args.external_usage_url.as_ref(),
    ]
    .iter()
    .any(Option::is_some);
    if !supplied {
        return Ok(None);
    }
    let status = match args.external_usage_status.as_deref() {
        Some("completed") => AgentTaskExternalUsageStatus::Completed,
        Some("unavailable_manual_review") => AgentTaskExternalUsageStatus::UnavailableManualReview,
        _ => {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "external-usage-status",
                "must be completed or unavailable_manual_review",
                None,
                None,
            ))
        }
    };
    Ok(Some(AgentTaskPublicContractEvidence {
        compatibility_impact: args.compatibility_impact.clone().unwrap_or_default(),
        external_consumer_impact: args.external_consumer_impact.clone().unwrap_or_default(),
        external_usage: AgentTaskExternalUsageEvidence {
            status,
            source: args.external_usage_source.clone().unwrap_or_default(),
            limitations: args.external_usage_limitations.clone().unwrap_or_default(),
            url: args.external_usage_url.clone().unwrap_or_default(),
        },
    }))
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
    let command = command.trim();
    let expected = expected.trim();
    if command.is_empty() || expected.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "test-step",
            "command and expected result must be non-empty",
            None,
            None,
        ));
    }
    Ok(AgentTaskReviewTestStep {
        command: command.to_string(),
        expected: expected.to_string(),
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
    if value.trim().is_empty() || provenance.trim().is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "review-override",
            "override value and provenance must be non-empty",
            None,
            None,
        ));
    }
    Ok(AgentTaskReviewOverride {
        target,
        value: value.trim().to_string(),
        provenance: provenance.trim().to_string(),
    })
}

const COMMAND_RESULT_ENVELOPE_SCHEMA: &str = "homeboy/command-result/v3";

/// Deserialize a structured input that may be either the bare report/request or
/// a `homeboy/command-result/v3` envelope wrapping it under `data`.
///
/// `agent-task promote --output` writes the command-result envelope, whose outer
/// `status` (e.g. `failed`) is not a promotion status; deserializing it directly
/// as a promotion report failed. Unwrap the envelope's `data` when present so the
/// canonical producer (`promote --output`) composes directly with the consumer
/// (`gate-feedback --promotion`) without a manual `jq '.data'` step (#9893).
fn deserialize_maybe_enveloped<T: serde::de::DeserializeOwned>(
    raw: &str,
    context: &str,
) -> homeboy::core::Result<T> {
    let value: Value = serde_json::from_str(raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some(context.to_string()),
            Some(raw.to_string()),
        )
    })?;

    // A command-result envelope carries the real payload under `data`; a bare
    // report is used as-is.
    let payload =
        if value.get("schema").and_then(Value::as_str) == Some(COMMAND_RESULT_ENVELOPE_SCHEMA) {
            value.get("data").cloned().unwrap_or(value)
        } else {
            value
        };

    serde_json::from_value(payload).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some(context.to_string()),
            Some(raw.to_string()),
        )
    })
}

pub(crate) fn gate_feedback(args: GateFeedbackArgs) -> CmdResult<Value> {
    let promotion_raw = config::read_json_spec_to_string(&args.promotion)?;
    let source_task_raw = config::read_json_spec_to_string(&args.source_task)?;
    let promotion_report: AgentTaskPromotionReport =
        deserialize_maybe_enveloped(&promotion_raw, "agent-task promotion report")?;
    let source_request: AgentTaskRequest =
        deserialize_maybe_enveloped(&source_task_raw, "agent-task source request")?;
    let current_diff = args
        .current_diff
        .as_deref()
        .map(config::read_json_spec_to_string)
        .transpose()?
        .or_else(|| {
            promotion_report
                .provenance
                .pointer("/gate_feedback_baseline/current_diff")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let report = evaluate_cook_loop(AgentTaskCookLoopOptions {
        source_request,
        promotion_report,
        attempt: args.attempt,
        max_attempts: args.max_attempts.max(1),
        source_run_id: args.source_run_id,
        current_diff,
        require_review_form: false,
        review_form: None,
        metadata: Value::Null,
    });

    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

/// The backend the providers presentation is scoped to, if any. `--catalog`
/// (full multi-backend view) and an absent `--backend` both leave the output
/// unscoped (#9654).
fn scoped_provider_backend(backend: Option<&str>, catalog: bool) -> Option<&str> {
    if catalog {
        return None;
    }
    backend
}

/// Filter the catalog to the scoped backend's providers, or return the full set
/// when unscoped. All derived catalog sections are built from the result, so a
/// single-backend query no longer emits every other backend's data (#9654).
fn scope_providers(
    all_providers: &[AgentTaskExecutorProvider],
    scoped_backend: Option<&str>,
) -> Vec<AgentTaskExecutorProvider> {
    match scoped_backend {
        Some(backend) => all_providers
            .iter()
            .filter(|provider| provider.backend == backend)
            .cloned()
            .collect(),
        None => all_providers.to_vec(),
    }
}

const DEFAULT_PROVIDER_LIMIT: usize = 10;
const DEFAULT_DIAGNOSTIC_LIMIT: usize = 10;
const DEFAULT_TEXT_LIMIT: usize = 256;
const DEFAULT_SCOPE_SOURCE_LIMIT: usize = 20;
pub(crate) const AGENT_TASK_PROVIDER_SCOPE_SCHEMA: &str = "homeboy/agent-task-provider-scope/v1";

/// The execution scope a provider catalog describes.
///
/// Controller and runner carry different extensions, runtime defaults,
/// secrets, and provider readiness, so a catalog is only interpretable
/// alongside where it was observed. This is emitted as an additive
/// `observed_scope` object: the pre-existing `scope` object keeps its meaning
/// (which slice of the catalog is being presented) so existing parsers are
/// unaffected (#9763).
fn observed_provider_scope(all_providers: &[AgentTaskExecutorProvider]) -> Value {
    let runner_id = homeboy::core::resource_policy_context::lab_execution_runner_id();
    let identity = homeboy::core::build_identity::current();
    let label = match runner_id.as_deref() {
        Some(runner_id) => format!("runner `{runner_id}`"),
        None => "controller".to_string(),
    };
    let location = if runner_id.is_some() {
        "lab"
    } else {
        "controller"
    };

    let bounded_ids = |values: Vec<String>| {
        let mut values = values;
        values.sort();
        values.dedup();
        let total = values.len();
        values.truncate(DEFAULT_SCOPE_SOURCE_LIMIT);
        (total, values)
    };
    let (extension_total, extension_ids) = bounded_ids(
        all_providers
            .iter()
            .filter_map(|provider| provider.extension_id.clone())
            .collect(),
    );
    let (runtime_total, runtime_ids) = bounded_ids(
        all_providers
            .iter()
            .filter_map(|provider| provider.runtime_id.clone())
            .collect(),
    );

    serde_json::json!({
        "schema": AGENT_TASK_PROVIDER_SCOPE_SCHEMA,
        "location": location,
        "runner_id": runner_id,
        "label": label,
        "homeboy_identity": {
            "version": identity.version,
            "display": identity.display,
            "git_commit": identity.git_commit,
        },
        "extension_source": {
            "total": extension_total,
            "shown": extension_ids.len(),
            "extension_ids": extension_ids,
        },
        "runtime_source": {
            "total": runtime_total,
            "shown": runtime_ids.len(),
            "runtime_ids": runtime_ids,
        },
        "observed_at": chrono::Utc::now().to_rfc3339(),
        "runner_scoped_command": "homeboy agent-task providers --runner <runner-id>",
    })
}

pub(crate) fn providers(args: ProvidersArgs) -> CmdResult<Value> {
    let catalog = if args.refresh {
        AgentTaskProviderCatalog::refresh()
    } else {
        AgentTaskProviderCatalog::discover()
    };
    providers_with_catalog(args, catalog)
}

fn providers_with_catalog(
    args: ProvidersArgs,
    catalog: AgentTaskProviderCatalog,
) -> CmdResult<Value> {
    let catalog_version = catalog.version.clone();
    let all_providers = catalog.providers();
    if args.machine_catalog {
        return Ok((
            serde_json::json!({
                "schema": "homeboy/agent-task-provider-catalog/v1",
                "providers": all_providers,
            }),
            0,
        ));
    }
    let route = resolve_provider_route(&args, &catalog);
    // An absent `--backend` sweeps every declared backend instead of inheriting
    // Cook's default-backend precondition (#12569).
    let declared_backends = (args.validate_readiness && args.backend.is_none())
        .then(|| declared_backend_readiness(&args, &catalog));
    let validated_provider = if args.validate_readiness {
        match validate_effective_provider_route(route.as_ref(), &catalog) {
            Ok(validated) => validated,
            // The sweep already reports this backend's verdict alongside every
            // other one, so an unusable effective route must not fail the
            // command that exists to find a usable backend (#12569). A supplied
            // `--backend` still fails fast: that query names one backend and
            // has no fuller picture to report.
            Err(_) if declared_backends.is_some() => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };

    // Default to the requested backend's slice. Dumping the whole multi-backend
    // catalog (every backend's providers, identity catalog, dispatch layers, and
    // diagnostics) for a single-backend readiness query overflowed the caller
    // display limit and buried the answer (#9654). `--catalog`/`--all` opts back
    // into the full catalog; an absent `--backend` still shows everything.
    let scoped_backend = scoped_provider_backend(args.backend.as_deref(), args.catalog);
    let mut presented_providers = all_providers.to_vec();
    if let Some(ProviderRoute::Resolved {
        provider_id,
        dispatchable,
        ..
    }) = route.as_ref()
    {
        for provider in &mut presented_providers {
            provider.default_backend = provider.id == *provider_id && *dispatchable;
        }
    }
    let scoped_providers = scope_providers(&presented_providers, scoped_backend);
    let filtered_providers = scoped_providers
        .iter()
        .filter(|provider| {
            args.selector
                .as_deref()
                .is_none_or(|selector| provider.id == selector)
                && args
                    .runtime
                    .as_deref()
                    .is_none_or(|runtime| provider.runtime_id.as_deref() == Some(runtime))
                && args
                    .status
                    .as_deref()
                    .is_none_or(|status| provider_status(provider).eq_ignore_ascii_case(status))
        })
        .cloned()
        .collect::<Vec<_>>();
    let providers: &[AgentTaskExecutorProvider] = &filtered_providers;
    let validated_provider_identity = validated_provider.as_ref();
    let validated_provider = validated_provider_identity.and_then(|(_, provider_id)| {
        all_providers
            .iter()
            .find(|provider| &provider.id == provider_id)
    });
    let fallback_sources =
        homeboy::agents::agent_tasks::provider::provider_secret_sources_for_providers(providers);

    let full_command = provider_full_command(&args);
    let shown_providers = if args.full {
        providers.to_vec()
    } else {
        providers
            .iter()
            .take(DEFAULT_PROVIDER_LIMIT)
            .cloned()
            .collect()
    };
    let diagnostics = catalog.diagnostics();
    let shown_diagnostics = if args.full {
        diagnostics
            .iter()
            .cloned()
            .map(|diagnostic| serde_json::to_value(diagnostic).unwrap_or(Value::Null))
            .collect::<Vec<_>>()
    } else {
        diagnostics
            .iter()
            .take(DEFAULT_DIAGNOSTIC_LIMIT)
            .map(compact_diagnostic)
            .collect::<Vec<_>>()
    };

    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-providers/v1",
            "catalog": {
                "refreshed": args.refresh,
                "version": catalog_version,
            },
            // Where this catalog was observed. Provider readiness is
            // scope-sensitive, so an unlabelled catalog is ambiguous (#9763).
            "observed_scope": observed_provider_scope(all_providers),
            "scope": {
                "backend": scoped_backend,
                "filtered": scoped_backend.is_some()
                    || args.selector.is_some()
                    || args.runtime.is_some()
                    || args.status.is_some(),
                "shown": shown_providers.len(),
                "matched": providers.len(),
                "total": all_providers.len(),
                "catalog_command": "homeboy agent-task providers --catalog",
            },
            "operator_summary": {
                "identity": "agent-task providers",
                "state": route.as_ref().map(ProviderRoute::state).unwrap_or(if providers.is_empty() { "empty" } else { "available" }),
                "risk": if diagnostics.is_empty() { Vec::new() } else { vec![format!("{} discovery diagnostic(s)", diagnostics.len())] },
                "next_action": route.as_ref().map(ProviderRoute::next_command).unwrap_or(full_command.clone()),
            },
            "truncation": {
                "providers": { "shown": shown_providers.len(), "omitted": providers.len().saturating_sub(shown_providers.len()), "evidence_ref": "agent-task:provider-catalog", "full_command": full_command.clone() },
                "diagnostics": { "shown": shown_diagnostics.len(), "omitted": diagnostics.len().saturating_sub(shown_diagnostics.len()), "evidence_ref": "agent-task:provider-discovery-diagnostics", "full_command": full_command },
            },
            "dispatch_config_layers": if args.full { dispatch_config_layers(providers) } else { Value::Null },
            "provider_identity_catalog": if args.full { provider_identity_catalog(providers) } else { Vec::new() },
            "capability_contract": homeboy::agents::agent_tasks::provider::provider_capability_contract(),
            "providers": if args.full { serde_json::to_value(shown_providers.clone()).unwrap_or(Value::Null) } else { Value::Array(shown_providers.iter().map(compact_provider).collect()) },
            // Availability means dispatchable. Anything that is declared but
            // not dispatchable reports the credential it is missing here so the
            // remediation survives the `--full` serde presentation too (#11479).
            "credential_readiness": credential_readiness_report(&shown_providers),
            "readiness_validation": readiness_validation_projection(
                validated_provider_identity,
                validated_provider,
                route.as_ref(),
                args.validate_readiness,
                declared_backends,
            ),
            "diagnostics": shown_diagnostics,
            "secret_env": homeboy::agents::agent_tasks::secrets::secret_env_status_with_fallbacks(&args.secret_env, &fallback_sources),
        }),
        0,
    ))
}

/// A provider's catalog status.
///
/// `available` used to mean only "discovery parsed this provider", which made
/// the catalog claim something Homeboy could not honor: a backend with no
/// credential still advertised itself (and its `provider_owned_auth`
/// capability), so a Cook dispatched to it and spent its whole execution budget
/// discovering the gap inside the provider (#11479). Availability now means
/// *dispatchable*: a provider whose declared credentials do not resolve here
/// reports `unavailable`, and the compact presentation's `reason` names the
/// credential.
fn provider_status(provider: &AgentTaskExecutorProvider) -> &'static str {
    provider_status_from_readiness(provider, &provider_credential_readiness(provider))
}

fn provider_status_from_readiness(
    provider: &AgentTaskExecutorProvider,
    readiness: &AgentTaskProviderCredentialReadiness,
) -> &'static str {
    if !readiness.dispatchable {
        return "unavailable";
    }
    if provider.default_backend {
        "default"
    } else {
        "available"
    }
}

/// Per-provider credential readiness for every provider that is not
/// dispatchable. Emitted in both compact and `--full` presentations so an
/// operator reading either one gets the remediation, not just a status word.
fn credential_readiness_report(providers: &[AgentTaskExecutorProvider]) -> Vec<Value> {
    providers
        .iter()
        .map(provider_credential_readiness)
        .filter(|readiness| !readiness.dispatchable)
        .map(|readiness| {
            let remediation = readiness.remediation();
            let mut value = serde_json::to_value(&readiness).unwrap_or(Value::Null);
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "reason".to_string(),
                    Value::String(readiness.reason().unwrap_or_default()),
                );
                object.insert(
                    "remediation".to_string(),
                    Value::Array(remediation.into_iter().map(Value::String).collect()),
                );
            }
            value
        })
        .collect()
}

fn compact_provider(provider: &AgentTaskExecutorProvider) -> Value {
    let readiness = provider_credential_readiness(provider);
    serde_json::json!({
        "id": bounded_text(&provider.id, 160),
        "label": provider.label.as_deref().map(|value| bounded_text(value, 160)),
        "backend": bounded_text(&provider.backend, 160),
        "runtime_id": provider.runtime_id.as_deref().map(|value| bounded_text(value, 160)),
        "extension_id": provider.extension_id.as_deref().map(|value| bounded_text(value, 160)),
        "status": provider_status_from_readiness(provider, &readiness),
        "reason": readiness.reason().map(|value| bounded_text(&value, DEFAULT_TEXT_LIMIT)),
        "default_backend": provider.default_backend,
        "capabilities": provider.capabilities.iter().take(8).map(|value| bounded_text(value, 96)).collect::<Vec<_>>(),
    })
}

#[derive(Debug, Clone)]
enum ProviderRoute {
    Resolved {
        backend: String,
        provider_id: String,
        model: Option<String>,
        dispatchable: bool,
    },
    Blocked {
        reason: String,
        next_command: String,
    },
    Ambiguous {
        backend: String,
        candidate_ids: Vec<String>,
    },
}

impl ProviderRoute {
    fn resolved(&self) -> Option<&Self> {
        matches!(self, Self::Resolved { .. }).then_some(self)
    }

    fn state(&self) -> &'static str {
        match self {
            Self::Resolved {
                dispatchable: true, ..
            } => "ready",
            Self::Resolved {
                dispatchable: false,
                ..
            }
            | Self::Blocked { .. } => "blocked",
            Self::Ambiguous { .. } => "configuration_ambiguous",
        }
    }

    fn next_command(&self) -> String {
        match self {
            Self::Resolved {
                backend,
                provider_id,
                ..
            } => format!(
                "homeboy agent-task providers --backend {} --selector {} --validate-readiness",
                shell_arg(backend),
                shell_arg(provider_id),
            ),
            Self::Blocked { next_command, .. } => next_command.clone(),
            Self::Ambiguous {
                backend,
                candidate_ids,
            } => format!(
                "homeboy agent-task providers --backend {} --selector {} --validate-readiness",
                shell_arg(backend),
                shell_arg(
                    candidate_ids
                        .first()
                        .expect("ambiguous routes have candidates")
                ),
            ),
        }
    }
}

fn resolve_provider_route(
    args: &ProvidersArgs,
    catalog: &AgentTaskProviderCatalog,
) -> Option<ProviderRoute> {
    resolve_provider_route_for(args.backend.clone(), args.selector.clone(), catalog)
}

/// Resolve the route one explicit backend/selector pair produces. The
/// per-backend readiness sweep reuses this so each backend's verdict comes from
/// exactly the resolution `--backend <backend>` would take (#12569).
fn resolve_provider_route_for(
    backend: Option<String>,
    selector: Option<String>,
    catalog: &AgentTaskProviderCatalog,
) -> Option<ProviderRoute> {
    let command = agent_task_dispatch_service::AgentTaskDispatchCommand {
        backend,
        selector,
        ..Default::default()
    };
    let route = match agent_task_dispatch_service::resolve_cook_initial_provider_route_with_catalog(
        command, catalog,
    ) {
        Ok(route) => route,
        Err(error) => {
            return Some(ProviderRoute::Blocked {
                reason: error.message,
                next_command: "homeboy agent-task providers --catalog".to_string(),
            });
        }
    };
    match resolve_provider_for_backend(
        catalog.providers(),
        &route.backend,
        route.selector.as_deref(),
    ) {
        ProviderResolution::Resolved(provider) => Some(ProviderRoute::Resolved {
            backend: route.backend,
            provider_id: provider.id.clone(),
            model: route.model,
            dispatchable: provider_credential_readiness(provider).dispatchable,
        }),
        ProviderResolution::AmbiguousExtensionAlias { mut candidate_ids } => {
            candidate_ids.sort();
            Some(ProviderRoute::Ambiguous {
                backend: route.backend,
                candidate_ids,
            })
        }
        ProviderResolution::NotFound | ProviderResolution::SelectorMismatch { .. } => {
            Some(ProviderRoute::Blocked {
                reason: format!(
                    "Cook resolved backend `{}` but no provider matches its selector",
                    route.backend
                ),
                next_command: format!(
                    "homeboy agent-task providers --backend {} --full",
                    shell_arg(&route.backend)
                ),
            })
        }
    }
}

fn validate_effective_provider_route(
    route: Option<&ProviderRoute>,
    catalog: &AgentTaskProviderCatalog,
) -> homeboy::core::Result<Option<(String, String)>> {
    let Some(route) = route else {
        return Ok(None);
    };
    let ProviderRoute::Resolved {
        backend,
        provider_id,
        ..
    } = route
    else {
        let reason = match route {
            ProviderRoute::Blocked { reason, .. } => reason.clone(),
            ProviderRoute::Ambiguous { backend, .. } => {
                format!("Cook's effective backend `{backend}` requires --selector")
            }
            ProviderRoute::Resolved { .. } => unreachable!(),
        };
        return Err(homeboy::core::Error::validation_invalid_argument(
            "backend",
            reason,
            None,
            Some(vec![route.next_command()]),
        ));
    };
    homeboy::agents::agent_tasks::provider::preflight_provider_credentials_for_backend(
        catalog.providers(),
        backend,
        Some(provider_id),
    )?;
    homeboy::agents::agent_tasks::provider::validate_provider_runner_readiness_for_backend_with_catalog(
        catalog,
        backend,
        Some(provider_id),
    )?;
    Ok(Some((backend.clone(), provider_id.clone())))
}

/// Readiness for every backend the catalog declares, one entry per backend.
///
/// `--validate-readiness` used to inherit Cook's default-backend precondition,
/// so the command Cook's own error tells an operator to run in order to *find* a
/// usable backend failed with that same missing-default error. The only way
/// forward was guessing declared backends one at a time (#12569). An absent
/// `--backend` now validates each declared backend exactly the way
/// `--backend <backend> --validate-readiness` would, and reports every verdict:
/// a backend that fails readiness is captured here, never propagated, because
/// the failing backends are half of the picture this sweep exists to produce.
fn declared_backend_readiness(
    args: &ProvidersArgs,
    catalog: &AgentTaskProviderCatalog,
) -> Vec<Value> {
    catalog
        .backends()
        .into_iter()
        .map(|backend| {
            // A selector is a provider id, so it belongs to exactly one
            // backend. Apply it where it resolves and let every other backend
            // report its own default provider instead of a selector mismatch.
            let selector = args
                .selector
                .as_deref()
                .filter(|selector| {
                    catalog
                        .providers()
                        .iter()
                        .any(|provider| provider.backend == backend && provider.id == *selector)
                })
                .map(str::to_string);
            let route = resolve_provider_route_for(Some(backend.clone()), selector, catalog);
            let (identity, failure) =
                match validate_effective_provider_route(route.as_ref(), catalog) {
                    Ok(identity) => (identity, None),
                    Err(error) => (None, Some(error.message)),
                };
            let provider = identity.as_ref().and_then(|(_, provider_id)| {
                catalog
                    .providers()
                    .iter()
                    .find(|provider| &provider.id == provider_id)
            });
            let mut value = readiness_validation_projection(
                identity.as_ref(),
                provider,
                route.as_ref(),
                true,
                None,
            );
            if let Some(object) = value.as_object_mut() {
                object.insert("backend".to_string(), Value::String(backend));
                if let Some(failure) = failure {
                    object.insert(
                        "reason".to_string(),
                        Value::String(bounded_text(&failure, DEFAULT_TEXT_LIMIT)),
                    );
                }
            }
            value
        })
        .collect()
}

fn live_dispatch_readiness(
    provider: Option<&AgentTaskExecutorProvider>,
    validation_requested: bool,
) -> &'static str {
    if !validation_requested {
        return "not_requested";
    }
    provider
        .filter(|provider| provider.readiness_invocation.is_some())
        .map(|_| "validated")
        .unwrap_or("unverified")
}

fn readiness_validation_projection(
    identity: Option<&(String, String)>,
    provider: Option<&AgentTaskExecutorProvider>,
    route: Option<&ProviderRoute>,
    validation_requested: bool,
    declared_backends: Option<Vec<Value>>,
) -> Value {
    // Present only when the sweep ran (`--validate-readiness` with no
    // `--backend`): `null` means "not swept", an array means "this is every
    // declared backend", so an empty array stays distinguishable (#12569).
    let ready_backends = declared_backends.as_ref().map(|backends| {
        backends
            .iter()
            .filter(|backend| backend["validated"].as_bool().unwrap_or(false))
            .filter_map(|backend| backend["backend"].as_str().map(str::to_string))
            .collect::<Vec<_>>()
    });
    let resolved = route.and_then(ProviderRoute::resolved);
    let (effective_backend, effective_provider_id, effective_model) = match resolved {
        Some(ProviderRoute::Resolved {
            backend,
            provider_id,
            model,
            ..
        }) => (Some(backend), Some(provider_id), model.as_ref()),
        _ => (
            identity.map(|(backend, _)| backend),
            identity.map(|(_, provider_id)| provider_id),
            None,
        ),
    };
    serde_json::json!({
        "validated": validation_requested && identity.is_some(),
        "effective_backend": effective_backend,
        "effective_provider_id": effective_provider_id,
        "effective_model": effective_model,
        // Catalog discovery proves static configuration only. A live dispatch
        // probe is opt-in because providers own its request.
        "static_configuration": "declared",
        "live_dispatch": live_dispatch_readiness(provider, validation_requested),
        "route_state": route.map(ProviderRoute::state),
        "next_command": route.map(ProviderRoute::next_command),
        "reason": match route { Some(ProviderRoute::Blocked { reason, .. }) => Some(reason), _ => None },
        // One entry per declared backend, each carrying this same projection
        // plus the `backend` it describes, so an operator with no configured
        // default can read which `--backend` value is usable here (#12569).
        "backends": declared_backends,
        "ready_backends": ready_backends,
    })
}

fn compact_diagnostic(diagnostic: &impl serde::Serialize) -> Value {
    let mut value = serde_json::to_value(diagnostic).unwrap_or(Value::Null);
    if let Some(Value::String(message)) = value.get_mut("message") {
        *message = bounded_text(message, DEFAULT_TEXT_LIMIT);
    }
    if let Some(object) = value.as_object_mut() {
        for key in ["response_body", "body", "stdout", "stderr"] {
            if let Some(bytes) = object.get(key).and_then(Value::as_str).map(str::len) {
                object.insert(
                    key.to_string(),
                    serde_json::json!({
                        "summarized": true,
                        "omitted_bytes": bytes,
                        "evidence_ref": "agent-task:provider-discovery-diagnostics",
                    }),
                );
            }
        }
    }
    value
}

fn bounded_text(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let bounded = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn provider_full_command(args: &ProvidersArgs) -> String {
    let mut command = "homeboy agent-task providers --full".to_string();
    for (flag, value) in [
        ("backend", args.backend.as_deref()),
        ("selector", args.selector.as_deref()),
        ("runtime", args.runtime.as_deref()),
        ("status", args.status.as_deref()),
    ] {
        if let Some(value) = value {
            command.push_str(&format!(" --{flag} {}", shell_arg(value)));
        }
    }
    command
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
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

#[derive(Clone, Copy)]
struct PromotionCandidateContext<'a> {
    source: &'a str,
    source_run_id: Option<&'a str>,
    aggregate_path: Option<&'a str>,
    to_worktree: Option<&'a str>,
    cook_base: Option<&'a str>,
    provider_command: Option<&'a str>,
    provider_argv: &'a [String],
    latest_promotion: Option<&'a Value>,
}

fn promotion_candidates(
    context: PromotionCandidateContext<'_>,
    aggregate: &AgentTaskAggregate,
    review: &AgentTaskAggregateReport,
) -> Vec<Value> {
    review
        .apply_candidates
        .iter()
        .chain(review.review_candidates.iter().filter(|candidate| {
            aggregate
                .outcomes
                .iter()
                .find(|outcome| outcome.task_id == candidate.task_id)
                .is_some_and(|outcome| {
                    outcome.status
                        == homeboy::agents::agent_tasks::AgentTaskOutcomeStatus::CandidateRecoverable
                })
        }))
        .flat_map(|candidate| {
            let artifact_ids = aggregate
                .outcomes
                .iter()
                .find(|outcome| outcome.task_id == candidate.task_id)
                .filter(|outcome| {
                    outcome.status
                        == homeboy::agents::agent_tasks::AgentTaskOutcomeStatus::CandidateRecoverable
                })
                .map(|outcome| {
                    canonical_recoverable_patch_artifacts(
                        outcome,
                        &AgentTaskPromotionOptions {
                            source: "{}".to_string(),
                            source_run_id: context.source_run_id.map(str::to_string),
                            source_path: context.aggregate_path.map(std::path::PathBuf::from),
                            source_worktree_path: None,
                            base_ref: None,
                            task_base_sha: None,
                            candidate_ref: None,
                            to_worktree: context.to_worktree.unwrap_or("<managed-worktree>").to_string(),
                            task_id: Some(candidate.task_id.clone()),
                            artifact_id: None,
                            dry_run: true,
                            gates: Default::default(),
                            provider_command: None,
                            provider_invocation: None,
                        },
                    )
                    .map(|canonical| canonical.artifacts.into_iter().map(|artifact| artifact.id).collect())
                    .unwrap_or_default()
                })
                .unwrap_or_else(|| candidate.artifact_ids.clone());
            let selection_required = artifact_ids.len() > 1;
            artifact_ids.into_iter().map(move |artifact_id| {
                let command = vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "promote".to_string(),
                    context.source.to_string(),
                    "--task-id".to_string(),
                    candidate.task_id.clone(),
                    "--artifact-id".to_string(),
                    artifact_id.clone(),
                ];
                let continuation = context.latest_promotion.filter(|promotion| {
                    promotion_is_resumable(promotion, false)
                        && promotion.pointer("/source/task_id").and_then(Value::as_str)
                            == Some(candidate.task_id.as_str())
                        && promotion.pointer("/patch_artifact/id").and_then(Value::as_str)
                            == Some(artifact_id.as_str())
                });
                let destination = continuation
                    .and_then(|promotion| promotion.pointer("/target/worktree"))
                    .and_then(Value::as_str)
                    .or(context.to_worktree);
                let command = destination.map(|destination| {
                    let mut command = command;
                    command.push("--to-worktree".to_string());
                    command.push(destination.to_string());
                    if let Some(contract) = continuation
                        .and_then(|promotion| promotion.pointer("/provenance/resume_contract"))
                    {
                        append_resume_contract(&mut command, contract);
                    } else if let Some(base) = context.cook_base {
                        command.extend(["--base".to_string(), base.to_string()]);
                    }
                    if let Some(provider_command) = context.provider_command {
                        command.push("--provider-command".to_string());
                        command.push(provider_command.to_string());
                    }
                    command.extend(
                        context.provider_argv
                            .iter()
                            .map(|argument| format!("--provider-argv={argument}")),
                    );
                    command
                });

                serde_json::json!({
                    "task_id": candidate.task_id,
                    "artifact_id": artifact_id,
                    "reason": candidate.reason,
                    "command": command,
                    "ready": destination.is_some(),
                    "destination_required": destination.is_none(),
                    "selection_required": selection_required,
                })
            })
        })
        .collect()
}

/// Render the durable gate contract rather than relying on evolving CLI defaults.
fn append_resume_contract(command: &mut Vec<String>, contract: &Value) {
    if let Some(base) = contract.pointer("/inputs/base_ref").and_then(Value::as_str) {
        command.extend(["--base".to_string(), base.to_string()]);
    }
    let Some(gates) = contract.get("gates") else {
        return;
    };
    for (key, flag) in [
        ("verify", "--verify"),
        ("private_verify", "--private-verify"),
    ] {
        if let Some(values) = gates.get(key).and_then(Value::as_array) {
            for value in values.iter().filter_map(Value::as_str) {
                command.extend([flag.to_string(), value.to_string()]);
            }
        }
    }
    for (key, flag) in [
        ("private_gate_reveal", "--private-gate-reveal"),
        ("gate_timeout_seconds", "--gate-timeout-seconds"),
        (
            "gate_heartbeat_interval_seconds",
            "--gate-heartbeat-interval-seconds",
        ),
        (
            "gate_no_progress_timeout_seconds",
            "--gate-no-progress-timeout-seconds",
        ),
    ] {
        if let Some(value) = gates.get(key) {
            let value = value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_u64().map(|value| value.to_string()));
            if let Some(value) = value {
                command.extend([flag.to_string(), value.replace('_', "-")]);
            }
        }
    }
    if gates.get("rerun_completed_gates").and_then(Value::as_bool) == Some(true) {
        command.push("--rerun-completed-gates".to_string());
    }
    if let Some(environment) = gates.get("gate_environment") {
        if let Some(mode) = environment.get("mode").and_then(Value::as_str) {
            command.extend([
                "--gate-environment-mode".to_string(),
                mode.replace('_', "-"),
            ]);
        }
        if let Some(variables) = environment.get("variables").and_then(Value::as_object) {
            for (name, value) in variables {
                if let Some(value) = value.as_str() {
                    command.extend(["--gate-env".to_string(), format!("{name}={value}")]);
                }
            }
        }
        for (key, flag) in [
            ("isolate_home", "--isolate-gate-home"),
            ("isolate_xdg", "--isolate-gate-xdg"),
        ] {
            if let Some(value) = environment.get(key).and_then(Value::as_bool) {
                command.push(format!("{flag}={value}"));
            }
        }
        if let Some(inputs) = environment
            .get("extension_inputs")
            .and_then(Value::as_array)
        {
            for input in inputs {
                if let Ok(input) = serde_json::to_string(input) {
                    command.extend(["--gate-extension-input".to_string(), input]);
                }
            }
        }
    }
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
            actions.push(format!(
                "rerun review with `homeboy agent-task review {run_id} --to-worktree <managed-worktree>` to generate executable promotion commands for apply candidates"
            ));
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

fn promotion_handoff(report: &AgentTaskPromotionReport, _to_worktree: &str) -> Value {
    let patch_promoted = report.status.patch_promoted();
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
        "finalize_command": report.source.run_id.as_ref().map(|run_id| format!(
            "homeboy agent-task finalize-pr --recover {run_id}"
        )),
        "next_actions": next_actions
    })
}

/// Render the finalization boundary.
///
/// A successful `--preflight` deliberately suppresses publication, so it must
/// not be described with the same "PR was not opened; inspect … errors" wording
/// as a failed publication attempt. Reporting a validated safety check as an
/// apparent failure sent an agent toward error inspection instead of the apply
/// command it had just earned (#9867).
fn finalization_handoff(status: &str, pr_url: Option<&str>, run_id: Option<&str>) -> Value {
    let pr_opened = status == "review_ready" && pr_url.is_some();
    // `validated` is the terminal success status for a non-mutating preflight;
    // `finalize_pr` never returns it.
    let publication_validated = status == "validated";

    let boundary = if pr_opened {
        "pr_opened"
    } else if publication_validated {
        "publication_validated_not_executed"
    } else {
        "pr_not_opened"
    };

    let finalize_command =
        run_id.map(|run_id| format!("homeboy agent-task finalize-pr --recover {run_id}"));

    let next_actions = if pr_opened {
        vec!["PR opened or updated; continue review in GitHub".to_string()]
    } else if publication_validated {
        let mut actions = vec![
            "Publication validated; no commit, push, or PR mutation occurred by design."
                .to_string(),
        ];
        match finalize_command.as_deref() {
            Some(command) => actions.push(format!("Run `{command}` to execute the validated publication.")),
            None => actions.push(
                "Rerun the same finalize-pr invocation without --preflight to execute the validated publication."
                    .to_string(),
            ),
        }
        actions
    } else {
        vec!["PR was not opened; inspect finalization status and git/PR errors".to_string()]
    };

    serde_json::json!({
        "schema": "homeboy/agent-task-finalization-handoff/v1",
        "states": {
            "patch_artifact_produced": true,
            "patch_promoted": true,
            "pr_opened": pr_opened,
            "publication_mutated": !publication_validated && pr_opened
        },
        "boundary": boundary,
        "pr_url": pr_url,
        "finalize_command": if publication_validated { finalize_command.clone() } else { None },
        "next_actions": next_actions
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
    use clap::Parser;
    use homeboy::agents::agent_tasks::promotion::{
        AgentTaskPromotionArtifactRef, AgentTaskPromotionCommandReport,
        AgentTaskPromotionNotification, AgentTaskPromotionSource, AgentTaskPromotionTarget,
    };
    use homeboy::agents::agent_tasks::{
        AgentTaskAggregateSummary, AgentTaskArtifact, AgentTaskDecisionRef, AgentTaskOutcome,
        AgentTaskOutcomeStatus, AgentTaskReconciliationDecision, AGENT_TASK_ARTIFACT_SCHEMA,
    };
    use sha2::{Digest, Sha256};
    use std::process::Command;

    #[test]
    fn adoption_envelope_reports_canonical_selection_for_a_child_run_source() {
        let args = AdoptArgs {
            run_or_cook_id: "cook-42-attempt-2".to_string(),
            attempt: None,
            candidate_ref: "deadbeef".to_string(),
            ai_model: Some("openai/gpt-5.6-terra".to_string()),
            replace_interrupted: false,
            accept_inherited_failures: false,
            full: false,
        };

        let envelope = adoption_envelope(&args, "cook-42", 2, "cook-42-attempt-2");

        assert_eq!(envelope["source"], "cook-42-attempt-2");
        assert_eq!(envelope["cook_id"], "cook-42");
        assert_eq!(envelope["attempt"], 2);
        assert_eq!(envelope["run_id"], "cook-42-attempt-2");
    }

    #[test]
    fn compact_provider_bounds_extensions_and_large_diagnostics() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "provider-".to_string() + &"x".repeat(10_000),
            "backend": "backend-".to_string() + &"x".repeat(10_000),
            "extension_id": "extension-".to_string() + &"x".repeat(10_000),
            "runtime_id": "runtime-".to_string() + &"x".repeat(10_000),
            "capabilities": vec!["capability-".to_string() + &"x".repeat(10_000); 100],
        }))
        .expect("provider fixture");
        let providers = std::iter::repeat_n(provider, DEFAULT_PROVIDER_LIMIT + 100)
            .map(|provider| compact_provider(&provider))
            .take(DEFAULT_PROVIDER_LIMIT)
            .collect::<Vec<_>>();
        let diagnostic = compact_diagnostic(&serde_json::json!({
            "class": "provider.error",
            "message": "x".repeat(100_000),
            "response_body": "x".repeat(100_000),
        }));

        assert_eq!(providers.len(), DEFAULT_PROVIDER_LIMIT);
        assert!(serde_json::to_vec(&providers).expect("provider JSON").len() < 20_000);
        assert!(diagnostic["message"].as_str().expect("message").len() <= DEFAULT_TEXT_LIMIT + 3);
        assert_eq!(diagnostic["response_body"]["omitted_bytes"], 100_000);
    }

    #[test]
    fn compact_apply_candidate_uses_the_selected_task_and_artifact_id() {
        let value = serde_json::json!({
            "promotion_candidates": [{
                "task_id": "selected-task",
                "artifact_id": "shared-patch"
            }],
            "aggregate_review": {
                "artifact_inventory": [
                    {
                        "task_id": "other-task",
                        "artifact_id": "shared-patch",
                        "kind": "patch",
                        "path": "/tmp/other.patch",
                        "size_bytes": 1,
                        "metadata": { "changed_files": ["other.rs"] }
                    },
                    {
                        "task_id": "selected-task",
                        "artifact_id": "shared-patch",
                        "kind": "patch",
                        "path": "/tmp/selected.patch",
                        "size_bytes": 17394,
                        "metadata": { "changed_files": ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"] }
                    }
                ]
            }
        });

        let selected = compact_apply_candidate(&value).expect("selected candidate");

        assert_eq!(selected["artifact"]["path"], "/tmp/selected.patch");
        assert_eq!(selected["size_bytes"], 17394);
        assert_eq!(selected["changed_files"].as_array().map(Vec::len), Some(6));
    }

    #[test]
    fn compact_review_preserves_one_promoted_candidate_fingerprint() {
        let patch = tempfile::NamedTempFile::new().expect("patch");
        std::fs::write(patch.path(), "x".repeat(7_635)).expect("write patch");
        let value = compact_review(
            serde_json::json!({
                "schema": "homeboy/agent-task-review/v1",
                "run_id": "agent-task-11805",
                "state": "succeeded",
                "record": {
                    "metadata": {
                        "latest_promotion": {
                            "status": "applied",
                            "source": { "task_id": "task-1" },
                            "patch_artifact": {
                                "id": "candidate",
                                "kind": "patch",
                                "path": patch.path(),
                            },
                            "changed_files": ["a.rs", "b.rs", "c.rs"],
                            "deterministic_gates": [{
                                "name": "cargo test",
                                "status": "succeeded",
                                "exit_code": 0,
                                "command": ["cargo", "test"],
                                "stdout": "large duplicate evidence"
                            }]
                        }
                    }
                },
                "logs": { "events": ["large lifecycle payload"] },
                "artifacts": { "artifacts": ["large lifecycle payload"] }
            }),
            false,
        );

        assert_eq!(value["selected_candidate"]["artifact"]["id"], "candidate");
        assert_eq!(value["selected_candidate"]["size_bytes"], 7_635);
        assert_eq!(
            value["selected_candidate"]["changed_files"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(value["gates"][0]["name"], "cargo test");
        assert!(value.get("record").is_none());
        assert!(value.get("logs").is_none());
        assert!(value.get("artifacts").is_none());
    }

    #[test]
    fn provider_full_command_quotes_filter_values() {
        let command = provider_full_command(&ProvidersArgs {
            backend: Some("backend; touch /tmp/unwanted".to_string()),
            selector: Some("provider id".to_string()),
            runtime: None,
            status: None,
            secret_env: Vec::new(),
            validate_readiness: false,
            refresh: false,
            catalog: false,
            full: false,
            machine_catalog: false,
        });

        assert_eq!(
            command,
            "homeboy agent-task providers --full --backend 'backend; touch /tmp/unwanted' --selector 'provider id'"
        );
    }

    /// The claude-code shape from #11479: it advertises `provider_owned_auth`
    /// and declares its own required credential.
    fn credential_declaring_provider() -> AgentTaskExecutorProvider {
        serde_json::from_value(serde_json::json!({
            "id": "claude-code.agent-task-executor",
            "backend": "claude-code",
            "capabilities": ["cli_runtime", "provider_owned_auth"],
            "provider_defaults": {
                "claude-code": {
                    "secret_env": ["AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN"],
                    "required_secret_env": ["AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN"]
                }
            }
        }))
        .expect("provider fixture")
    }

    fn providers_args() -> ProvidersArgs {
        ProvidersArgs {
            backend: None,
            selector: None,
            runtime: None,
            status: None,
            secret_env: Vec::new(),
            validate_readiness: false,
            refresh: false,
            catalog: false,
            full: false,
            machine_catalog: false,
        }
    }

    fn provider_catalog(providers: Vec<AgentTaskExecutorProvider>) -> AgentTaskProviderCatalog {
        AgentTaskProviderCatalog {
            providers,
            ..Default::default()
        }
    }

    fn provider(id: &str, backend: &str) -> AgentTaskExecutorProvider {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "backend": backend,
        }))
        .expect("provider fixture")
    }

    fn save_provider_policy(
        default_backend: Option<&str>,
        rotation: Option<homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationPolicy>,
    ) {
        let mut config = homeboy::core::defaults::load_config();
        config.agent_task.default_backend = default_backend.map(str::to_string);
        config.agent_task.rotation =
            rotation.map(|rotation| serde_json::to_value(rotation).expect("serialize rotation"));
        homeboy::core::defaults::save_config(&config).expect("save provider policy");
    }

    #[test]
    fn providers_end_to_end_projects_configured_default_and_status_filters() {
        crate::test_support::with_isolated_home(|_| {
            save_provider_policy(Some("configured"), None);
            let catalog = provider_catalog(vec![
                provider("configured.provider", "configured"),
                provider("other.provider", "other"),
            ]);
            let output = providers_with_catalog(providers_args(), catalog.clone())
                .expect("output")
                .0;
            assert_eq!(output["operator_summary"]["state"], "ready");
            assert_eq!(
                output["readiness_validation"]["effective_provider_id"],
                "configured.provider"
            );
            assert_eq!(output["providers"][0]["default_backend"], true);

            let mut default_filter = providers_args();
            default_filter.status = Some("default".to_string());
            let output = providers_with_catalog(default_filter, catalog.clone())
                .expect("default filter")
                .0;
            assert_eq!(output["scope"]["matched"], 1);
            assert_eq!(output["providers"][0]["id"], "configured.provider");

            let mut available_filter = providers_args();
            available_filter.status = Some("available".to_string());
            let output = providers_with_catalog(available_filter, catalog)
                .expect("available filter")
                .0;
            assert_eq!(output["scope"]["matched"], 1);
            assert_eq!(output["providers"][0]["id"], "other.provider");
        });
    }

    #[test]
    fn providers_end_to_end_uses_rotation_for_unavailable_default_and_live_validation() {
        crate::test_support::with_isolated_home(|_| {
            save_provider_policy(
                Some("unavailable"),
                Some(
                    homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationPolicy {
                        entries: vec![
                            homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationEntry {
                                backend: Some("fallback".to_string()),
                                selector: Some("fallback.provider".to_string()),
                                model: Some("fallback-model".to_string()),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                ),
            );
            let mut unavailable = credential_declaring_provider();
            unavailable.id = "unavailable.provider".to_string();
            unavailable.backend = "unavailable".to_string();
            let mut fallback = provider("fallback.provider", "fallback");
            fallback.readiness_invocation = Some(
                serde_json::from_value(serde_json::json!({
                    "argv": ["sh", "-c", "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/agent-task-provider-readiness-result/v1\",\"ready\":true,\"classification\":\"ready\",\"retryable\":false,\"remediation\":\"\",\"reason\":\"\",\"cache_key\":\"test\",\"identity\":{}}'"]
                }))
                .expect("readiness invocation"),
            );
            let catalog = provider_catalog(vec![unavailable, fallback]);
            let mut args = providers_args();
            args.validate_readiness = true;
            let output = providers_with_catalog(args, catalog)
                .expect("validated fallback")
                .0;
            assert_eq!(output["operator_summary"]["state"], "ready");
            assert_eq!(
                output["readiness_validation"]["effective_backend"],
                "fallback"
            );
            assert_eq!(
                output["readiness_validation"]["effective_provider_id"],
                "fallback.provider"
            );
            assert_eq!(
                output["readiness_validation"]["effective_model"],
                "fallback-model"
            );
            assert_eq!(output["readiness_validation"]["live_dispatch"], "validated");
        });
    }

    /// `--validate-readiness` with no `--backend` and no configured default is
    /// exactly the discovery question Cook's missing-default error sends the
    /// operator here to answer, so it must sweep instead of inheriting Cook's
    /// precondition — and a backend that fails readiness must be reported, not
    /// propagated (#12569).
    #[test]
    fn providers_validate_readiness_without_backend_sweeps_every_declared_backend() {
        crate::test_support::with_isolated_home(|_| {
            save_provider_policy(None, None);
            let mut ready = provider("ready.provider", "ready");
            ready.readiness_invocation = Some(
                serde_json::from_value(serde_json::json!({
                    "argv": ["sh", "-c", "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/agent-task-provider-readiness-result/v1\",\"ready\":true,\"classification\":\"ready\",\"retryable\":false,\"remediation\":\"\",\"reason\":\"\",\"cache_key\":\"test\",\"identity\":{}}'"]
                }))
                .expect("ready readiness invocation"),
            );
            let mut failing = provider("failing.provider", "failing");
            failing.readiness_invocation = Some(
                serde_json::from_value(serde_json::json!({
                    "argv": ["sh", "-c", "cat >/dev/null; printf '%s' '{\"schema\":\"homeboy/agent-task-provider-readiness-result/v1\",\"ready\":false,\"classification\":\"configuration\",\"retryable\":false,\"remediation\":\"install the executable\",\"reason\":\"executable_not_found\",\"cache_key\":\"test\",\"identity\":{}}'"]
                }))
                .expect("failing readiness invocation"),
            );
            let mut args = providers_args();
            args.validate_readiness = true;

            let (output, status) =
                providers_with_catalog(args, provider_catalog(vec![ready, failing]))
                    .expect("an unusable backend is reported, not propagated");

            assert_eq!(status, 0);
            let validation = &output["readiness_validation"];
            let backends = validation["backends"]
                .as_array()
                .expect("every declared backend is reported");
            assert_eq!(backends.len(), 2);
            let failing = backends
                .iter()
                .find(|backend| backend["backend"] == "failing")
                .expect("failing backend readiness");
            assert_eq!(failing["validated"], false);
            assert!(
                failing["reason"]
                    .as_str()
                    .expect("failure reason")
                    .contains("executable_not_found"),
                "the readiness failure detail is captured per backend"
            );
            let ready = backends
                .iter()
                .find(|backend| backend["backend"] == "ready")
                .expect("ready backend readiness");
            assert_eq!(ready["validated"], true);
            assert_eq!(ready["effective_provider_id"], "ready.provider");
            assert_eq!(ready["live_dispatch"], "validated");
            assert_eq!(
                validation["ready_backends"],
                serde_json::json!(["ready"]),
                "the usable --backend values are named directly"
            );
            // The effective route stays blocked because no default backend is
            // configured; the sweep is what makes that recoverable.
            assert_eq!(output["operator_summary"]["state"], "blocked");
            assert_eq!(validation["validated"], false);
        });
    }

    #[test]
    fn providers_end_to_end_reports_ambiguous_and_missing_default_routes() {
        crate::test_support::with_isolated_home(|_| {
            save_provider_policy(Some("extension"), None);
            let mut first = provider("first.provider", "first");
            first.extension_id = Some("extension".to_string());
            let mut second = provider("second.provider", "second");
            second.extension_id = Some("extension".to_string());
            let output =
                providers_with_catalog(providers_args(), provider_catalog(vec![first, second]))
                    .expect("ambiguous output")
                    .0;
            assert_eq!(
                output["operator_summary"]["state"],
                "configuration_ambiguous"
            );

            save_provider_policy(None, None);
            let output = providers_with_catalog(providers_args(), provider_catalog(Vec::new()))
                .expect("missing-default output")
                .0;
            assert_eq!(output["operator_summary"]["state"], "blocked");
        });
    }

    #[test]
    fn cook_credential_preflight_uses_the_same_rotated_provider_route() {
        crate::test_support::with_isolated_home(|_| {
            save_provider_policy(
                Some("unavailable"),
                Some(
                    homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationPolicy {
                        entries: vec![
                            homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationEntry {
                                backend: Some("fallback".to_string()),
                                selector: Some("fallback.provider".to_string()),
                                model: Some("fallback-model".to_string()),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                ),
            );
            let mut unavailable = credential_declaring_provider();
            unavailable.backend = "unavailable".to_string();
            let credential = format!("HOMEBOY_ROTATED_FALLBACK_{}", uuid::Uuid::new_v4());
            let mut fallback = provider("fallback.provider", "fallback");
            fallback.provider_defaults.insert(
                "fallback".to_string(),
                serde_json::json!({ "required_secret_env": [credential.clone()] }),
            );
            let catalog = provider_catalog(vec![unavailable, fallback]);
            let route =
                agent_task_dispatch_service::resolve_cook_initial_provider_route_with_catalog(
                    agent_task_dispatch_service::AgentTaskDispatchCommand::default(),
                    &catalog,
                )
                .expect("resolve rotated Cook route");
            assert_eq!(route.backend, "fallback");
            assert_eq!(route.selector.as_deref(), Some("fallback.provider"));
            assert_eq!(route.model.as_deref(), Some("fallback-model"));
            let error = super::super::run::preflight_cook_provider_credentials_with_catalog(
                agent_task_dispatch_service::AgentTaskDispatchCommand::default(),
                &catalog,
            )
            .expect_err("injected fallback credential must be preflighted without rediscovery");
            assert!(error.message.contains(&credential), "{error}");
        });
    }

    #[test]
    fn a_provider_missing_its_declared_credential_is_not_reported_available() {
        crate::test_support::with_isolated_home(|_| {
            let provider = credential_declaring_provider();

            assert_eq!(
                provider_status(&provider),
                "unavailable",
                "availability must mean dispatchable, not merely declared (#11479)"
            );

            let compact = compact_provider(&provider);
            assert_eq!(compact["status"], "unavailable");
            assert_eq!(
                compact["reason"], "missing credential AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN",
                "the reason must name the credential an operator has to set"
            );
        });
    }

    #[test]
    fn a_provider_with_no_declared_credential_stays_available() {
        crate::test_support::with_isolated_home(|_| {
            let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
                "id": "local-shell.agent-task-executor",
                "backend": "local-shell",
            }))
            .expect("provider fixture");

            assert_eq!(provider_status(&provider), "available");
            assert!(compact_provider(&provider)["reason"].is_null());
        });
    }

    #[test]
    fn live_dispatch_readiness_uses_the_resolved_provider_only() {
        let mut probed: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "probed", "backend": "test", "readiness_invocation": { "argv": ["true"] }
        }))
        .expect("probed provider");
        let unprobed: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "unprobed", "backend": "test"
        }))
        .expect("unprobed provider");

        assert_eq!(live_dispatch_readiness(Some(&probed), true), "validated");
        assert_eq!(live_dispatch_readiness(Some(&unprobed), true), "unverified");
        assert_eq!(live_dispatch_readiness(None, true), "unverified");
        assert_eq!(
            live_dispatch_readiness(Some(&probed), false),
            "not_requested"
        );
        probed.readiness_invocation = None;
        assert_eq!(live_dispatch_readiness(Some(&probed), true), "unverified");
    }

    #[test]
    fn readiness_validation_reports_effective_identity_not_raw_arguments() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "resolved.provider", "backend": "resolved-backend",
            "readiness_invocation": { "argv": ["true"] }
        }))
        .expect("provider");
        let identity = (
            "resolved-backend".to_string(),
            "resolved.provider".to_string(),
        );

        let projection =
            readiness_validation_projection(Some(&identity), Some(&provider), None, true, None);

        assert_eq!(projection["effective_backend"], "resolved-backend");
        assert_eq!(projection["effective_provider_id"], "resolved.provider");
        assert_eq!(projection["live_dispatch"], "validated");
        // A single-backend query is not a sweep: it reports no per-backend
        // readiness rather than an empty one (#12569).
        assert!(projection["backends"].is_null());
        assert!(projection["ready_backends"].is_null());
        assert!(projection.get("backend").is_none());
        assert!(projection.get("selector").is_none());
    }

    #[test]
    fn credential_readiness_report_carries_the_remediation() {
        crate::test_support::with_isolated_home(|_| {
            let report = credential_readiness_report(&[credential_declaring_provider()]);

            assert_eq!(report.len(), 1, "one undispatchable provider is reported");
            assert_eq!(report[0]["provider_id"], "claude-code.agent-task-executor");
            assert_eq!(report[0]["dispatchable"], false);
            assert_eq!(
                report[0]["missing"][0],
                "AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN"
            );
            assert!(
                report[0]["remediation"]
                    .as_array()
                    .expect("remediation lines")
                    .iter()
                    .any(|line| {
                        line.as_str().is_some_and(|line| {
                            line.contains("AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN")
                        })
                    }),
                "an undispatchable provider must report how to fix it"
            );
        });
    }

    #[test]
    fn dispatchable_providers_are_absent_from_the_credential_readiness_report() {
        crate::test_support::with_isolated_home(|_| {
            let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
                "id": "local-shell.agent-task-executor",
                "backend": "local-shell",
            }))
            .expect("provider fixture");

            assert!(credential_readiness_report(&[provider]).is_empty());
        });
    }

    fn recoverable_review_aggregate(
        temp: &tempfile::TempDir,
        producer_attempts: &[u64],
    ) -> AgentTaskAggregate {
        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let sha256 = format!("{:x}", Sha256::digest(patch.as_bytes()));
        let artifacts = producer_attempts
            .iter()
            .enumerate()
            .map(|(index, attempt)| {
                let path = temp.path().join(format!("candidate-{index}.patch"));
                std::fs::write(&path, patch).expect("write patch");
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: format!("candidate-{index}"),
                    kind: if index == 0 { "patch" } else { "git-diff" }.to_string(),
                    path: Some(path.display().to_string()),
                    size_bytes: Some(patch.len() as u64),
                    sha256: Some(sha256.clone()),
                    metadata: serde_json::json!({
                        "role": "patch",
                        "run_id": "review-run",
                        "task_id": "task-1",
                        "producer_attempt": attempt,
                        "base_ref": "base",
                        "provider_backend": "provider",
                        "repository_identity": "repo",
                        "workspace_identity": "workspace",
                    }),
                    ..Default::default()
                }
            })
            .collect();
        let mut aggregate: AgentTaskAggregate = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": "test",
            "status": "candidate_recoverable",
            "totals": { "skipped": 0 },
        }))
        .expect("aggregate");
        aggregate.outcomes = vec![AgentTaskOutcome {
            task_id: "task-1".to_string(),
            status: AgentTaskOutcomeStatus::CandidateRecoverable,
            artifacts,
            ..Default::default()
        }];
        aggregate
    }

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
        let aggregate: AgentTaskAggregate = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": "test",
            "status": "succeeded",
            "totals": { "skipped": 0 },
        }))
        .expect("aggregate");

        let provider_argv = [
            "homeboy".to_string(),
            "agent-task".to_string(),
            "promotion-provider".to_string(),
            "--workspace=/tmp/target".to_string(),
        ];
        let candidates = promotion_candidates(
            PromotionCandidateContext {
                source: "aggregate.json",
                source_run_id: None,
                aggregate_path: None,
                to_worktree: Some("fixture@target"),
                cook_base: None,
                provider_command: None,
                provider_argv: &provider_argv,
                latest_promotion: None,
            },
            &aggregate,
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
    fn promotion_candidates_preserve_the_declared_cook_base() {
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
        let aggregate: AgentTaskAggregate = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": "test",
            "status": "succeeded",
            "totals": { "skipped": 0 },
        }))
        .expect("aggregate");

        let candidates = promotion_candidates(
            PromotionCandidateContext {
                source: "cook-attempt-9400",
                source_run_id: Some("cook-attempt-9400"),
                aggregate_path: None,
                to_worktree: Some("fixture@target"),
                cook_base: Some("trunk"),
                provider_command: None,
                provider_argv: &[],
                latest_promotion: None,
            },
            &aggregate,
            &review,
        );

        assert_eq!(
            candidates[0]["command"],
            serde_json::json!([
                "homeboy",
                "agent-task",
                "promote",
                "cook-attempt-9400",
                "--task-id",
                "task-1",
                "--artifact-id",
                "patch-1",
                "--to-worktree",
                "fixture@target",
                "--base",
                "trunk",
            ])
        );
    }

    #[test]
    fn resume_contract_emits_exact_base_and_gate_arguments() {
        let mut command = vec!["homeboy".to_string(), "agent-task".to_string()];
        append_resume_contract(
            &mut command,
            &serde_json::json!({
                "inputs": { "base_ref": "release" },
                "gates": {
                    "verify": ["cargo test --lib"],
                    "private_verify": ["./private-check"],
                    "private_gate_reveal": "full_evidence",
                    "gate_timeout_seconds": 42,
                    "gate_heartbeat_interval_seconds": 7,
                    "rerun_completed_gates": false,
                    "gate_environment": {
                        "mode": "replace",
                        "variables": { "MODE": "test" },
                        "isolate_home": true,
                        "isolate_xdg": false,
                        "extension_inputs": [{
                            "id": "wordpress",
                            "source": "/opt/extensions/wordpress",
                            "identity": "sha256:content"
                        }]
                    }
                }
            }),
        );

        assert_eq!(
            command,
            vec![
                "homeboy",
                "agent-task",
                "--base",
                "release",
                "--verify",
                "cargo test --lib",
                "--private-verify",
                "./private-check",
                "--private-gate-reveal",
                "full-evidence",
                "--gate-timeout-seconds",
                "42",
                "--gate-heartbeat-interval-seconds",
                "7",
                "--gate-environment-mode",
                "replace",
                "--gate-env",
                "MODE=test",
                "--isolate-gate-home=true",
                "--isolate-gate-xdg=false",
                "--gate-extension-input",
                "{\"id\":\"wordpress\",\"identity\":\"sha256:content\",\"source\":\"/opt/extensions/wordpress\"}",
            ]
        );
    }

    #[test]
    fn promotion_candidates_canonicalize_aliases_and_preserve_attempt_choices() {
        let temp = tempfile::tempdir().expect("tempdir");
        let equivalent_aggregate = recoverable_review_aggregate(&temp, &[1, 1]);
        let equivalent_review =
            AgentTaskAggregateReport::from(equivalent_aggregate.outcomes.clone());
        assert_eq!(equivalent_review.apply_candidates.len(), 0);
        assert_eq!(equivalent_review.review_candidates.len(), 1);
        let equivalent = promotion_candidates(
            PromotionCandidateContext {
                source: "review-run",
                source_run_id: None,
                aggregate_path: None,
                to_worktree: Some("fixture@target"),
                cook_base: None,
                provider_command: None,
                provider_argv: &[],
                latest_promotion: None,
            },
            &equivalent_aggregate,
            &equivalent_review,
        );
        assert_eq!(equivalent.len(), 1);
        assert_eq!(equivalent[0]["artifact_id"], "candidate-0");
        assert_eq!(equivalent[0]["selection_required"], false);
        assert_eq!(equivalent[0]["command"][9], "fixture@target");

        let distinct_aggregate = recoverable_review_aggregate(&temp, &[1, 2]);
        let distinct_review = AgentTaskAggregateReport::from(distinct_aggregate.outcomes.clone());
        let distinct = promotion_candidates(
            PromotionCandidateContext {
                source: "review-run",
                source_run_id: None,
                aggregate_path: None,
                to_worktree: Some("fixture@target"),
                cook_base: None,
                provider_command: None,
                provider_argv: &[],
                latest_promotion: None,
            },
            &distinct_aggregate,
            &distinct_review,
        );
        assert_eq!(distinct.len(), 2);
        assert!(distinct
            .iter()
            .all(|candidate| candidate["selection_required"] == true));
    }

    #[test]
    fn typed_test_steps_and_overrides_have_explicit_grammar() {
        let step = parse_test_step("cargo test dossier=>all tests pass").expect("typed step");
        assert_eq!(step.command, "cargo test dossier");
        assert_eq!(step.expected, "all tests pass");
        assert!(parse_test_step("cargo test dossier").is_err());
        assert!(parse_test_step("=>all tests pass").is_err());
        assert!(parse_test_step("cargo test dossier=>").is_err());

        let override_ = parse_override("summary=Reviewed summary@operator").expect("override");
        assert!(matches!(
            override_.target,
            AgentTaskReviewOverrideTarget::Summary
        ));
        assert_eq!(override_.provenance, "operator");
        assert!(parse_override("evidence=nope@operator").is_err());
        assert!(parse_override("summary=@operator").is_err());
        assert!(parse_override("summary=Reviewed summary@").is_err());
        assert!(parse_public_contract("cli.finalize-pr=>").is_err());
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
            verified_base: Some(
                homeboy::agents::agent_tasks::promotion::AgentTaskPromotionVerifiedBase {
                    base: "release".to_string(),
                    sha: "0123456789012345678901234567890123456789".to_string(),
                },
            ),
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
        assert_eq!(
            handoff["finalize_command"],
            "homeboy agent-task finalize-pr --recover agent-task-run-1"
        );
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
            Some("agent-task-1234"),
        );

        assert_eq!(handoff["states"]["patch_artifact_produced"], true);
        assert_eq!(handoff["states"]["patch_promoted"], true);
        assert_eq!(handoff["states"]["pr_opened"], true);
        assert_eq!(handoff["boundary"], "pr_opened");
        assert_eq!(
            handoff["pr_url"],
            "https://github.com/Extra-Chill/homeboy/pull/9999"
        );
        assert!(
            handoff["finalize_command"].is_null(),
            "an executed publication has nothing left to apply"
        );
    }

    /// A validated preflight suppressed publication on purpose, so it must not
    /// be rendered with failed-publication wording (#9867).
    #[test]
    fn finalization_handoff_distinguishes_validated_preflight_from_failed_publication() {
        let handoff = finalization_handoff("validated", None, Some("agent-task-9867"));

        assert_eq!(handoff["boundary"], "publication_validated_not_executed");
        assert_eq!(handoff["states"]["pr_opened"], false);
        assert_eq!(handoff["states"]["publication_mutated"], false);
        assert_eq!(
            handoff["finalize_command"],
            "homeboy agent-task finalize-pr --recover agent-task-9867"
        );

        let actions = handoff["next_actions"]
            .as_array()
            .expect("next actions")
            .iter()
            .filter_map(|action| action.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            actions.contains("no commit, push, or PR mutation occurred by design"),
            "must state the suppression was intentional: {actions}"
        );
        assert!(
            actions.contains("homeboy agent-task finalize-pr --recover agent-task-9867"),
            "must name the exact apply command: {actions}"
        );
        assert!(
            !actions.contains("inspect finalization status"),
            "error-inspection guidance is reserved for real failures: {actions}"
        );
    }

    /// A genuinely failed publication keeps the error-inspection guidance.
    #[test]
    fn finalization_handoff_keeps_error_guidance_for_failed_publication() {
        let handoff = finalization_handoff("failed", None, Some("agent-task-9867"));

        assert_eq!(handoff["boundary"], "pr_not_opened");
        assert!(handoff["finalize_command"].is_null());
        assert_eq!(
            handoff["next_actions"][0],
            "PR was not opened; inspect finalization status and git/PR errors"
        );
    }

    #[test]
    fn manual_preflight_rejects_dirty_candidates_and_recovers_committed_candidates_idempotently() {
        homeboy::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("fixture root");
            let remote = root.path().join("origin.git");
            let checkout = root.path().join("checkout");
            run_git(
                root.path(),
                &[
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    remote.to_str().expect("remote path"),
                ],
            );
            run_git(
                root.path(),
                &[
                    "init",
                    "--initial-branch=main",
                    checkout.to_str().expect("checkout path"),
                ],
            );
            run_git(&checkout, &["config", "user.email", "test@example.test"]);
            run_git(&checkout, &["config", "user.name", "Finalization Test"]);
            std::fs::write(
                checkout.join("homeboy.json"),
                r#"{"id":"manual-finalization","remote_url":"https://github.com/example/manual-finalization.git"}"#,
            )
            .expect("write portable component config");
            std::fs::write(checkout.join("base.txt"), "base\n").expect("write base");
            run_git(&checkout, &["add", "."]);
            run_git(&checkout, &["commit", "-m", "base"]);
            run_git(
                &checkout,
                &[
                    "remote",
                    "add",
                    "origin",
                    remote.to_str().expect("remote path"),
                ],
            );
            run_git(&checkout, &["push", "-u", "origin", "main"]);
            let base_sha = git_output(&checkout, &["rev-parse", "HEAD"]);
            run_git(&checkout, &["checkout", "-b", "feature"]);
            std::fs::write(checkout.join("feature.txt"), "feature\n").expect("write feature");
            run_git(
                &checkout,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "git@github.com:example/manual-finalization.git",
                ],
            );
            let error = dispatch_agent_task_error(&[
                "homeboy",
                "agent-task",
                "finalize-pr",
                "--manual-finalization",
                "--preflight",
                "--run-id",
                "manual-cli-dirty-12706",
                "--path",
                checkout.to_str().expect("checkout path"),
                "--base",
                "main",
                "--verified-base-sha",
                &base_sha,
                "--head",
                "feature",
                "--title",
                "Dirty manual preflight",
                "--commit-message",
                "fixture",
                "--gate-result",
                "fixture=passed",
                "--changed-file",
                "feature.txt",
                "--targeted-check-run",
                "cargo test fixture",
                "--ai-model",
                "fixture-model",
                "--ai-used-for",
                "CLI dirty preflight coverage",
            ]);
            assert!(
                error
                    .message
                    .contains("recoverable manual preflight requires a committed candidate"),
                "unexpected error: {error:?}"
            );
            assert!(error.message.contains("without --preflight"));
            assert!(agent_task_lifecycle::status("manual-cli-dirty-12706")
                .expect("manual identity was reserved")
                .metadata["manual_finalization_intent"]
                .is_null());
            run_git(
                &checkout,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    remote.to_str().expect("remote path"),
                ],
            );
            run_git(&checkout, &["add", "."]);
            run_git(&checkout, &["commit", "-m", "feature"]);
            run_git(&checkout, &["push", "-u", "origin", "feature"]);
            run_git(
                &checkout,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "git@github.com:example/manual-finalization.git",
                ],
            );

            let bin = root.path().join("bin");
            std::fs::create_dir(&bin).expect("fake gh bin");
            let ssh = bin.join("ssh");
            std::fs::write(
                &ssh,
                r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    *git-upload-pack*) exec git-upload-pack "$HOMEBOY_FAKE_GIT_REMOTE" ;;
    *git-receive-pack*) exec git-receive-pack "$HOMEBOY_FAKE_GIT_REMOTE" ;;
  esac
done
exit 2
"#,
            )
            .expect("write fake SSH transport");
            let gh = bin.join("gh");
            std::fs::write(
                &gh,
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOMEBOY_FAKE_GH_LOG"
case "$1 $2" in
  "auth status"|"repo view") printf '%s\n' '{"nameWithOwner":"example/manual-finalization"}' ;;
  "pr list") printf '%s\n' '[]' ;;
  "pr create") printf '%s\n' 'https://github.com/example/manual-finalization/pull/1' ;;
  "pr view") sha=$(git rev-parse HEAD); printf '{"baseRefName":"main","headRefName":"feature","headRefOid":"%s","headRepository":{"nameWithOwner":"example/manual-finalization"}}\n' "$sha" ;;
  *) [ "$1" = "--version" ] || exit 2 ;;
esac
"#,
            )
            .expect("write fake gh");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
                    .expect("make fake SSH transport executable");
                std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))
                    .expect("make fake gh executable");
            }
            let log = root.path().join("gh.log");
            let old_path = std::env::var_os("PATH");
            let old_ssh_command = std::env::var_os("GIT_SSH_COMMAND");
            std::env::set_var(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    old_path.as_deref().unwrap_or_default().to_string_lossy()
                ),
            );
            std::env::set_var("HOMEBOY_FAKE_GH_LOG", &log);
            std::env::set_var("HOMEBOY_FAKE_GIT_REMOTE", &remote);
            std::env::set_var("GIT_SSH_COMMAND", &ssh);

            let preflight = dispatch_agent_task(&[
                "homeboy",
                "agent-task",
                "finalize-pr",
                "--manual-finalization",
                "--preflight",
                "--run-id",
                "manual-cli-11974",
                "--path",
                checkout.to_str().expect("checkout path"),
                "--base",
                "main",
                "--verified-base-sha",
                &base_sha,
                "--head",
                "feature",
                "--title",
                "Manual continuation",
                "--commit-message",
                "fixture",
                "--gate-result",
                "fixture=passed",
                "--changed-file",
                "feature.txt",
                "--targeted-check-run",
                "cargo test fixture",
                "--ai-model",
                "fixture-model",
                "--ai-used-for",
                "CLI recovery coverage",
            ]);
            assert_eq!(preflight["status"], "validated");
            let continuation = preflight["handoff"]["finalize_command"]
                .as_str()
                .expect("emitted continuation")
                .to_string();
            let argv = continuation.split_whitespace().collect::<Vec<_>>();
            let published = dispatch_agent_task(&argv);
            assert_eq!(published["status"], "review_ready");
            let repeated = dispatch_agent_task(&argv);
            assert_eq!(repeated, published);
            let error = dispatch_agent_task_error(&[
                "homeboy",
                "agent-task",
                "finalize-pr",
                "--recover",
                "missing-manual-finalization",
            ]);
            assert!(error
                .message
                .contains("no durable Cook recipe or manual finalization record"));
            let created = std::fs::read_to_string(&log)
                .expect("fake gh log")
                .lines()
                .filter(|line| line.starts_with("pr create "))
                .count();
            assert_eq!(
                created, 1,
                "the emitted continuation publishes exactly once"
            );

            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
            std::env::remove_var("HOMEBOY_FAKE_GH_LOG");
            std::env::remove_var("HOMEBOY_FAKE_GIT_REMOTE");
            match old_ssh_command {
                Some(command) => std::env::set_var("GIT_SSH_COMMAND", command),
                None => std::env::remove_var("GIT_SSH_COMMAND"),
            }
        });
    }

    fn dispatch_agent_task(argv: &[&str]) -> Value {
        let cli = crate::cli_surface::Cli::try_parse_from(argv).expect("parse CLI command");
        let crate::cli_surface::Commands::AgentTask(args) = cli.command else {
            panic!("expected agent-task command");
        };
        let (value, exit_code) =
            crate::commands::agent_task::run(args).expect("dispatch CLI command");
        assert_eq!(exit_code, 0, "CLI command succeeds");
        value
    }

    fn dispatch_agent_task_error(argv: &[&str]) -> homeboy::core::Error {
        let cli = crate::cli_surface::Cli::try_parse_from(argv).expect("parse CLI command");
        let crate::cli_surface::Commands::AgentTask(args) = cli.command else {
            panic!("expected agent-task command");
        };
        crate::commands::agent_task::run(args).expect_err("CLI command fails")
    }

    fn run_git(path: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_output(path: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout)
            .expect("Git output is UTF-8")
            .trim()
            .to_string()
    }

    #[test]
    fn durable_run_manual_preflight_does_not_persist_manual_intent() {
        assert!(!should_persist_manual_preflight_intent(
            true,
            "validated",
            false,
        ));
        assert!(should_persist_manual_preflight_intent(
            true,
            "validated",
            true,
        ));
    }

    fn provider_fixture(id: &str, backend: &str) -> AgentTaskExecutorProvider {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "backend": backend,
            "extension_id": format!("{backend}.extension"),
            "runtime_id": format!("{backend}-runtime"),
        }))
        .expect("provider fixture")
    }

    #[test]
    fn providers_scope_filters_to_requested_backend_by_default() {
        let all = vec![
            provider_fixture("opencode.executor", "opencode"),
            provider_fixture("codex.executor", "codex"),
            provider_fixture("opencode.executor-2", "opencode"),
        ];

        // `--backend opencode` (no `--catalog`) scopes to opencode only.
        let backend = scoped_provider_backend(Some("opencode"), false);
        assert_eq!(backend, Some("opencode"));
        let scoped = scope_providers(&all, backend);
        assert_eq!(scoped.len(), 2);
        assert!(scoped.iter().all(|provider| provider.backend == "opencode"));
    }

    #[test]
    fn providers_scope_catalog_flag_returns_full_multi_backend_set() {
        let all = vec![
            provider_fixture("opencode.executor", "opencode"),
            provider_fixture("codex.executor", "codex"),
        ];

        // `--catalog` overrides `--backend` and returns everything.
        let backend = scoped_provider_backend(Some("opencode"), true);
        assert_eq!(backend, None);
        assert_eq!(scope_providers(&all, backend).len(), 2);

        // An absent `--backend` also returns the full set.
        let unscoped = scoped_provider_backend(None, false);
        assert_eq!(unscoped, None);
        assert_eq!(scope_providers(&all, unscoped).len(), 2);
    }

    #[test]
    fn providers_scope_unknown_backend_yields_empty_slice() {
        let all = vec![provider_fixture("opencode.executor", "opencode")];
        let scoped = scope_providers(&all, scoped_provider_backend(Some("nope"), false));
        assert!(scoped.is_empty());
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct SamplePayload {
        status: String,
    }

    #[test]
    fn deserialize_maybe_enveloped_unwraps_command_result_envelope() {
        // #9893: `promote --output` emits a command-result envelope whose outer
        // status is not a promotion status; the payload under `data` is used.
        let enveloped = r#"{
            "schema": "homeboy/command-result/v3",
            "success": false,
            "status": "failed",
            "data": { "status": "gate_failed" }
        }"#;
        let payload: SamplePayload =
            deserialize_maybe_enveloped(enveloped, "sample").expect("unwrap envelope");
        assert_eq!(payload.status, "gate_failed");
    }

    #[test]
    fn deserialize_maybe_enveloped_accepts_bare_report() {
        // A bare report (already `jq '.data'`-ed, or produced directly) still works.
        let bare = r#"{ "status": "applied" }"#;
        let payload: SamplePayload =
            deserialize_maybe_enveloped(bare, "sample").expect("bare report");
        assert_eq!(payload.status, "applied");
    }

    #[test]
    fn deserialize_maybe_enveloped_surfaces_invalid_json() {
        let err = deserialize_maybe_enveloped::<SamplePayload>("not json", "sample")
            .expect_err("invalid json should error");
        assert_eq!(
            err.code,
            homeboy::core::error::ErrorCode::ValidationInvalidJson
        );
    }
}

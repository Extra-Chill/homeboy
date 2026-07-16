use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::agent_task_promotion::AgentTaskPromotionReport;
use crate::core::agent_task_review_dossier::{
    enrich_dossier, render_review_dossier, AgentTaskReviewDossier, AgentTaskReviewProfile,
};
use crate::core::error::{Error, Result};
use crate::core::gate::{HomeboyGateKind, HomeboyGateResult, HomeboyGateStatus};
use crate::core::proof::HomeboyProof;
use crate::core::run_lifecycle_record::RunLifecycleRecord;

pub const AGENT_TASK_PR_FINALIZATION_SCHEMA: &str = "homeboy/agent-task-pr-finalization/v1";
pub const AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA: &str =
    "homeboy/agent-task-pr-finalization-outcome/v1";
pub const AGENT_TASK_PUBLICATION_INTENT_SCHEMA: &str = "homeboy/agent-task-publication-intent/v1";
pub const AGENT_TASK_PUBLICATION_PROOF_SCHEMA: &str = "homeboy/agent-task-publication-proof/v1";

mod backend;
mod proof;
mod schemas;

pub use backend::RealAgentTaskPrFinalizationBackend;
use schemas::{finalization_outcome_schema, publication_intent_schema, publication_proof_schema};
mod types;
pub use types::*;

pub fn finalize_pr(
    options: AgentTaskPrFinalizationOptions,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend(options, &mut RealAgentTaskPrFinalizationBackend)
}

fn validate_real_candidate_fingerprint(options: &AgentTaskPrFinalizationOptions) -> Result<()> {
    backend::validate_real_candidate_fingerprint(options)
}

pub fn finalize_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    mut options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
) -> Result<AgentTaskPrFinalizationReport> {
    let mut durable_changed_files = Vec::new();
    if !options.manual_finalization {
        let lifecycle = backend.hydrate_run(&options.run_id)?;
        validate_durable_publication_eligibility(&lifecycle)?;
        let gate_proof = backend.hydrate_gate_proof(&options.run_id)?;
        if gate_proof.run_id != options.run_id {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "durable gate proof belongs to a different run",
                None,
                None,
            ));
        }
        validate_gate_proof_binding(&gate_proof, &options)?;
        durable_changed_files = gate_proof.promotion.changed_files.clone();
        options.normalized_gate_results = gate_proof.promotion.gate_results;
        if options.normalized_gate_results.is_empty() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "durable gate proof contains no normalized deterministic gates",
                None,
                None,
            ));
        }
        options.review_dossier.ai_assistance.model = durable_model(&lifecycle)?;
        options.evidence.lifecycle = Some(lifecycle);
    }
    validate_green_gates(&options.normalized_gate_results)?;
    options.review_dossier.apply_overrides()?;
    enrich_dossier(
        &mut options.review_dossier,
        &options.evidence.source_refs,
        &options.evidence.artifact_refs,
        &options.normalized_gate_results,
        &options.evidence.verification.ci_expected,
        options.evidence.lifecycle.as_ref(),
    );
    options.review_dossier.validate(&options.review_profile)?;
    let proof = build_finalization_proof(&options, options.normalized_gate_results.clone());
    let current_branch = backend.current_branch(&options.path)?;
    let head = options
        .head
        .clone()
        .unwrap_or_else(|| current_branch.clone());
    if head != current_branch {
        return Err(Error::validation_invalid_argument(
            "head",
            "requested head does not match the checked-out branch; check out the requested branch before finalizing",
            Some(head),
            None,
        ));
    }
    refuse_protected_head(&head, &options.protected_branches)?;

    let base = backend.resolve_base(&options.path, &options.base)?;
    let candidate = backend.candidate_state(&options.path, &base, &head)?;
    let (mut changed_files, commit_required, push_required) = match candidate {
        AgentTaskPrCandidateState::Dirty { changed_files } => (changed_files, true, true),
        AgentTaskPrCandidateState::Committed {
            changed_files,
            push_required,
        } => (changed_files, false, push_required),
        AgentTaskPrCandidateState::Equivalent => (Vec::new(), false, false),
        AgentTaskPrCandidateState::Invalid { diagnostic } => {
            return Err(Error::validation_invalid_argument(
                "base",
                &diagnostic,
                None,
                None,
            ));
        }
    };
    if !options.changed_files.is_empty() {
        changed_files = options.changed_files.clone();
    } else if changed_files.is_empty() && !options.manual_finalization {
        changed_files = durable_changed_files;
    }
    changed_files.sort();
    changed_files.dedup();
    let intent = build_pr_publication_intent(&options, &head, &changed_files, proof.clone());
    validate_publication_intent(&intent)?;

    if changed_files.is_empty() {
        return Ok(report(
            &options,
            intent,
            &head,
            "no_changes",
            "none",
            None,
            None,
            changed_files,
            Some(proof),
            false,
            false,
        ));
    }

    if !options.manual_finalization {
        backend.validate_candidate(&options)?;
    }
    if commit_required {
        backend.commit_all(&options.path, &options.commit_message)?;
    }
    if push_required {
        backend.push_branch(&options.path, &head)?;
    }
    let body = render_review_dossier(&options.review_dossier, &options.review_profile);
    let existing = backend.find_open_pr(&options.path, &options.base, &head)?;
    let (action, pr) = match existing {
        Some(existing) => (
            "updated",
            backend.update_pr(&options.path, existing.number, &options.title, &body)?,
        ),
        None => (
            "created",
            backend.create_pr(&options.path, &options.base, &head, &options.title, &body)?,
        ),
    };

    Ok(report(
        &options,
        intent,
        &head,
        "review_ready",
        action,
        Some(pr.number),
        Some(pr.url),
        changed_files,
        Some(proof),
        commit_required,
        push_required,
    ))
}

fn validate_gate_proof_binding(
    gate_proof: &AgentTaskPrDurableGateProof,
    options: &AgentTaskPrFinalizationOptions,
) -> Result<()> {
    use crate::core::agent_task_promotion::AgentTaskPromotionStatus;
    if gate_proof.promotion.status != AgentTaskPromotionStatus::Applied {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "durable gate proof must record an applied promotion",
            None,
            None,
        ));
    }
    if gate_proof.promotion.source.run_id.as_deref() != Some(options.run_id.as_str()) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "durable gate proof promotion source belongs to a different run",
            None,
            None,
        ));
    }
    if gate_proof.promotion.target.path.as_deref() != Some(options.path.as_str()) {
        return Err(Error::validation_invalid_argument(
            "path",
            "durable gate proof promotion target does not match finalization path",
            None,
            None,
        ));
    }
    Ok(())
}

fn validate_green_gates(gates: &[HomeboyGateResult]) -> Result<()> {
    if gates.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_results",
            "at least one deterministic green gate is required before PR finalization",
            None,
            None,
        ));
    }
    let red: Vec<String> = gates
        .iter()
        .filter(|gate| gate.status != HomeboyGateStatus::Passed)
        .map(|gate| format!("{}={:?}", gate.name, gate.status))
        .collect();
    if !red.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_results",
            format!(
                "finalization requires green gates; red gates: {}",
                red.join(", ")
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn is_green_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "green" | "passed" | "pass" | "succeeded" | "success" | "ok"
    )
}

impl From<AgentTaskGateResult> for HomeboyGateResult {
    fn from(gate: AgentTaskGateResult) -> Self {
        gate_result_from_legacy(gate)
    }
}

pub(crate) fn gate_result_from_legacy(gate: AgentTaskGateResult) -> HomeboyGateResult {
    let status = if is_green_status(&gate.status) {
        HomeboyGateStatus::Passed
    } else {
        HomeboyGateStatus::Failed
    };
    let summary = match gate
        .detail
        .as_deref()
        .filter(|detail| !detail.trim().is_empty())
    {
        Some(detail) => format!("{}: {} ({detail})", gate.name, gate.status),
        None => format!("{}: {}", gate.name, gate.status),
    };

    HomeboyGateResult::new(
        format!("finalization.gate.{}", gate.name),
        gate.name.clone(),
        HomeboyGateKind::Command,
        status,
    )
    .summary(summary)
    .evidence(json!({
        "name": gate.name,
        "status": gate.status,
        "detail": gate.detail,
    }))
    .retryable(status == HomeboyGateStatus::Failed)
    .provenance(json!({
        "source_type": "AgentTaskGateResult",
    }))
}

fn refuse_protected_head(head: &str, protected_branches: &[String]) -> Result<()> {
    if protected_branches.iter().any(|branch| branch == head) {
        return Err(Error::validation_invalid_argument(
            "head",
            format!(
                "refusing to finalize directly on protected branch '{}'",
                head
            ),
            None,
            Some(protected_branches.to_vec()),
        ));
    }
    Ok(())
}

pub fn validate_publication_intent(intent: &AgentTaskPublicationIntent) -> Result<()> {
    if intent.schema != AGENT_TASK_PUBLICATION_INTENT_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "publication_intent.schema",
            "publication intent schema is not supported",
            None,
            Some(vec![intent.schema.clone()]),
        ));
    }
    if intent.run_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.run_id",
            "publication intent requires a run id",
            None,
            None,
        ));
    }
    if intent.action.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.action",
            "publication intent requires an action",
            None,
            None,
        ));
    }
    if intent.target.kind.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.target.kind",
            "publication intent requires a target kind",
            None,
            None,
        ));
    }
    if intent
        .target
        .head
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "publication_intent.target.head",
            "publication intent requires a target head ref",
            None,
            None,
        ));
    }
    Ok(())
}

fn build_pr_publication_intent(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    changed_files: &[String],
    proof: HomeboyProof,
) -> AgentTaskPublicationIntent {
    AgentTaskPublicationIntent {
        schema: AGENT_TASK_PUBLICATION_INTENT_SCHEMA.to_string(),
        run_id: options.run_id.clone(),
        action: "review_request".to_string(),
        target: AgentTaskPublicationTarget {
            kind: "code_review".to_string(),
            adapter: Some("github_pull_request".to_string()),
            path: Some(options.path.clone()),
            base: Some(options.base.clone()),
            head: Some(head.to_string()),
            url: None,
        },
        changed_files: changed_files.to_vec(),
        source_refs: options.evidence.source_refs.clone(),
        artifact_refs: options.evidence.artifact_refs.clone(),
        proof,
    }
}

fn publication_proof(
    intent: &AgentTaskPublicationIntent,
    status: &str,
    adapter_action: &str,
    adapter_ref: Option<String>,
) -> AgentTaskPublicationProof {
    let mut target = intent.target.clone();
    target.url = adapter_ref.clone();
    AgentTaskPublicationProof {
        schema: AGENT_TASK_PUBLICATION_PROOF_SCHEMA.to_string(),
        run_id: intent.run_id.clone(),
        status: status.to_string(),
        intent_schema: intent.schema.clone(),
        target,
        adapter_action: (adapter_action != "none").then(|| adapter_action.to_string()),
        adapter_ref,
        proof: intent.proof.clone(),
    }
}

fn report(
    options: &AgentTaskPrFinalizationOptions,
    mut publication_intent: AgentTaskPublicationIntent,
    head: &str,
    status: &str,
    pr_action: &str,
    pr_number: Option<u64>,
    pr_url: Option<String>,
    changed_files: Vec<String>,
    proof: Option<HomeboyProof>,
    committed: bool,
    pushed: bool,
) -> AgentTaskPrFinalizationReport {
    let normalized_gate_results = options.normalized_gate_results.clone();
    let proof =
        proof.unwrap_or_else(|| build_finalization_proof(options, normalized_gate_results.clone()));
    publication_intent.proof = proof.clone();
    let publication_proof =
        publication_proof(&publication_intent, status, pr_action, pr_url.clone());
    let finalization_outcome = finalization_outcome(
        &publication_intent,
        &publication_proof,
        status,
        pr_action,
        pr_number,
        pr_url.clone(),
        &changed_files,
        committed,
        pushed,
    );
    AgentTaskPrFinalizationReport {
        schema: AGENT_TASK_PR_FINALIZATION_SCHEMA.to_string(),
        run_id: options.run_id.clone(),
        status: status.to_string(),
        path: options.path.clone(),
        base: options.base.clone(),
        head: head.to_string(),
        pr_action: pr_action.to_string(),
        pr_number,
        pr_url,
        changed_files,
        gate_results: options.gate_results.clone(),
        normalized_gate_results,
        proof,
        publication_intent,
        publication_proof,
        finalization_outcome,
        review_dossier: options.review_dossier.clone(),
        manual_finalization: options.manual_finalization,
        evidence: options.evidence.clone(),
    }
}

fn validate_durable_publication_eligibility(lifecycle: &RunLifecycleRecord) -> Result<()> {
    use crate::core::run_lifecycle_record::{ProviderRuntimeState, RunExecutionState};
    if lifecycle.execution.state != RunExecutionState::Succeeded
        || lifecycle.provider_runtime.is_empty()
        || lifecycle
            .provider_runtime
            .iter()
            .any(|runtime| runtime.state != ProviderRuntimeState::Succeeded)
    {
        return Err(Error::validation_invalid_argument("run_id", "durable run must have succeeded execution and succeeded provider runtime before publication", None, None));
    }
    Ok(())
}

fn durable_model(lifecycle: &RunLifecycleRecord) -> Result<String> {
    let model = lifecycle
        .provider_runtime
        .iter()
        .find_map(|runtime| {
            runtime
                .metadata
                .get("model")
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or_default()
        .to_string();
    if model.trim().is_empty()
        || matches!(
            model.trim().to_ascii_lowercase().as_str(),
            "not recorded" | "unknown" | "ai-assisted" | "ai assisted"
        )
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "durable provider metadata must record a concrete model before publication",
            None,
            None,
        ));
    }
    Ok(model)
}

fn finalization_outcome(
    intent: &AgentTaskPublicationIntent,
    publication_proof: &AgentTaskPublicationProof,
    status: &str,
    pr_action: &str,
    pr_number: Option<u64>,
    pr_url: Option<String>,
    changed_files: &[String],
    committed: bool,
    pushed: bool,
) -> AgentTaskPrFinalizationOutcome {
    let published = matches!(pr_action, "created" | "updated");
    AgentTaskPrFinalizationOutcome {
        schema: AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA.to_string(),
        run_id: intent.run_id.clone(),
        status: status.to_string(),
        publication_status: publication_proof.status.clone(),
        publication_action: pr_action.to_string(),
        target: publication_proof.target.clone(),
        base: intent.target.base.clone().unwrap_or_default(),
        head: intent.target.head.clone().unwrap_or_default(),
        pr_number,
        pr_url,
        changed_files: changed_files.to_vec(),
        committed,
        pushed,
        published,
    }
}

fn build_finalization_proof(
    options: &AgentTaskPrFinalizationOptions,
    gates: Vec<HomeboyGateResult>,
) -> HomeboyProof {
    proof::build_finalization_proof(options, gates)
}

#[cfg(test)]
mod tests;

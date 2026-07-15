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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateResult {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentTaskPrFinalizationReport {
    pub schema: String,
    pub run_id: String,
    pub status: String,
    pub path: String,
    pub base: String,
    pub head: String,
    pub pr_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    pub changed_files: Vec<String>,
    pub gate_results: Vec<AgentTaskGateResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normalized_gate_results: Vec<HomeboyGateResult>,
    pub proof: HomeboyProof,
    pub publication_intent: AgentTaskPublicationIntent,
    pub publication_proof: AgentTaskPublicationProof,
    pub finalization_outcome: AgentTaskPrFinalizationOutcome,
    pub review_dossier: AgentTaskReviewDossier,
    pub manual_finalization: bool,
    #[serde(flatten)]
    pub evidence: AgentTaskPrEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrFinalizationOutcome {
    #[serde(default = "finalization_outcome_schema")]
    pub schema: String,
    pub run_id: String,
    pub status: String,
    pub publication_status: String,
    pub publication_action: String,
    pub target: AgentTaskPublicationTarget,
    pub base: String,
    pub head: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    pub committed: bool,
    pub pushed: bool,
    pub published: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPublicationIntent {
    #[serde(default = "publication_intent_schema")]
    pub schema: String,
    pub run_id: String,
    pub action: String,
    pub target: AgentTaskPublicationTarget,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    pub proof: HomeboyProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPublicationTarget {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPublicationProof {
    #[serde(default = "publication_proof_schema")]
    pub schema: String,
    pub run_id: String,
    pub status: String,
    pub intent_schema: String,
    pub target: AgentTaskPublicationTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_ref: Option<String>,
    pub proof: HomeboyProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrEvidence {
    pub source_refs: Vec<String>,
    pub artifact_refs: Vec<String>,
    pub attempt_summary: String,
    pub ai_tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskPrSourceRelationship::is_empty"
    )]
    pub source_relationship: AgentTaskPrSourceRelationship,
    #[serde(default, skip_serializing_if = "AgentTaskPrVerification::is_empty")]
    pub verification: AgentTaskPrVerification,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskPrRuntimeGuardrails::is_empty"
    )]
    pub runtime_guardrails: AgentTaskPrRuntimeGuardrails,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<RunLifecycleRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrSourceRelationship {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_finding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_packet_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

impl AgentTaskPrSourceRelationship {
    pub fn is_empty(&self) -> bool {
        self.related_finding_id.is_none()
            && self.source_packet_id.is_none()
            && self.change_kind.is_none()
            && self.supersedes.is_empty()
            && self.depends_on.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrVerification {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targeted_checks_run: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targeted_checks_unavailable: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ci_expected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_reviewer_check: Option<String>,
}

impl AgentTaskPrVerification {
    pub fn is_empty(&self) -> bool {
        self.targeted_checks_run.is_empty()
            && self.targeted_checks_unavailable.is_none()
            && self.ci_expected.is_empty()
            && self.manual_reviewer_check.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPrRuntimeGuardrails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_not_broader_than_packet: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_discriminators: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nearby_contracts_preserved: Vec<String>,
}

impl AgentTaskPrRuntimeGuardrails {
    pub fn is_empty(&self) -> bool {
        self.why_not_broader_than_packet.is_none()
            && self.evidence_discriminators.is_empty()
            && self.nearby_contracts_preserved.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct AgentTaskPrFinalizationOptions {
    pub path: String,
    pub run_id: String,
    pub base: String,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub gate_results: Vec<AgentTaskGateResult>,
    pub normalized_gate_results: Vec<HomeboyGateResult>,
    pub changed_files: Vec<String>,
    pub evidence: AgentTaskPrEvidence,
    pub ai_used_for: String,
    pub review_dossier: AgentTaskReviewDossier,
    pub review_profile: AgentTaskReviewProfile,
    /// Manual finalization is an explicit migration mode for work not produced by a durable run.
    pub manual_finalization: bool,
    pub protected_branches: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskPrRef {
    pub number: u64,
    pub url: String,
}

/// The complete Git candidate classification, determined before finalization
/// mutates the worktree or remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTaskPrCandidateState {
    Dirty {
        changed_files: Vec<String>,
    },
    Committed {
        changed_files: Vec<String>,
        push_required: bool,
    },
    Equivalent,
    Invalid {
        diagnostic: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskPrResolvedBase {
    pub reference: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskPrDurableGateProof {
    pub run_id: String,
    pub promotion: AgentTaskPromotionReport,
}

pub trait AgentTaskPrFinalizationBackend {
    fn hydrate_run(&mut self, run_id: &str) -> Result<RunLifecycleRecord>;
    fn hydrate_gate_proof(&mut self, run_id: &str) -> Result<AgentTaskPrDurableGateProof>;
    /// Real finalization binds the exact promoted bytes immediately before the
    /// first mutation. Test backends can focus on publication behavior.
    fn validate_candidate(&mut self, _options: &AgentTaskPrFinalizationOptions) -> Result<()> {
        Ok(())
    }
    fn current_branch(&mut self, path: &str) -> Result<String>;
    fn changed_files(&mut self, path: &str) -> Result<Vec<String>>;
    fn resolve_base(&mut self, _path: &str, base: &str) -> Result<AgentTaskPrResolvedBase> {
        Ok(AgentTaskPrResolvedBase {
            reference: base.to_string(),
            sha: String::new(),
        })
    }
    fn candidate_state(
        &mut self,
        path: &str,
        _base: &AgentTaskPrResolvedBase,
        _head: &str,
    ) -> Result<AgentTaskPrCandidateState> {
        let changed_files = self.changed_files(path)?;
        Ok(if changed_files.is_empty() {
            AgentTaskPrCandidateState::Equivalent
        } else {
            AgentTaskPrCandidateState::Dirty { changed_files }
        })
    }
    fn commit_all(&mut self, path: &str, message: &str) -> Result<()>;
    fn push_branch(&mut self, path: &str, head: &str) -> Result<()>;
    fn find_open_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<AgentTaskPrRef>>;
    fn create_pr(
        &mut self,
        path: &str,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef>;
    fn update_pr(
        &mut self,
        path: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<AgentTaskPrRef>;
}

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
mod tests {
    use super::*;
    use crate::core::run_lifecycle_record::{
        ArtifactRetentionLifecycle, ArtifactRetentionStatus, CleanupLifecycle, CleanupState,
        ExternalRuntimeId, FinalizationLifecycle, FinalizationState, ProviderRuntimeLifecycle,
        ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
    };
    use crate::core::{
        agent_task::{
            AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest,
            AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
        },
        agent_task_scheduler::{
            AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals, AgentTaskPlan,
            AgentTaskProgressEvent, AgentTaskQueueStatus, AgentTaskState,
            AGENT_TASK_AGGREGATE_SCHEMA,
        },
    };
    use std::process::Command;

    #[derive(Default)]
    struct MockBackend {
        branch: String,
        changed_files: Vec<String>,
        candidate_state: Option<AgentTaskPrCandidateState>,
        existing_pr: Option<AgentTaskPrRef>,
        create_error: bool,
        push_error: bool,
        hydrate_error: bool,
        hydrate_run_id: Option<String>,
        lifecycle: Option<RunLifecycleRecord>,
        gate_proof: Option<AgentTaskPrDurableGateProof>,
        candidate: Option<crate::core::agent_task_promotion::AgentTaskPromotionCandidate>,
        committed: bool,
        pushed: bool,
        created: bool,
        create_calls: u8,
        changed_files_calls: u8,
        commit_calls: u8,
        push_calls: u8,
        updated: bool,
        last_body: String,
    }

    impl AgentTaskPrFinalizationBackend for MockBackend {
        fn hydrate_run(&mut self, _run_id: &str) -> Result<RunLifecycleRecord> {
            if self.hydrate_error {
                return Err(Error::validation_invalid_argument(
                    "run_id",
                    "durable run was not found",
                    None,
                    None,
                ));
            }
            if let Some(run_id) = self.hydrate_run_id.as_deref() {
                return RealAgentTaskPrFinalizationBackend.hydrate_run(run_id);
            }
            Ok(self.lifecycle.clone().unwrap_or_default())
        }
        fn hydrate_gate_proof(&mut self, _run_id: &str) -> Result<AgentTaskPrDurableGateProof> {
            self.gate_proof.clone().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "run_id",
                    "normal finalization requires durable deterministic gate proof",
                    None,
                    None,
                )
            })
        }
        fn validate_candidate(&mut self, options: &AgentTaskPrFinalizationOptions) -> Result<()> {
            let Some(expected) = self.candidate.as_ref() else {
                return Ok(());
            };
            let actual = crate::core::agent_task_promotion::candidate_fingerprint(&options.path)?;
            if actual != *expected {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "candidate changed after promotion; rerun promotion gates before finalization",
                    None,
                    None,
                ));
            }
            let crate::core::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } =
                actual
            else {
                unreachable!("test candidate is Git")
            };
            let mut changed_files = options.changed_files.clone();
            changed_files.sort();
            changed_files.dedup();
            if !changed_files.is_empty() && changed_files != fingerprint.changed_files {
                return Err(Error::validation_invalid_argument(
                    "changed-file",
                    "caller changed files do not match promoted candidate",
                    None,
                    None,
                ));
            }
            Ok(())
        }
        fn current_branch(&mut self, _path: &str) -> Result<String> {
            Ok(if self.branch.is_empty() {
                "fix/cook".to_string()
            } else {
                self.branch.clone()
            })
        }

        fn changed_files(&mut self, _path: &str) -> Result<Vec<String>> {
            self.changed_files_calls += 1;
            Ok(self.changed_files.clone())
        }

        fn candidate_state(
            &mut self,
            _path: &str,
            _base: &AgentTaskPrResolvedBase,
            _head: &str,
        ) -> Result<AgentTaskPrCandidateState> {
            Ok(self.candidate_state.clone().unwrap_or_else(|| {
                if self.changed_files.is_empty() {
                    AgentTaskPrCandidateState::Equivalent
                } else {
                    AgentTaskPrCandidateState::Dirty {
                        changed_files: self.changed_files.clone(),
                    }
                }
            }))
        }

        fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
            self.committed = true;
            self.commit_calls += 1;
            Ok(())
        }

        fn push_branch(&mut self, _path: &str, _head: &str) -> Result<()> {
            if self.push_error {
                return Err(Error::git_command_failed("git push failed"));
            }
            self.pushed = true;
            self.push_calls += 1;
            Ok(())
        }

        fn find_open_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<Option<AgentTaskPrRef>> {
            Ok(self.existing_pr.clone())
        }

        fn create_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            if self.create_error {
                return Err(Error::git_command_failed("gh pr create failed"));
            }
            self.created = true;
            self.create_calls += 1;
            self.last_body = body.to_string();
            Ok(AgentTaskPrRef {
                number: 123,
                url: "https://github.com/Extra-Chill/homeboy/pull/123".to_string(),
            })
        }

        fn update_pr(
            &mut self,
            _path: &str,
            number: u64,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            self.updated = true;
            self.last_body = body.to_string();
            Ok(AgentTaskPrRef {
                number,
                url: format!("https://github.com/Extra-Chill/homeboy/pull/{}", number),
            })
        }
    }

    #[test]
    fn creates_new_pr_after_green_gates() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "review_ready");
        assert_eq!(report.pr_action, "created");
        assert_eq!(report.pr_number, Some(123));
        assert!(backend.committed);
        assert!(backend.pushed);
        assert!(backend.created);
        assert_eq!(backend.changed_files_calls, 0);
        assert_eq!(backend.commit_calls, 1);
        assert_eq!(backend.push_calls, 1);
        assert!(backend.last_body.contains("## AI assistance"));
        assert!(backend.last_body.contains("## Summary"));
        assert!(backend.last_body.contains("## How to test"));
        assert!(!backend.last_body.contains("Publication intent"));
        assert_eq!(
            report.publication_intent.schema,
            AGENT_TASK_PUBLICATION_INTENT_SCHEMA
        );
        assert_eq!(report.publication_intent.action, "review_request");
        assert_eq!(report.publication_intent.target.kind, "code_review");
        assert_eq!(
            report.publication_intent.target.adapter.as_deref(),
            Some("github_pull_request")
        );
        assert_eq!(
            report.publication_proof.schema,
            AGENT_TASK_PUBLICATION_PROOF_SCHEMA
        );
        assert_eq!(
            report.publication_proof.adapter_action.as_deref(),
            Some("created")
        );
        assert_eq!(
            report.publication_proof.adapter_ref.as_deref(),
            Some("https://github.com/Extra-Chill/homeboy/pull/123")
        );
        assert_eq!(
            report.finalization_outcome.schema,
            AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA
        );
        assert_eq!(report.finalization_outcome.status, "review_ready");
        assert_eq!(
            report.finalization_outcome.publication_status,
            "review_ready"
        );
        assert_eq!(report.finalization_outcome.publication_action, "created");
        assert_eq!(report.finalization_outcome.base, "main");
        assert_eq!(report.finalization_outcome.head, "fix/cook");
        assert_eq!(report.finalization_outcome.pr_number, Some(123));
        assert_eq!(
            report.finalization_outcome.pr_url.as_deref(),
            Some("https://github.com/Extra-Chill/homeboy/pull/123")
        );
        assert_eq!(
            report.finalization_outcome.changed_files,
            vec!["src/lib.rs"]
        );
        assert!(report.finalization_outcome.committed);
        assert!(report.finalization_outcome.pushed);
        assert!(report.finalization_outcome.published);
    }

    #[test]
    fn pr_body_labels_ci_equivalent_gates() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.normalized_gate_results = vec![HomeboyGateResult::new(
            "gate-1",
            "required project gate",
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        )
        .evidence(json!({
            "command": ["sh", "-lc", "project verify"],
            "exit_code": 0,
            "ci_equivalent": true,
        }))];

        finalize_pr_with_backend(options, &mut backend).expect("finalized");

        assert!(backend.last_body.contains("## Evidence"));
    }

    #[test]
    fn pr_body_reports_iterator_evidence_metadata() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.evidence.source_relationship = AgentTaskPrSourceRelationship {
            related_finding_id: Some("finding-123".to_string()),
            source_packet_id: Some("packet-456".to_string()),
            change_kind: Some("runtime-fix".to_string()),
            supersedes: vec!["https://github.com/org/repo/pull/1".to_string()],
            depends_on: vec!["https://github.com/org/repo/pull/2".to_string()],
        };
        options.evidence.verification = AgentTaskPrVerification {
            targeted_checks_run: vec!["cargo test pr_body".to_string()],
            targeted_checks_unavailable: None,
            ci_expected: vec!["Homeboy CI".to_string()],
            manual_reviewer_check: None,
        };
        options.evidence.runtime_guardrails = AgentTaskPrRuntimeGuardrails {
            why_not_broader_than_packet: Some("Preserves class and href gates.".to_string()),
            evidence_discriminators: vec!["class=brand".to_string(), "href=#top".to_string()],
            nearby_contracts_preserved: vec!["is_branded_inline_anchor".to_string()],
        };

        finalize_pr_with_backend(options, &mut backend).expect("finalized");

        assert!(backend.last_body.contains("## AI assistance"));
        assert!(backend.last_body.contains("GPT-5.5"));
    }

    #[test]
    fn pr_body_reports_run_lifecycle_evidence() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.evidence.lifecycle = Some(RunLifecycleRecord {
            execution: RunExecutionLifecycle {
                state: RunExecutionState::Succeeded,
                started_at: None,
                finished_at: Some("2026-06-16T00:00:05Z".to_string()),
                updated_at: Some("2026-06-16T00:00:05Z".to_string()),
            },
            provider_runtime: vec![ProviderRuntimeLifecycle {
                task_id: "task-a".to_string(),
                backend: "sample-runtime".to_string(),
                state: ProviderRuntimeState::Succeeded,
                stream_uri: None,
                external_runtime_ids: vec![ExternalRuntimeId {
                    kind: "provider_run_id".to_string(),
                    value: "provider-run-123".to_string(),
                    provider: Some("sample-runtime".to_string()),
                    url: None,
                }],
                metadata: serde_json::Value::Null,
            }],
            external_runtime_ids: vec![ExternalRuntimeId {
                kind: "provider_run_id".to_string(),
                value: "provider-run-123".to_string(),
                provider: Some("sample-runtime".to_string()),
                url: None,
            }],
            cleanup: CleanupLifecycle {
                state: CleanupState::Preserved,
                policy: Some("preserve".to_string()),
                updated_at: None,
            },
            finalization: FinalizationLifecycle {
                state: FinalizationState::Pending,
                updated_at: None,
            },
            artifact_retention: ArtifactRetentionLifecycle {
                status: ArtifactRetentionStatus::Retained,
                policy: Some("retain".to_string()),
                updated_at: None,
            },
            ..RunLifecycleRecord::default()
        });

        finalize_pr_with_backend(options, &mut backend).expect("finalized");

        assert!(!backend.last_body.contains("Run lifecycle"));
    }

    #[test]
    fn updates_existing_pr_for_same_branch() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            existing_pr: Some(AgentTaskPrRef {
                number: 77,
                url: "https://github.com/Extra-Chill/homeboy/pull/77".to_string(),
            }),
            ..Default::default()
        };

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "review_ready");
        assert_eq!(report.pr_action, "updated");
        assert_eq!(report.pr_number, Some(77));
        assert!(backend.updated);
        assert!(!backend.created);
    }

    #[test]
    fn reports_no_changes_without_commit_push_or_pr() {
        let mut backend = MockBackend::default();

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.status, "no_changes");
        assert_eq!(report.pr_action, "none");
        assert_eq!(report.finalization_outcome.status, "no_changes");
        assert_eq!(report.finalization_outcome.publication_action, "none");
        assert!(!report.finalization_outcome.committed);
        assert!(!report.finalization_outcome.pushed);
        assert!(!report.finalization_outcome.published);
        assert!(!backend.committed);
        assert!(!backend.pushed);
        assert!(!backend.created);
        assert_eq!(backend.changed_files_calls, 0);
        assert_eq!(backend.commit_calls, 0);
        assert_eq!(backend.push_calls, 0);
    }

    #[test]
    fn publishes_clean_committed_candidate_without_committing() {
        let mut backend = MockBackend {
            candidate_state: Some(AgentTaskPrCandidateState::Committed {
                changed_files: vec!["deleted.rs".to_string(), "renamed.rs".to_string()],
                push_required: true,
            }),
            ..Default::default()
        };

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.pr_action, "created");
        assert_eq!(report.changed_files, vec!["deleted.rs", "renamed.rs"]);
        assert!(!backend.committed);
        assert!(backend.pushed);
        assert!(backend.created);
        assert!(!report.finalization_outcome.committed);
        assert!(report.finalization_outcome.pushed);
    }

    #[test]
    fn already_pushed_clean_committed_candidate_updates_open_pr_once() {
        let mut backend = MockBackend {
            candidate_state: Some(AgentTaskPrCandidateState::Committed {
                changed_files: vec!["src/lib.rs".to_string()],
                push_required: false,
            }),
            existing_pr: Some(AgentTaskPrRef {
                number: 77,
                url: "https://github.com/Extra-Chill/homeboy/pull/77".to_string(),
            }),
            ..Default::default()
        };

        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        assert_eq!(report.pr_action, "updated");
        assert!(!backend.committed);
        assert!(!backend.pushed);
        assert!(backend.updated);
        assert!(!backend.created);
    }

    #[test]
    fn synthetic_changed_file_does_not_commit_clean_committed_candidate() {
        let mut backend = MockBackend {
            candidate_state: Some(AgentTaskPrCandidateState::Committed {
                changed_files: vec!["src/lib.rs".to_string()],
                push_required: true,
            }),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.changed_files = vec!["synthetic.rs".to_string()];

        let report =
            finalize_pr_with_backend(finalization_options, &mut backend).expect("finalized");

        assert_eq!(report.changed_files, vec!["synthetic.rs"]);
        assert!(!backend.committed);
        assert!(backend.pushed);
    }

    #[test]
    fn invalid_base_or_diverged_candidate_stops_before_publication() {
        let mut backend = MockBackend {
            candidate_state: Some(AgentTaskPrCandidateState::Invalid {
                diagnostic: "HEAD is behind requested base `trunk`; rebase first".to_string(),
            }),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.base = "trunk".to_string();

        let error =
            finalize_pr_with_backend(finalization_options, &mut backend).expect_err("blocked");

        assert!(error.message.contains("behind requested base"));
        assert!(!backend.committed);
        assert!(!backend.pushed);
        assert!(!backend.created);
    }

    #[test]
    fn push_failure_stops_pr_publication_for_clean_committed_candidate() {
        let mut backend = MockBackend {
            candidate_state: Some(AgentTaskPrCandidateState::Committed {
                changed_files: vec!["src/lib.rs".to_string()],
                push_required: true,
            }),
            push_error: true,
            ..Default::default()
        };

        let error = finalize_pr_with_backend(options(), &mut backend).expect_err("push fails");

        assert!(error.message.contains("git push failed"));
        assert!(!backend.created);
        assert!(!backend.updated);
    }

    #[test]
    fn refuses_protected_branch() {
        let mut backend = MockBackend {
            branch: "main".to_string(),
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };

        let error = finalize_pr_with_backend(options(), &mut backend).expect_err("blocked");

        assert!(error.message.contains("protected branch"));
        assert!(!backend.committed);
    }

    #[test]
    fn propagates_pr_creation_failure() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            create_error: true,
            ..Default::default()
        };

        let error = finalize_pr_with_backend(options(), &mut backend).expect_err("failed");

        assert!(error.message.contains("gh pr create failed"));
        assert!(backend.committed);
        assert!(backend.pushed);
    }

    #[test]
    fn refuses_red_gates() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let mut options = options();
        options.gate_results[0].status = "failed".to_string();
        options.normalized_gate_results[0] =
            HomeboyGateResult::from(options.gate_results[0].clone());

        let error = finalize_pr_with_backend(options, &mut backend).expect_err("blocked");

        assert!(error.message.contains("green gates"));
        assert!(!backend.committed);
    }

    #[test]
    fn requires_a_real_durable_run_unless_manual_mode_is_selected() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            hydrate_error: true,
            ..Default::default()
        };
        let mut options = options();
        options.manual_finalization = false;
        let error = finalize_pr_with_backend(options, &mut backend)
            .expect_err("missing run blocks finalization");
        assert!(error.message.contains("durable run was not found"));
        assert!(!backend.committed);
    }

    #[test]
    fn durable_finalization_requires_successful_provider_run_and_hydrates_model() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
            gate_proof: Some(successful_gate_proof()),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.manual_finalization = false;
        finalization_options.review_dossier.ai_assistance.model = "caller model".to_string();
        let report = finalize_pr_with_backend(finalization_options, &mut backend)
            .expect("successful run finalizes");
        assert_eq!(
            report.review_dossier.ai_assistance.model,
            "openai/gpt-5.6-terra"
        );

        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(RunLifecycleRecord::default()),
            ..Default::default()
        };
        let mut queued_options = options();
        queued_options.manual_finalization = false;
        assert!(finalize_pr_with_backend(queued_options, &mut backend).is_err());

        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
            ..Default::default()
        };
        let mut proofless_options = options();
        proofless_options.manual_finalization = false;
        assert!(finalize_pr_with_backend(proofless_options, &mut backend).is_err());

        let mut mismatched = successful_gate_proof();
        mismatched.run_id = "unrelated-successful-run".to_string();
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
            gate_proof: Some(mismatched),
            ..Default::default()
        };
        let mut mismatched_options = options();
        mismatched_options.manual_finalization = false;
        assert!(finalize_pr_with_backend(mismatched_options, &mut backend).is_err());

        let mut failed = successful_lifecycle("openai/gpt-5.6-terra");
        failed.execution.state = RunExecutionState::Failed;
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(failed),
            ..Default::default()
        };
        let mut failed_options = options();
        failed_options.manual_finalization = false;
        assert!(finalize_pr_with_backend(failed_options, &mut backend).is_err());

        let mut manual_options = options();
        manual_options.review_dossier.ai_assistance.model = "unknown".to_string();
        assert!(finalize_pr_with_backend(
            manual_options,
            &mut MockBackend {
                changed_files: vec!["src/lib.rs".to_string()],
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn durable_finalization_accepts_succeeded_generic_executor_outcome_once() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            lifecycle: Some(generic_executor_lifecycle(
                ProviderRuntimeState::Succeeded,
                "openai/gpt-5.6-terra",
            )),
            gate_proof: Some(successful_gate_proof()),
            ..Default::default()
        };
        let mut finalization_options = options();
        finalization_options.manual_finalization = false;

        let report = finalize_pr_with_backend(finalization_options, &mut backend)
            .expect("succeeded generic executor outcome finalizes");

        assert_eq!(report.pr_action, "created");
        assert!(backend.committed && backend.pushed && backend.created);
        assert_eq!(backend.create_calls, 1);
        assert_eq!(
            report.evidence.lifecycle.as_ref().unwrap().provider_runtime[0].metadata
                ["evidence_source"],
            "canonical_executor_outcome"
        );
        assert!(
            report.evidence.lifecycle.as_ref().unwrap().provider_runtime[0]
                .external_runtime_ids
                .is_empty()
        );

        for rejected_state in [ProviderRuntimeState::Failed, ProviderRuntimeState::TimedOut] {
            let mut rejected_backend = MockBackend {
                changed_files: vec!["src/lib.rs".to_string()],
                lifecycle: Some(generic_executor_lifecycle(
                    rejected_state,
                    "openai/gpt-5.6-terra",
                )),
                gate_proof: Some(successful_gate_proof()),
                ..Default::default()
            };
            let mut rejected_options = options();
            rejected_options.manual_finalization = false;

            assert!(finalize_pr_with_backend(rejected_options, &mut rejected_backend).is_err());
            assert!(!rejected_backend.committed);
        }
    }

    #[test]
    fn durable_finalization_accepts_status_backfilled_legacy_executor_evidence() {
        crate::test_support::with_isolated_home(|_| {
            let run_id = legacy_generic_terminal_run("cook-3678", false);
            let mut backend = MockBackend {
                changed_files: vec!["src/lib.rs".to_string()],
                hydrate_run_id: Some(run_id),
                gate_proof: Some(successful_gate_proof()),
                ..Default::default()
            };
            let mut finalization_options = options();
            finalization_options.manual_finalization = false;

            let report = finalize_pr_with_backend(finalization_options, &mut backend)
                .expect("status-backfilled run finalizes");

            assert_eq!(report.pr_action, "created");
            assert!(backend.committed && backend.pushed && backend.created);
        });
    }

    #[test]
    fn durable_finalization_fails_closed_when_status_backfill_does_not_persist() {
        crate::test_support::with_isolated_home(|_| {
            let run_id = legacy_generic_terminal_run("failed-backfill-finalization", false);
            crate::core::agent_task_lifecycle::fail_next_record_write_for_test();
            let mut backend = MockBackend {
                hydrate_run_id: Some(run_id.clone()),
                gate_proof: Some(successful_gate_proof()),
                ..Default::default()
            };
            let mut finalization_options = options();
            finalization_options.run_id = run_id;
            finalization_options.manual_finalization = false;

            let error = finalize_pr_with_backend(finalization_options, &mut backend)
                .expect_err("unpersisted runtime evidence cannot finalize");

            assert_eq!(error.code, crate::core::ErrorCode::InternalIoError);
            assert!(!backend.committed);
        });
    }

    #[test]
    fn durable_finalization_rejects_status_backfilled_timed_out_evidence() {
        crate::test_support::with_isolated_home(|_| {
            let run_id = legacy_generic_terminal_run("timed-out-backfill-finalization", true);
            let mut backend = MockBackend {
                hydrate_run_id: Some(run_id.clone()),
                gate_proof: Some(successful_gate_proof()),
                ..Default::default()
            };
            let mut finalization_options = options();
            finalization_options.run_id = run_id;
            finalization_options.manual_finalization = false;

            assert!(finalize_pr_with_backend(finalization_options, &mut backend).is_err());
            assert!(!backend.committed);
        });
    }

    #[test]
    fn durable_finalization_accepts_native_and_generic_evidence_but_omits_skipped_work() {
        crate::test_support::with_isolated_home(|_| {
            let plan = AgentTaskPlan::new(
                "mixed-executor-plan",
                vec![
                    durable_task("native", "native-executor", Some("native-model")),
                    durable_task("generic", "opencode", Some("openai/gpt-5.6-terra")),
                    durable_task("skipped", "opencode", None),
                ],
            );
            let aggregate = AgentTaskAggregate {
                schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: plan.plan_id.clone(),
                status: AgentTaskAggregateStatus::Succeeded,
                totals: AgentTaskAggregateTotals {
                    succeeded: 2,
                    skipped: 1,
                    ..Default::default()
                },
                outcomes: vec![
                    durable_succeeded_outcome(
                        "native",
                        json!({
                            "provider": "native-executor",
                            "provider_run_id": "native-run-123",
                            "model": "native-model",
                        }),
                    ),
                    durable_succeeded_outcome("generic", serde_json::Value::Null),
                ],
                events: vec![AgentTaskProgressEvent {
                    task_id: "skipped".to_string(),
                    state: AgentTaskState::Skipped,
                    attempt: 1,
                    message: Some("not applicable".to_string()),
                }],
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: AgentTaskQueueStatus::default(),
            };
            let record = crate::core::agent_task_lifecycle::record_completed_run(
                &plan,
                &aggregate,
                Some("cook-3678"),
            )
            .expect("durable aggregate recorded");
            let runtimes = &record.lifecycle.provider_runtime;

            assert_eq!(
                record.state,
                crate::core::agent_task_lifecycle::AgentTaskRunState::Succeeded
            );
            assert_eq!(runtimes.len(), 2);
            assert_eq!(runtimes[0].external_runtime_ids[0].value, "native-run-123");
            assert_eq!(runtimes[1].backend, "opencode");
            assert_eq!(
                runtimes[1].metadata["evidence_source"],
                "canonical_executor_outcome"
            );
            assert!(runtimes.iter().all(|runtime| runtime.task_id != "skipped"));
            assert_eq!(record.artifact_refs[0].kind, "patch");

            let mut backend = MockBackend {
                changed_files: vec!["src/lib.rs".to_string()],
                lifecycle: Some(record.lifecycle),
                gate_proof: Some(successful_gate_proof()),
                ..Default::default()
            };
            let mut finalization_options = options();
            finalization_options.manual_finalization = false;

            let report = finalize_pr_with_backend(finalization_options.clone(), &mut backend)
                .expect("mixed durable evidence finalizes");
            assert_eq!(report.pr_action, "created");
            assert_eq!(backend.create_calls, 1);

            backend.existing_pr = Some(AgentTaskPrRef {
                number: 123,
                url: "https://github.com/Extra-Chill/homeboy/pull/123".to_string(),
            });
            let repeated = finalize_pr_with_backend(finalization_options, &mut backend)
                .expect("existing PR is reused");
            assert_eq!(repeated.pr_action, "updated");
            assert_eq!(backend.create_calls, 1);
            assert!(backend.updated);
        });
    }

    fn real_git_finalization_options(
        path: &std::path::Path,
        changed_files: Vec<String>,
    ) -> AgentTaskPrFinalizationOptions {
        let mut options = options();
        options.path = path.display().to_string();
        options.manual_finalization = false;
        options.changed_files = changed_files;
        options.evidence.source_refs =
            vec!["https://github.com/Extra-Chill/homeboy/issues/8058".to_string()];
        options
    }

    fn real_git_backend(
        path: &std::path::Path,
        candidate: crate::core::agent_task_promotion::AgentTaskPromotionCandidate,
    ) -> MockBackend {
        let mut gate_proof = successful_gate_proof();
        gate_proof.promotion.target.path = Some(path.display().to_string());
        MockBackend {
            lifecycle: Some(successful_lifecycle("openai/gpt-5.6-terra")),
            gate_proof: Some(gate_proof),
            candidate: Some(candidate),
            ..Default::default()
        }
    }

    fn real_git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            ["init"].as_slice(),
            ["config", "user.email", "test@example.com"].as_slice(),
            ["config", "user.name", "Test"].as_slice(),
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(dir.path().join("base"), "base").unwrap();
        assert!(Command::new("git")
            .args(["add", "base"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        dir
    }

    #[test]
    fn stale_candidate_mutation_is_rejected_before_commit_and_push() {
        let repo = real_git_repo();
        std::fs::write(repo.path().join("candidate"), "before").unwrap();
        let candidate =
            crate::core::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
                .unwrap();
        std::fs::write(repo.path().join("candidate"), "after").unwrap();
        let mut backend = real_git_backend(repo.path(), candidate);
        let error = finalize_pr_with_backend(
            real_git_finalization_options(repo.path(), vec!["candidate".to_string()]),
            &mut backend,
        )
        .expect_err("stale candidate");
        assert!(error.message.contains("candidate changed"));
        assert!(!backend.committed);
        assert!(!backend.pushed);
    }

    #[test]
    fn head_drift_is_rejected_before_commit_and_push() {
        let repo = real_git_repo();
        std::fs::write(repo.path().join("candidate"), "candidate").unwrap();
        let candidate =
            crate::core::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
                .unwrap();
        assert!(Command::new("git")
            .args(["add", "candidate"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "drift"])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success());
        let mut backend = real_git_backend(repo.path(), candidate);
        assert!(finalize_pr_with_backend(
            real_git_finalization_options(repo.path(), vec!["candidate".to_string()]),
            &mut backend
        )
        .is_err());
        assert!(!backend.committed);
        assert!(!backend.pushed);
    }

    #[test]
    fn changed_file_order_and_duplicates_are_normalized() {
        let repo = real_git_repo();
        std::fs::write(repo.path().join("a"), "a").unwrap();
        std::fs::write(repo.path().join("b"), "b").unwrap();
        let candidate =
            crate::core::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
                .unwrap();
        let mut backend = real_git_backend(repo.path(), candidate);
        let report = finalize_pr_with_backend(
            real_git_finalization_options(
                repo.path(),
                vec!["b".to_string(), "a".to_string(), "a".to_string()],
            ),
            &mut backend,
        )
        .expect("normalized changed files");
        assert_eq!(report.changed_files, vec!["a", "b"]);
        let mut mismatch = real_git_backend(
            repo.path(),
            crate::core::agent_task_promotion::candidate_fingerprint(repo.path().to_str().unwrap())
                .unwrap(),
        );
        assert!(finalize_pr_with_backend(
            real_git_finalization_options(repo.path(), vec!["a".to_string()]),
            &mut mismatch
        )
        .is_err());
    }

    #[test]
    fn production_validator_normalizes_changed_file_order_and_duplicates() {
        crate::test_support::with_isolated_home(|_| {
            let repo = real_git_repo();
            std::fs::write(repo.path().join("a"), "a").unwrap();
            std::fs::write(repo.path().join("b"), "b").unwrap();
            let candidate = crate::core::agent_task_promotion::candidate_fingerprint(
                repo.path().to_str().unwrap(),
            )
            .unwrap();
            let run_id = "production-validator-8058";
            crate::core::agent_task_lifecycle::submit_plan(
                &crate::core::agent_task_scheduler::AgentTaskPlan::new("validator", Vec::new()),
                Some(run_id),
            )
            .unwrap();
            let mut promotion = successful_gate_proof().promotion;
            promotion.source.run_id = Some(run_id.to_string());
            promotion.target.path = Some(repo.path().display().to_string());
            promotion.provenance = json!({ "candidate": candidate });
            crate::core::agent_task_lifecycle::record_promotion(
                run_id,
                serde_json::to_value(promotion).unwrap(),
            )
            .unwrap();
            let mut options = real_git_finalization_options(
                repo.path(),
                vec!["b".to_string(), "a".to_string(), "a".to_string()],
            );
            options.run_id = run_id.to_string();
            validate_real_candidate_fingerprint(&options).unwrap();
            options.changed_files = vec!["a".to_string()];
            assert!(validate_real_candidate_fingerprint(&options).is_err());
        });
    }

    #[test]
    fn production_validator_accepts_only_the_exact_promoted_recovery_commit() {
        crate::test_support::with_isolated_home(|_| {
            let repo = real_git_repo();
            std::fs::write(repo.path().join("candidate"), "promoted bytes").unwrap();
            let candidate = crate::core::agent_task_promotion::candidate_fingerprint(
                repo.path().to_str().unwrap(),
            )
            .unwrap();
            let run_id = "production-recovery-commit";
            crate::core::agent_task_lifecycle::submit_plan(
                &crate::core::agent_task_scheduler::AgentTaskPlan::new("validator", Vec::new()),
                Some(run_id),
            )
            .unwrap();
            let mut promotion = successful_gate_proof().promotion;
            promotion.source.run_id = Some(run_id.to_string());
            promotion.target.path = Some(repo.path().display().to_string());
            promotion.provenance = json!({ "candidate": candidate });
            crate::core::agent_task_lifecycle::record_promotion(
                run_id,
                serde_json::to_value(promotion).unwrap(),
            )
            .unwrap();

            assert!(Command::new("git")
                .args(["add", "candidate"])
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success());
            assert!(Command::new("git")
                .args(["commit", "-m", "recover promoted candidate"])
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success());
            let mut options =
                real_git_finalization_options(repo.path(), vec!["candidate".to_string()]);
            options.run_id = run_id.to_string();
            validate_real_candidate_fingerprint(&options).expect("exact recovery commit accepted");

            let mut missing = options.clone();
            missing.changed_files.clear();
            let error =
                validate_real_candidate_fingerprint(&missing).expect_err("missing paths rejected");
            assert!(error.message.contains("changed files must exactly match"));
            let mut synthetic = options.clone();
            synthetic.changed_files = vec!["candidate".to_string(), "synthetic".to_string()];
            let error =
                validate_real_candidate_fingerprint(&synthetic).expect_err("extra paths rejected");
            assert!(error.message.contains("changed files must exactly match"));

            std::fs::write(repo.path().join("drift"), "drift").unwrap();
            Command::new("git")
                .args(["add", "drift"])
                .current_dir(repo.path())
                .status()
                .unwrap();
            Command::new("git")
                .args(["commit", "-m", "drift"])
                .current_dir(repo.path())
                .status()
                .unwrap();
            let error = validate_real_candidate_fingerprint(&options).expect_err("drift rejected");
            assert!(error.message.contains("parent and tree exactly match"));
        });
    }

    fn successful_lifecycle(model: &str) -> RunLifecycleRecord {
        RunLifecycleRecord {
            execution: RunExecutionLifecycle {
                state: RunExecutionState::Succeeded,
                started_at: None,
                finished_at: Some("2026-01-01T00:00:00Z".to_string()),
                updated_at: None,
            },
            provider_runtime: vec![ProviderRuntimeLifecycle {
                task_id: "task".to_string(),
                backend: "provider".to_string(),
                state: ProviderRuntimeState::Succeeded,
                stream_uri: None,
                external_runtime_ids: Vec::new(),
                metadata: json!({ "model": model }),
            }],
            ..RunLifecycleRecord::default()
        }
    }

    fn generic_executor_lifecycle(state: ProviderRuntimeState, model: &str) -> RunLifecycleRecord {
        RunLifecycleRecord {
            execution: RunExecutionLifecycle {
                state: RunExecutionState::Succeeded,
                started_at: None,
                finished_at: Some("2026-01-01T00:00:00Z".to_string()),
                updated_at: None,
            },
            provider_runtime: vec![ProviderRuntimeLifecycle {
                task_id: "task".to_string(),
                backend: "opencode".to_string(),
                state,
                stream_uri: None,
                external_runtime_ids: Vec::new(),
                metadata: json!({
                    "evidence_source": "canonical_executor_outcome",
                    "executor": { "backend": "opencode", "model": model },
                    "model": model,
                }),
            }],
            ..RunLifecycleRecord::default()
        }
    }

    fn durable_task(task_id: &str, backend: &str, model: Option<&str>) -> AgentTaskRequest {
        serde_json::from_value(json!({
            "task_id": task_id,
            "executor": { "backend": backend, "model": model },
            "instructions": "run",
        }))
        .expect("durable task")
    }

    fn durable_succeeded_outcome(task_id: &str, metadata: serde_json::Value) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("succeeded".to_string()),
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: format!("{task_id}-patch"),
                kind: "patch".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: Some(format!("/tmp/{task_id}.patch")),
                url: None,
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: serde_json::Value::Null,
            }],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: serde_json::Value::Null,
            workflow: None,
            follow_up: None,
            metadata,
        }
    }

    fn legacy_generic_terminal_run(run_id: &str, timed_out: bool) -> String {
        let plan = AgentTaskPlan::new(
            "legacy-generic-plan",
            vec![durable_task(
                "task",
                "opencode",
                Some("openai/gpt-5.6-terra"),
            )],
        );
        let mut aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                succeeded: 1,
                ..Default::default()
            },
            outcomes: vec![durable_succeeded_outcome("task", serde_json::Value::Null)],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus::default(),
        };
        if timed_out {
            aggregate.status = AgentTaskAggregateStatus::CandidateRecoverable;
            aggregate.totals = AgentTaskAggregateTotals {
                candidate_recoverable: 1,
                ..Default::default()
            };
            aggregate.outcomes[0].status = AgentTaskOutcomeStatus::CandidateRecoverable;
        }
        let record = crate::core::agent_task_lifecycle::record_completed_run(
            &plan,
            &aggregate,
            Some(run_id),
        )
        .expect("terminal record");
        crate::core::agent_task_lifecycle::rewrite_record_for_test(&record.run_id, |record| {
            record.lifecycle.provider_runtime.clear();
        })
        .expect("legacy lifecycle persisted");
        record.run_id
    }

    fn successful_gate_proof() -> AgentTaskPrDurableGateProof {
        AgentTaskPrDurableGateProof {
            run_id: "cook-3678".to_string(),
            promotion: serde_json::from_value(json!({
                "status": "applied", "source": { "kind": "aggregate", "task_id": "task", "run_id": "cook-3678" },
                "to_worktree": "worktree", "target": { "worktree": "worktree", "path": "/repo" },
                "patch_artifact": { "id": "patch", "kind": "patch", "path": "patch" },
                "operator_notification": { "status": "completed", "message": "complete" },
                "gate_results": [{ "id": "gate", "name": "cargo test", "kind": "command", "status": "passed" }]
            })).expect("promotion proof"),
        }
    }

    #[test]
    fn validates_publication_intent_contract() {
        let mut backend = MockBackend {
            changed_files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        };
        let report = finalize_pr_with_backend(options(), &mut backend).expect("finalized");

        validate_publication_intent(&report.publication_intent).expect("intent is valid");

        let mut invalid = report.publication_intent;
        invalid.target.head = Some(String::new());
        let error = validate_publication_intent(&invalid).expect_err("missing head rejected");

        assert!(error.message.contains("target head ref"));
    }

    fn options() -> AgentTaskPrFinalizationOptions {
        let gate_results = vec![AgentTaskGateResult {
            name: "focused project check".to_string(),
            status: "passed".to_string(),
            detail: Some("targeted".to_string()),
        }];
        let normalized_gate_results = gate_results
            .iter()
            .cloned()
            .map(HomeboyGateResult::from)
            .collect();

        AgentTaskPrFinalizationOptions {
            path: "/repo".to_string(),
            run_id: "cook-3678".to_string(),
            base: "main".to_string(),
            head: None,
            title: "Cook issue #3678".to_string(),
            commit_message: "finalize cook loop PR plumbing".to_string(),
            gate_results,
            normalized_gate_results,
            changed_files: Vec::new(),
            evidence: AgentTaskPrEvidence {
                source_refs: vec!["https://github.com/Extra-Chill/homeboy/issues/3678".to_string()],
                artifact_refs: vec!["artifact://aggregate.json".to_string()],
                attempt_summary: "attempt 1 passed deterministic gates".to_string(),
                ai_tool: "OpenCode (GPT-5.5)".to_string(),
                ai_model: Some("GPT-5.5".to_string()),
                source_relationship: AgentTaskPrSourceRelationship::default(),
                verification: AgentTaskPrVerification::default(),
                runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
                lifecycle: None,
            },
            ai_used_for: "Drafted implementation and tests; Chris reviews and owns the change."
                .to_string(),
            review_dossier: AgentTaskReviewDossier {
                schema: "homeboy/agent-task-review-dossier/v1".to_string(),
                summary: "Finalize a verified candidate.".to_string(),
                what_changed: vec!["Updates the finalization contract.".to_string()],
                how_to_test: vec![
                    crate::core::agent_task_review_dossier::AgentTaskReviewTestStep {
                        command: "cargo test agent_task_finalization".to_string(),
                        expected: "passes".to_string(),
                    },
                ],
                compatibility: "No compatibility impact.".to_string(),
                evidence: Vec::new(),
                ai_assistance:
                    crate::core::agent_task_review_dossier::AgentTaskReviewAiAssistance {
                        used: true,
                        tool: "OpenCode (GPT-5.5)".to_string(),
                        model: "GPT-5.5".to_string(),
                        used_for:
                            "Drafted implementation and tests; Chris reviews and owns the change."
                                .to_string(),
                    },
                source_relationships: Vec::new(),
                overrides: Vec::new(),
            },
            review_profile: crate::core::agent_task_review_dossier::default_profile(),
            manual_finalization: true,
            protected_branches: vec![
                "main".to_string(),
                "master".to_string(),
                "trunk".to_string(),
            ],
        }
    }
}

use serde::{Deserialize, Serialize};

use super::*;
use crate::agent_task_review_dossier::{AgentTaskPublicContract, AgentTaskPublicContractEvidence};
use homeboy_core::git::GitIdentityProof;

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
    /// Immutable base snapshot used to verify the candidate before publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_base_sha: Option<String>,
    /// Live base SHA observed immediately before publication, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_base_sha: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_identity: Option<GitIdentityProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_tracking: Option<AgentTaskPublicationGitTracking>,
    pub proof: HomeboyProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskPublicationGitTracking {
    pub local_branch: String,
    pub remote: String,
    pub upstream_ref: String,
    pub verified_remote_sha: String,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_public_contracts: Vec<AgentTaskPublicContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_contract_evidence: Option<AgentTaskPublicContractEvidence>,
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
    /// Immutable commit SHA recorded before the declared verification gates ran.
    pub verified_base_sha: Option<String>,
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
    fn resolve_verified_base(
        &mut self,
        _path: &str,
        verified_base_sha: &str,
    ) -> Result<AgentTaskPrResolvedBase> {
        Ok(AgentTaskPrResolvedBase {
            reference: verified_base_sha.to_string(),
            sha: verified_base_sha.to_string(),
        })
    }
    /// Observes the live base without updating the immutable finalization snapshot.
    fn publication_base_sha(&mut self, _path: &str, _base: &str) -> Result<Option<String>> {
        Ok(None)
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
    /// Validates the effective prospective identity before commit mutation.
    fn validate_publication_identity(&mut self, path: &str) -> Result<GitIdentityProof>;
    /// Validates and reports the immutable identity stored in the candidate commit.
    fn validate_committed_publication_identity(
        &mut self,
        path: &str,
        expected: Option<&GitIdentityProof>,
    ) -> Result<GitIdentityProof>;
    fn commit_all(&mut self, path: &str, message: &str) -> Result<()>;
    /// Pushes the verified commit SHA to the candidate branch.
    fn push_branch(
        &mut self,
        path: &str,
        commit_sha: &str,
        head: &str,
    ) -> Result<AgentTaskPublicationGitTracking>;
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

//! Generic run-level proof envelope for Homeboy workflows.
//!
//! Plans declare intended work and required gates. Proof records the observed
//! evidence from a run: provenance, gate outcomes, artifacts, environment, and
//! explicit coverage gaps.
//!
//! These are pure serde data types plus their builders and pure classification
//! helpers. The proof *validation* behavior (`validate_proof_value`) and the
//! `loop_spec_validation` provider live in `homeboy-core`, because validation
//! reaches into core's `artifact_address`/observation layer.

use serde::{Deserialize, Serialize};

use crate::gate::{HomeboyGateResult, HomeboyGateStatus};

pub const HOMEBOY_PROOF_SCHEMA: &str = "homeboy/proof/v1";
pub const HOMEBOY_PROOF_VALIDATION_SCHEMA: &str = "homeboy/proof-validation/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeboyProof {
    #[serde(default = "proof_schema")]
    pub schema: String,
    pub id: String,
    pub scope: HomeboyProofScope,
    pub provenance: HomeboyProofProvenance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<HomeboyGateResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<HomeboyProofArtifactRef>,
    #[serde(
        default,
        alias = "required_artifacts",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub artifact_requirements: Vec<HomeboyProofArtifactRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<HomeboyProofEnvironmentVariable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<HomeboyProofGap>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofScope {
    Targeted,
    CiEquivalent,
    Mixed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofProvenance {
    pub runner: HomeboyProofRunner,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofRunner {
    Homeboy,
    Manual,
    ExternalCi,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofArtifactRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(
        default = "default_proof_artifact_purpose",
        skip_serializing_if = "is_default_proof_artifact_purpose"
    )]
    pub purpose: HomeboyProofArtifactPurpose,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofArtifactRequirement {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(
        default = "default_proof_artifact_purpose",
        skip_serializing_if = "is_default_proof_artifact_purpose"
    )]
    pub purpose: HomeboyProofArtifactPurpose,
    #[serde(default = "default_required_proof_artifact")]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofArtifactPurpose {
    Proof,
    Diagnostic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofEnvironmentVariable {
    pub name: String,
    pub value: String,
    pub disposition: HomeboyProofEnvironmentDisposition,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofEnvironmentDisposition {
    Inherited,
    Sanitized,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofGap {
    pub kind: HomeboyProofGapKind,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofValidationReport {
    #[serde(default = "proof_validation_schema")]
    pub schema: String,
    #[serde(default = "default_proof_validation_status")]
    pub status: HomeboyProofValidationStatus,
    pub valid: bool,
    pub diagnostics: Vec<HomeboyProofValidationDiagnostic>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofValidationStatus {
    Passed,
    Incomplete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyProofValidationDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HomeboyProofGapKind {
    CiEquivalentNotRecorded,
    ArtifactUnavailable,
    RunnerUnavailable,
    ManualOnly,
    Other,
}

impl HomeboyProof {
    pub fn new(id: impl Into<String>, provenance: HomeboyProofProvenance) -> Self {
        Self {
            schema: HOMEBOY_PROOF_SCHEMA.to_string(),
            id: id.into(),
            scope: HomeboyProofScope::Unknown,
            provenance,
            gates: Vec::new(),
            artifacts: Vec::new(),
            artifact_requirements: Vec::new(),
            environment: Vec::new(),
            gaps: Vec::new(),
        }
    }

    pub fn gates(mut self, gates: impl IntoIterator<Item = HomeboyGateResult>) -> Self {
        self.gates = gates.into_iter().collect();
        self.scope = proof_scope(&self.gates);
        self
    }

    pub fn gates_requiring_ci_equivalent(
        self,
        gates: impl IntoIterator<Item = HomeboyGateResult>,
    ) -> Self {
        self.gates(gates).with_ci_equivalent_gap_if_missing()
    }

    pub fn artifacts(
        mut self,
        artifacts: impl IntoIterator<Item = HomeboyProofArtifactRef>,
    ) -> Self {
        self.artifacts = artifacts.into_iter().collect();
        self
    }

    pub fn artifact_requirements(
        mut self,
        artifact_requirements: impl IntoIterator<Item = HomeboyProofArtifactRequirement>,
    ) -> Self {
        self.artifact_requirements = artifact_requirements.into_iter().collect();
        self
    }

    pub fn environment(
        mut self,
        environment: impl IntoIterator<Item = HomeboyProofEnvironmentVariable>,
    ) -> Self {
        self.environment = environment.into_iter().collect();
        self
    }

    pub fn gaps(mut self, gaps: impl IntoIterator<Item = HomeboyProofGap>) -> Self {
        self.gaps = gaps.into_iter().collect();
        self
    }

    pub fn with_ci_equivalent_gap_if_missing(mut self) -> Self {
        if !self.has_ci_equivalent_gate() {
            self.gaps.push(HomeboyProofGap {
                kind: HomeboyProofGapKind::CiEquivalentNotRecorded,
                summary: "CI-equivalent required gate was not recorded; targeted proof must not be treated as full CI coverage".to_string(),
            });
        }
        self
    }

    pub fn has_ci_equivalent_gate(&self) -> bool {
        self.gates.iter().any(is_ci_equivalent_gate)
    }
}

impl HomeboyProofProvenance {
    pub fn homeboy_run(run_id: impl Into<String>) -> Self {
        Self {
            runner: HomeboyProofRunner::Homeboy,
            run_id: Some(run_id.into()),
            task_id: None,
            source_refs: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn source_refs(mut self, source_refs: impl IntoIterator<Item = String>) -> Self {
        self.source_refs = source_refs.into_iter().collect();
        self
    }

    pub fn notes(mut self, notes: impl IntoIterator<Item = String>) -> Self {
        self.notes = notes.into_iter().collect();
        self
    }
}

impl HomeboyProofArtifactRef {
    pub fn uri(uri: impl Into<String>) -> Self {
        Self {
            id: None,
            uri: uri.into(),
            kind: None,
            label: None,
            semantic_key: None,
            purpose: HomeboyProofArtifactPurpose::Proof,
        }
    }
}

fn default_proof_artifact_purpose() -> HomeboyProofArtifactPurpose {
    HomeboyProofArtifactPurpose::Proof
}

fn is_default_proof_artifact_purpose(purpose: &HomeboyProofArtifactPurpose) -> bool {
    *purpose == HomeboyProofArtifactPurpose::Proof
}

fn default_required_proof_artifact() -> bool {
    true
}

fn default_proof_validation_status() -> HomeboyProofValidationStatus {
    HomeboyProofValidationStatus::Passed
}

fn proof_schema() -> String {
    HOMEBOY_PROOF_SCHEMA.to_string()
}

fn proof_validation_schema() -> String {
    HOMEBOY_PROOF_VALIDATION_SCHEMA.to_string()
}

/// Classify a proof's coverage scope from its recorded gates.
pub fn proof_scope(gates: &[HomeboyGateResult]) -> HomeboyProofScope {
    if gates.is_empty() {
        return HomeboyProofScope::Unknown;
    }

    let ci_equivalent = gates
        .iter()
        .filter(|gate| is_ci_equivalent_gate(gate))
        .count();
    if ci_equivalent == 0 {
        HomeboyProofScope::Targeted
    } else if ci_equivalent == gates.len() {
        HomeboyProofScope::CiEquivalent
    } else {
        HomeboyProofScope::Mixed
    }
}

/// Whether a gate result is marked CI-equivalent in its provenance/evidence.
pub fn is_ci_equivalent_gate(gate: &HomeboyGateResult) -> bool {
    gate.provenance
        .get("ci_equivalent")
        .and_then(|value| value.as_bool())
        .or_else(|| {
            gate.evidence
                .get("ci_equivalent")
                .and_then(|value| value.as_bool())
        })
        .unwrap_or(false)
}

/// Human-readable label for a gate status.
pub fn gate_status_label(status: HomeboyGateStatus) -> &'static str {
    match status {
        HomeboyGateStatus::Passed => "passed",
        HomeboyGateStatus::Failed => "failed",
        HomeboyGateStatus::Skipped => "skipped",
        HomeboyGateStatus::Blocked => "blocked",
    }
}

/// Human-readable coverage-scope label for a gate.
pub fn gate_scope_label(gate: &HomeboyGateResult) -> &'static str {
    if is_ci_equivalent_gate(gate) {
        "CI-equivalent"
    } else {
        "targeted"
    }
}

/// Human-readable label for a proof runner.
pub fn proof_runner_label(runner: HomeboyProofRunner) -> &'static str {
    match runner {
        HomeboyProofRunner::Homeboy => "Homeboy agent-task cook loop",
        HomeboyProofRunner::Manual => "manual",
        HomeboyProofRunner::ExternalCi => "external CI",
        HomeboyProofRunner::Unknown => "unknown",
    }
}

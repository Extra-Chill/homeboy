//! Generic run-level proof envelope for Homeboy workflows.
//!
//! Plans declare intended work and required gates. Proof records the observed
//! evidence from a run: provenance, gate outcomes, artifacts, environment, and
//! explicit coverage gaps.

use serde::{Deserialize, Serialize};

use crate::core::gate::{HomeboyGateResult, HomeboyGateStatus};

pub const HOMEBOY_PROOF_SCHEMA: &str = "homeboy/proof/v1";

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
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
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
            environment: Vec::new(),
            gaps: Vec::new(),
        }
    }

    pub fn gates(mut self, gates: impl IntoIterator<Item = HomeboyGateResult>) -> Self {
        self.gates = gates.into_iter().collect();
        self.scope = proof_scope(&self.gates);
        self
    }

    pub fn artifacts(
        mut self,
        artifacts: impl IntoIterator<Item = HomeboyProofArtifactRef>,
    ) -> Self {
        self.artifacts = artifacts.into_iter().collect();
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
            uri: uri.into(),
            kind: None,
            label: None,
        }
    }
}

fn proof_scope(gates: &[HomeboyGateResult]) -> HomeboyProofScope {
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

pub fn gate_status_label(status: HomeboyGateStatus) -> &'static str {
    match status {
        HomeboyGateStatus::Passed => "passed",
        HomeboyGateStatus::Failed => "failed",
        HomeboyGateStatus::Skipped => "skipped",
        HomeboyGateStatus::Blocked => "blocked",
    }
}

fn proof_schema() -> String {
    HOMEBOY_PROOF_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gate::{HomeboyGateKind, HomeboyGateStatus};

    #[test]
    fn proof_scope_distinguishes_targeted_and_ci_equivalent_gates() {
        let targeted = HomeboyGateResult::new(
            "gate-1",
            "focused check",
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        );
        let ci_equivalent = HomeboyGateResult::new(
            "gate-2",
            "required project gate",
            HomeboyGateKind::Command,
            HomeboyGateStatus::Passed,
        )
        .evidence(serde_json::json!({ "ci_equivalent": true }));

        let proof = HomeboyProof::new("proof-1", HomeboyProofProvenance::homeboy_run("run-1"))
            .gates([targeted, ci_equivalent]);

        assert_eq!(proof.scope, HomeboyProofScope::Mixed);
        assert!(proof.has_ci_equivalent_gate());
    }

    #[test]
    fn proof_records_ci_equivalent_gap_when_missing() {
        let proof = HomeboyProof::new("proof-1", HomeboyProofProvenance::homeboy_run("run-1"))
            .gates([HomeboyGateResult::new(
                "gate-1",
                "focused check",
                HomeboyGateKind::Command,
                HomeboyGateStatus::Passed,
            )])
            .with_ci_equivalent_gap_if_missing();

        assert_eq!(proof.scope, HomeboyProofScope::Targeted);
        assert_eq!(proof.gaps.len(), 1);
        assert_eq!(
            proof.gaps[0].kind,
            HomeboyProofGapKind::CiEquivalentNotRecorded
        );
    }
}

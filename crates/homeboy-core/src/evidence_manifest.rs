//! Portable interpretation contract for a body of evidence.
//!
//! An evidence manifest answers the two questions a raw run record cannot:
//! *what does this evidence mean*, and *what is blocking*. It is deliberately
//! separate from the run record itself so a producer that knows more than the
//! exit code — an extension, an agent, an external orchestrator — can attach a
//! judgement, and so that judgement stays portable when it is lifted out of the
//! report it arrived in.
//!
//! ## Provenance is part of the contract
//!
//! A manifest is either **authored** (a producer attached it to a run, in
//! `metadata.evidence_manifest` or as an artifact of kind `evidence_manifest`)
//! or **derived** (Homeboy composed it from the run record because no producer
//! attached one). Those two are not interchangeable: an authored manifest is an
//! assertion, a derived one is a mechanical reading of status, gate failures,
//! and artifact reviewability. [`EvidenceManifest::source`] records which, and
//! the reader stamps it from the resolution path rather than trusting the
//! serialized value, so a producer cannot claim provenance it does not have.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::artifact_ref::ArtifactRef;

pub const EVIDENCE_MANIFEST_SCHEMA: &str = "homeboy/evidence-manifest/v1";

/// Where a resolved manifest came from.
///
/// Optional on the wire so the field is additive: a manifest written by an
/// older producer simply carries no source, and an older reader ignores it.
/// Readers overwrite it with the truth of the resolution path.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceManifestSource {
    /// A producer attached it to the run's metadata.
    RunMetadata,
    /// A producer attached it as an artifact.
    Artifact,
    /// No producer attached one; Homeboy composed it from the run record.
    Derived,
}

impl EvidenceManifestSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunMetadata => "run_metadata",
            Self::Artifact => "artifact",
            Self::Derived => "derived",
        }
    }

    /// Whether a producer asserted this interpretation.
    ///
    /// Consumers that will act on a manifest without a human in the loop should
    /// gate on this: a derived manifest is Homeboy reading its own run record,
    /// not an independent judgement.
    pub fn is_authored(self) -> bool {
        matches!(self, Self::RunMetadata | Self::Artifact)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceManifest {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Resolution provenance. Absent on a manifest that has not been resolved
    /// through a reader yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<EvidenceManifestSource>,
    pub status: EvidenceManifestStatus,
    pub interpretation: EvidenceManifestInterpretation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracker_refs: Vec<TrackerRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pr_refs: Vec<PullRequestRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_refs: Vec<RunRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocking_conditions: Vec<BlockingCondition>,
}

impl EvidenceManifest {
    /// Start a manifest with the two members the contract requires.
    ///
    /// Every other member is optional, so this is the only constructor a
    /// producer needs; fill the rest by assignment.
    pub fn new(state: EvidenceManifestState, summary: impl Into<String>) -> Self {
        Self {
            schema: EVIDENCE_MANIFEST_SCHEMA.to_string(),
            id: None,
            title: None,
            source: None,
            status: EvidenceManifestStatus {
                state,
                label: None,
                updated_at: None,
            },
            interpretation: EvidenceManifestInterpretation {
                summary: summary.into(),
                confidence: None,
                notes: Vec::new(),
            },
            tracker_refs: Vec::new(),
            pr_refs: Vec::new(),
            run_refs: Vec::new(),
            artifact_refs: Vec::new(),
            blocking_conditions: Vec::new(),
        }
    }

    pub fn parse_value(value: Value) -> Result<Self, String> {
        let manifest: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reject a manifest that parses but says nothing.
    ///
    /// The point of this contract is that a consumer can act on it without
    /// re-deriving anything. A blank summary, an identifier-less tracker or run
    /// reference, or a blocking condition with no kind are all shapes that
    /// satisfy serde and then force the consumer back to the raw run — which is
    /// worse than no manifest, because the field's presence claims otherwise.
    /// Failing closed here surfaces the producer bug in
    /// `evidence_manifest_errors` instead of publishing an empty judgement.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != EVIDENCE_MANIFEST_SCHEMA {
            return Err(format!(
                "evidence manifest schema must be {EVIDENCE_MANIFEST_SCHEMA}"
            ));
        }
        if self.interpretation.summary.trim().is_empty() {
            return Err("evidence manifest interpretation.summary must not be empty".to_string());
        }
        for (index, tracker) in self.tracker_refs.iter().enumerate() {
            if tracker.kind.trim().is_empty() || tracker.id.trim().is_empty() {
                return Err(format!(
                    "evidence manifest tracker_refs[{index}] requires a non-empty kind and id"
                ));
            }
        }
        for (index, pr) in self.pr_refs.iter().enumerate() {
            if pr.repo.trim().is_empty() || pr.number == 0 {
                return Err(format!(
                    "evidence manifest pr_refs[{index}] requires a non-empty repo and a nonzero number"
                ));
            }
        }
        for (index, run) in self.run_refs.iter().enumerate() {
            if run.id.trim().is_empty() {
                return Err(format!(
                    "evidence manifest run_refs[{index}] requires a non-empty id"
                ));
            }
        }
        for (index, condition) in self.blocking_conditions.iter().enumerate() {
            if condition.kind.trim().is_empty() || condition.summary.trim().is_empty() {
                return Err(format!(
                    "evidence manifest blocking_conditions[{index}] requires a non-empty kind and summary"
                ));
            }
        }
        Ok(())
    }

    /// Whether the manifest reports at least one blocker of the given severity.
    pub fn has_blocking_condition(&self, severity: BlockingSeverity) -> bool {
        self.blocking_conditions
            .iter()
            .any(|condition| condition.severity == Some(severity))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceManifestStatus {
    pub state: EvidenceManifestState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceManifestState {
    Pending,
    Passed,
    Failed,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceManifestInterpretation {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<EvidenceConfidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackerRef {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestRef {
    pub repo: String,
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockingCondition {
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<BlockingSeverity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockingSeverity {
    Info,
    Warning,
    Critical,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn evidence_manifest_parses_portable_refs_status_and_blockers() {
        let manifest = EvidenceManifest::parse_value(json!({
            "schema": "homeboy/evidence-manifest/v1",
            "id": "manifest-1",
            "title": "Site editor preload proof",
            "status": {
                "state": "blocked",
                "label": "Needs maintainer decision",
                "updated_at": "2026-06-17T00:00:00Z"
            },
            "interpretation": {
                "summary": "Candidate reduces REST preloads but one scenario regressed.",
                "confidence": "medium",
                "notes": ["Review full matrix before merge."]
            },
            "tracker_refs": [{
                "kind": "github_issue",
                "id": "Extra-Chill/homeboy#123",
                "url": "https://github.com/Extra-Chill/homeboy/issues/123",
                "state": "open"
            }],
            "pr_refs": [{
                "repo": "Extra-Chill/homeboy",
                "number": 456,
                "url": "https://github.com/Extra-Chill/homeboy/pull/456",
                "head_ref": "feature/proof",
                "base_ref": "main"
            }],
            "run_refs": [{
                "id": "run-1",
                "kind": "bench",
                "component_id": "homeboy",
                "rig_id": "studio"
            }],
            "artifact_refs": [{
                "schema": "homeboy/artifact-ref/v1",
                "id": "artifact-1",
                "run_id": "run-1",
                "kind": "summary",
                "type": "file",
                "path": "summary.json"
            }],
            "blocking_conditions": [{
                "kind": "coverage_gap",
                "summary": "Missing mobile scenario.",
                "severity": "warning",
                "refs": ["run-1"]
            }]
        }))
        .expect("manifest");

        assert_eq!(manifest.schema, EVIDENCE_MANIFEST_SCHEMA);
        assert_eq!(manifest.status.state, EvidenceManifestState::Blocked);
        assert_eq!(
            manifest.interpretation.confidence,
            Some(EvidenceConfidence::Medium)
        );
        assert_eq!(manifest.tracker_refs[0].id, "Extra-Chill/homeboy#123");
        assert_eq!(manifest.pr_refs[0].number, 456);
        assert_eq!(manifest.run_refs[0].id, "run-1");
        assert_eq!(manifest.artifact_refs[0].artifact_type, "file");
        assert_eq!(
            manifest.blocking_conditions[0].severity,
            Some(BlockingSeverity::Warning)
        );
    }

    #[test]
    fn evidence_manifest_serializes_without_empty_optional_collections() {
        let mut manifest = EvidenceManifest::new(
            EvidenceManifestState::Passed,
            "Evidence supports merge.".to_string(),
        );
        manifest.interpretation.confidence = Some(EvidenceConfidence::High);

        assert_eq!(
            serde_json::to_value(&manifest).expect("manifest json"),
            json!({
                "schema": "homeboy/evidence-manifest/v1",
                "status": { "state": "passed" },
                "interpretation": {
                    "summary": "Evidence supports merge.",
                    "confidence": "high"
                }
            })
        );
    }

    /// The provenance member is additive: a manifest that never went through a
    /// reader serializes exactly as it did before the member existed, so an
    /// already-published manifest stays byte-identical.
    #[test]
    fn evidence_manifest_source_is_absent_until_a_reader_stamps_it() {
        let manifest = EvidenceManifest::new(EvidenceManifestState::Passed, "Fine.");

        assert!(manifest.source.is_none());
        let value = serde_json::to_value(&manifest).expect("manifest json");
        assert!(value.get("source").is_none());
    }

    #[test]
    fn evidence_manifest_round_trips_every_source_value() {
        for (source, label) in [
            (EvidenceManifestSource::RunMetadata, "run_metadata"),
            (EvidenceManifestSource::Artifact, "artifact"),
            (EvidenceManifestSource::Derived, "derived"),
        ] {
            let mut manifest = EvidenceManifest::new(EvidenceManifestState::Passed, "Fine.");
            manifest.source = Some(source);

            let value = serde_json::to_value(&manifest).expect("manifest json");
            assert_eq!(value["source"], json!(label));
            assert_eq!(source.as_str(), label);

            let parsed = EvidenceManifest::parse_value(value).expect("manifest");
            assert_eq!(parsed.source, Some(source));
        }

        assert!(EvidenceManifestSource::RunMetadata.is_authored());
        assert!(EvidenceManifestSource::Artifact.is_authored());
        assert!(!EvidenceManifestSource::Derived.is_authored());
    }

    /// An unknown member must stay ignorable: evidence manifests are written by
    /// producers that ship on their own cadence, so a manifest from a newer
    /// producer has to remain readable by an older reader.
    #[test]
    fn evidence_manifest_ignores_unknown_members() {
        let manifest = EvidenceManifest::parse_value(json!({
            "schema": "homeboy/evidence-manifest/v1",
            "status": { "state": "passed" },
            "interpretation": { "summary": "Fine." },
            "member_from_a_newer_producer": { "anything": true }
        }))
        .expect("manifest");

        assert_eq!(manifest.status.state, EvidenceManifestState::Passed);
    }

    #[test]
    fn evidence_manifest_rejects_unknown_schema() {
        let err = EvidenceManifest::parse_value(json!({
            "schema": "example/manifest/v1",
            "status": { "state": "unknown" },
            "interpretation": { "summary": "Unknown schema." }
        }))
        .expect_err("schema error");

        assert!(err.contains(EVIDENCE_MANIFEST_SCHEMA));
    }

    #[test]
    fn evidence_manifest_rejects_a_blank_interpretation() {
        let err = EvidenceManifest::parse_value(json!({
            "schema": "homeboy/evidence-manifest/v1",
            "status": { "state": "passed" },
            "interpretation": { "summary": "   " }
        }))
        .expect_err("summary error");

        assert!(err.contains("interpretation.summary"), "{err}");
    }

    #[test]
    fn evidence_manifest_rejects_references_without_identifiers() {
        let cases = [
            ("tracker_refs", json!([{ "kind": "", "id": "HB-1" }])),
            ("tracker_refs", json!([{ "kind": "issue", "id": " " }])),
            ("run_refs", json!([{ "id": "" }])),
        ];

        for (member, refs) in cases {
            let mut value = json!({
                "schema": "homeboy/evidence-manifest/v1",
                "status": { "state": "passed" },
                "interpretation": { "summary": "Fine." }
            });
            value
                .as_object_mut()
                .expect("object")
                .insert(member.to_string(), refs);

            let err = EvidenceManifest::parse_value(value).expect_err("reference error");

            assert!(err.contains(member), "{err}");
        }
    }

    #[test]
    fn evidence_manifest_rejects_a_pull_request_reference_without_a_repo_or_number() {
        let err = EvidenceManifest::parse_value(json!({
            "schema": "homeboy/evidence-manifest/v1",
            "status": { "state": "passed" },
            "interpretation": { "summary": "Fine." },
            "pr_refs": [{ "repo": "", "number": 1 }]
        }))
        .expect_err("pr error");
        assert!(err.contains("pr_refs[0]"), "{err}");

        let err = EvidenceManifest::parse_value(json!({
            "schema": "homeboy/evidence-manifest/v1",
            "status": { "state": "passed" },
            "interpretation": { "summary": "Fine." },
            "pr_refs": [{ "repo": "owner/repo", "number": 0 }]
        }))
        .expect_err("pr error");
        assert!(err.contains("pr_refs[0]"), "{err}");
    }

    #[test]
    fn evidence_manifest_rejects_a_blocking_condition_without_a_kind_or_summary() {
        let err = EvidenceManifest::parse_value(json!({
            "schema": "homeboy/evidence-manifest/v1",
            "status": { "state": "failed" },
            "interpretation": { "summary": "Failed." },
            "blocking_conditions": [{ "kind": "gate_failure", "summary": "" }]
        }))
        .expect_err("blocking condition error");

        assert!(err.contains("blocking_conditions[0]"), "{err}");
    }

    #[test]
    fn evidence_manifest_reports_blocking_condition_severity() {
        let mut manifest = EvidenceManifest::new(EvidenceManifestState::Failed, "Failed.");
        manifest.blocking_conditions.push(BlockingCondition {
            kind: "gate_failure".to_string(),
            summary: "p95_ms exceeded".to_string(),
            severity: Some(BlockingSeverity::Critical),
            refs: Vec::new(),
        });

        assert!(manifest.has_blocking_condition(BlockingSeverity::Critical));
        assert!(!manifest.has_blocking_condition(BlockingSeverity::Warning));
    }
}

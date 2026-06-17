use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_ref::ArtifactRef;

pub const EVIDENCE_MANIFEST_SCHEMA: &str = "homeboy/evidence-manifest/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceManifest {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
    pub fn parse_value(value: Value) -> Result<Self, String> {
        let manifest: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        if manifest.schema != EVIDENCE_MANIFEST_SCHEMA {
            return Err(format!(
                "evidence manifest schema must be {EVIDENCE_MANIFEST_SCHEMA}"
            ));
        }
        Ok(manifest)
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
        let manifest = EvidenceManifest {
            schema: EVIDENCE_MANIFEST_SCHEMA.to_string(),
            id: None,
            title: None,
            status: EvidenceManifestStatus {
                state: EvidenceManifestState::Passed,
                label: None,
                updated_at: None,
            },
            interpretation: EvidenceManifestInterpretation {
                summary: "Evidence supports merge.".to_string(),
                confidence: Some(EvidenceConfidence::High),
                notes: Vec::new(),
            },
            tracker_refs: Vec::new(),
            pr_refs: Vec::new(),
            run_refs: Vec::new(),
            artifact_refs: Vec::new(),
            blocking_conditions: Vec::new(),
        };

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
}

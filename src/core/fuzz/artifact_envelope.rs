use std::path::Path;

use serde::Serialize;

use crate::core::observation::ArtifactRecord;

use super::envelope::FuzzResultEnvelope;
use super::schemas::{
    FUZZ_RESULT_ENVELOPE_SCHEMA, FUZZ_SEQUENCE_PLAN_SCHEMA, FUZZ_SEQUENCE_RESULT_SCHEMA,
};
use super::{FuzzSequencePlan, FuzzSequenceResult};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FuzzResultEnvelopeArtifactInspection {
    pub artifact_id: String,
    pub artifact_kind: String,
    pub artifact_path: String,
    pub recognized_by: Vec<String>,
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<FuzzResultEnvelopeArtifactSummary>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FuzzResultEnvelopeArtifactSummary {
    pub schema: String,
    pub envelope_id: String,
    pub status: String,
    pub gate_status: String,
    pub campaign_id: String,
    pub gate_count: usize,
    pub sequence_count_source: String,
    pub sequence_case_count: usize,
    pub sequence_step_count: usize,
    pub required_artifact_count: usize,
    pub artifact_ref_count: usize,
}

pub fn inspect_fuzz_result_envelope_artifact(
    artifact: &ArtifactRecord,
) -> Option<FuzzResultEnvelopeArtifactInspection> {
    let mut recognized_by = recognition_reasons(artifact);
    if artifact.artifact_type != "file" || !Path::new(&artifact.path).is_file() {
        return (!recognized_by.is_empty()).then(|| FuzzResultEnvelopeArtifactInspection {
            artifact_id: artifact.id.clone(),
            artifact_kind: artifact.kind.clone(),
            artifact_path: artifact.path.clone(),
            recognized_by,
            valid: false,
            errors: vec!["artifact bytes are not available locally".to_string()],
            summary: None,
        });
    }

    let bytes = std::fs::read(&artifact.path).ok()?;
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return (!recognized_by.is_empty()).then(|| FuzzResultEnvelopeArtifactInspection {
            artifact_id: artifact.id.clone(),
            artifact_kind: artifact.kind.clone(),
            artifact_path: artifact.path.clone(),
            recognized_by,
            valid: false,
            errors: vec!["artifact file is not valid JSON".to_string()],
            summary: None,
        });
    };

    if value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|schema| schema == FUZZ_RESULT_ENVELOPE_SCHEMA)
    {
        recognized_by.push("content.schema".to_string());
    }
    if recognized_by.is_empty() {
        return None;
    }

    let envelope = serde_json::from_value::<FuzzResultEnvelope>(value);
    let (summary, errors) = match envelope {
        Ok(envelope) => summarize_validated_envelope(&envelope),
        Err(error) => (None, vec![format!("invalid envelope JSON: {error}")]),
    };
    Some(FuzzResultEnvelopeArtifactInspection {
        artifact_id: artifact.id.clone(),
        artifact_kind: artifact.kind.clone(),
        artifact_path: artifact.path.clone(),
        recognized_by,
        valid: errors.is_empty() && summary.is_some(),
        errors,
        summary,
    })
}

fn recognition_reasons(artifact: &ArtifactRecord) -> Vec<String> {
    let mut reasons = Vec::new();
    if artifact.kind == "fuzz_result_envelope" {
        reasons.push("artifact.kind".to_string());
    }
    if artifact
        .metadata_json
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|schema| schema == FUZZ_RESULT_ENVELOPE_SCHEMA)
    {
        reasons.push("metadata.schema".to_string());
    }
    let file_name = Path::new(&artifact.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if file_name.contains("fuzz-result-envelope") {
        reasons.push("path".to_string());
    }
    reasons
}

fn summarize_validated_envelope(
    envelope: &FuzzResultEnvelope,
) -> (Option<FuzzResultEnvelopeArtifactSummary>, Vec<String>) {
    let mut errors = Vec::new();
    if envelope.schema != FUZZ_RESULT_ENVELOPE_SCHEMA {
        errors.push(format!(
            "schema must be {FUZZ_RESULT_ENVELOPE_SCHEMA}, got {}",
            envelope.schema
        ));
    }
    if envelope.id.trim().is_empty() {
        errors.push("envelope id is required".to_string());
    }
    if envelope.status.trim().is_empty() {
        errors.push("envelope status is required".to_string());
    }
    let campaign_id = match envelope.campaign.as_ref() {
        Some(campaign) if !campaign.id.trim().is_empty() => campaign.id.clone(),
        Some(_) => {
            errors.push("campaign id is required".to_string());
            String::new()
        }
        None => {
            errors.push("campaign is required".to_string());
            String::new()
        }
    };
    for gate in &envelope.gates {
        if gate.id.trim().is_empty() {
            errors.push("gate id is required".to_string());
        }
        if gate.metric.trim().is_empty() {
            errors.push(format!("gate {} metric is required", gate.id));
        }
        if !gate.value.is_finite() {
            errors.push(format!("gate {} value must be finite", gate.id));
        }
    }
    for required in &envelope.required_artifacts {
        if required.id.trim().is_empty() {
            errors.push("required artifact id is required".to_string());
        }
        if required.kind.trim().is_empty() {
            errors.push(format!(
                "required artifact {} kind is required",
                required.id
            ));
        }
    }
    validate_artifact_refs("envelope", &envelope.artifacts, &mut errors);
    if let Some(campaign) = envelope.campaign.as_ref() {
        validate_artifact_refs("campaign", &campaign.artifacts, &mut errors);
    }

    let artifact_ref_count = envelope.artifacts.len()
        + envelope
            .campaign
            .as_ref()
            .map(|campaign| campaign.artifacts.len())
            .unwrap_or(0);
    let sequence_counts = sequence_counts(envelope);
    let summary = FuzzResultEnvelopeArtifactSummary {
        schema: envelope.schema.clone(),
        envelope_id: envelope.id.clone(),
        status: envelope.status.clone(),
        gate_status: envelope.status.clone(),
        campaign_id,
        gate_count: envelope.gates.len(),
        sequence_count_source: sequence_counts.source,
        sequence_case_count: sequence_counts.cases,
        sequence_step_count: sequence_counts.steps,
        required_artifact_count: envelope.required_artifacts.len(),
        artifact_ref_count,
    };
    (Some(summary), errors)
}

fn sequence_counts(envelope: &FuzzResultEnvelope) -> SequenceCounts {
    let mut counts = SequenceCounts::default();
    collect_sequence_counts(&envelope.metadata, &mut counts);
    collect_sequence_counts(&envelope.request.metadata, &mut counts);
    if let Some(campaign) = envelope.campaign.as_ref() {
        collect_sequence_counts(&campaign.metadata, &mut counts);
        for case in &campaign.cases {
            collect_sequence_counts(&case.metadata, &mut counts);
            collect_sequence_counts(&case.observed, &mut counts);
        }
    }
    counts
}

struct SequenceCounts {
    cases: usize,
    steps: usize,
    source: String,
}

impl Default for SequenceCounts {
    fn default() -> Self {
        Self {
            cases: 0,
            steps: 0,
            source: "none".to_string(),
        }
    }
}

fn collect_sequence_counts(value: &serde_json::Value, counts: &mut SequenceCounts) {
    if value.is_null() {
        return;
    }
    for key in ["sequence", "sequence_plan", "sequence_result"] {
        if let Some(sequence) = value.get(key) {
            collect_sequence_counts_from_node(sequence, counts);
        }
    }
    collect_sequence_counts_from_node(value, counts);
}

fn collect_sequence_counts_from_node(value: &serde_json::Value, counts: &mut SequenceCounts) {
    if let Some(schema) = value.get("schema").and_then(serde_json::Value::as_str) {
        match schema {
            FUZZ_SEQUENCE_PLAN_SCHEMA => {
                if let Ok(plan) = FuzzSequencePlan::from_value(value.clone()) {
                    counts.add_schema_backed(&plan.cases);
                    return;
                }
            }
            FUZZ_SEQUENCE_RESULT_SCHEMA => {
                if let Ok(result) = FuzzSequenceResult::from_value(value.clone()) {
                    counts.add_schema_backed(&result.cases);
                    return;
                }
            }
            _ => {}
        }
    }
    if let Some(cases) = value
        .get("cases")
        .or_else(|| value.get("sequence_cases"))
        .and_then(serde_json::Value::as_array)
    {
        counts.mark_best_effort();
        counts.cases += cases.len();
        for case in cases {
            counts.steps += case
                .get("steps")
                .or_else(|| case.get("sequence_steps"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
        }
    }
    if let Some(steps) = value
        .get("steps")
        .or_else(|| value.get("sequence_steps"))
        .and_then(serde_json::Value::as_array)
    {
        counts.mark_best_effort();
        counts.steps += steps.len();
    }
}

impl SequenceCounts {
    fn add_schema_backed(&mut self, cases: &[super::FuzzSequenceCase]) {
        self.source = "schema_backed".to_string();
        self.cases += cases.len();
        self.steps += cases.iter().map(|case| case.steps.len()).sum::<usize>();
    }

    fn mark_best_effort(&mut self) {
        if self.source == "none" {
            self.source = "best_effort_metadata".to_string();
        }
    }
}

fn validate_artifact_refs(
    label: &str,
    artifacts: &[super::coverage::FuzzArtifact],
    errors: &mut Vec<String>,
) {
    for artifact in artifacts {
        if artifact.id.trim().is_empty() {
            errors.push(format!("{label} artifact id is required"));
        }
        if artifact.kind.trim().is_empty() {
            errors.push(format!("{label} artifact {} kind is required", artifact.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::observation::ArtifactRecord;

    use super::*;

    fn artifact(path: &Path) -> ArtifactRecord {
        ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "runner-output".to_string(),
            artifact_type: "file".to_string(),
            path: path.display().to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: Some("application/json".to_string()),
            metadata_json: serde_json::json!({}),
            created_at: "2026-06-26T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn recognizes_canonical_envelope_from_content_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runner-output.json");
        std::fs::write(
            &path,
            r#"{
                "schema":"homeboy/fuzz-result-envelope/v1",
                "version":1,
                "id":"envelope-1",
                "status":"passed",
                "request":{"id":"request-1","component":"homeboy"},
                "campaign":{"id":"campaign-1","safety_class":"read_only"},
                "artifacts":[{"id":"case-log","kind":"case_log"}],
                "required_artifacts":[{"id":"case-log","kind":"case_log","required":true}],
                "gates":[{"id":"open-findings","kind":"threshold","metric":"open_findings","operator":"equal","value":0}]
            }"#,
        )
        .expect("write fixture");

        let inspection =
            inspect_fuzz_result_envelope_artifact(&artifact(&path)).expect("recognized");

        assert!(inspection.valid);
        assert!(inspection
            .recognized_by
            .contains(&"content.schema".to_string()));
        let summary = inspection.summary.expect("summary");
        assert_eq!(summary.campaign_id, "campaign-1");
        assert_eq!(summary.gate_status, "passed");
        assert_eq!(summary.gate_count, 1);
        assert_eq!(summary.required_artifact_count, 1);
        assert_eq!(summary.artifact_ref_count, 1);
    }

    #[test]
    fn validates_required_campaign_and_artifact_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fuzz-result-envelope.json");
        std::fs::write(
            &path,
            r#"{
                "schema":"homeboy/fuzz-result-envelope/v1",
                "id":"envelope-1",
                "status":"failed",
                "request":{"id":"request-1","component":"homeboy"},
                "artifacts":[{"id":"","kind":""}],
                "gates":[{"id":"","kind":"threshold","metric":"","operator":"equal","value":0}]
            }"#,
        )
        .expect("write fixture");

        let inspection =
            inspect_fuzz_result_envelope_artifact(&artifact(&path)).expect("recognized");

        assert!(!inspection.valid);
        assert!(inspection
            .errors
            .iter()
            .any(|error| error == "campaign is required"));
        assert!(inspection
            .errors
            .iter()
            .any(|error| error == "envelope artifact id is required"));
        assert!(inspection
            .errors
            .iter()
            .any(|error| error == "gate id is required"));
    }

    #[test]
    fn sequence_summary_prefers_schema_backed_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runner-output.json");
        std::fs::write(
            &path,
            r#"{
                "schema":"homeboy/fuzz-result-envelope/v1",
                "version":1,
                "id":"envelope-1",
                "status":"passed",
                "request":{"id":"request-1","component":"homeboy"},
                "campaign":{
                    "id":"campaign-1",
                    "safety_class":"read_only",
                    "metadata":{
                        "sequence_plan":{
                            "schema":"homeboy/fuzz-sequence-plan/v1",
                            "version":1,
                            "id":"sequence-plan-1",
                            "cases":[{"id":"case-1","steps":[{"id":"step-1","kind":"prepare"}]}]
                        }
                    }
                }
            }"#,
        )
        .expect("write fixture");

        let inspection =
            inspect_fuzz_result_envelope_artifact(&artifact(&path)).expect("recognized");
        let summary = inspection.summary.expect("summary");

        assert_eq!(summary.sequence_count_source, "schema_backed");
        assert_eq!(summary.sequence_case_count, 1);
        assert_eq!(summary.sequence_step_count, 1);
    }
}

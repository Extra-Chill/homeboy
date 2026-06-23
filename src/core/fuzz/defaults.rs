//! Product-neutral default required-artifact and gate definitions.

use super::coverage::FuzzThresholdOperator;
use super::envelope::{FuzzGate, FuzzRequiredArtifact};
use super::schemas::{FUZZ_GATE_SCHEMA, FUZZ_REQUIRED_ARTIFACT_SCHEMA};

pub fn default_fuzz_required_artifacts() -> Vec<FuzzRequiredArtifact> {
    vec![
        FuzzRequiredArtifact {
            schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            id: "result-envelope".to_string(),
            kind: "result_envelope".to_string(),
            required: true,
            description: Some(
                "Homeboy fuzz result envelope for the campaign or planned execution".to_string(),
            ),
            acceptable_artifact_kinds: vec!["json".to_string()],
        },
        FuzzRequiredArtifact {
            schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            id: "case-log".to_string(),
            kind: "case_log".to_string(),
            required: true,
            description: Some(
                "Case-level execution log or equivalent replayable trace".to_string(),
            ),
            acceptable_artifact_kinds: vec!["jsonl".to_string(), "text".to_string()],
        },
        FuzzRequiredArtifact {
            schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            id: "replay-data".to_string(),
            kind: "replay_data".to_string(),
            required: true,
            description: Some(
                "Inputs, seed, or fixture data required to replay a failing case".to_string(),
            ),
            acceptable_artifact_kinds: vec!["json".to_string(), "artifact".to_string()],
        },
        FuzzRequiredArtifact {
            schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
            id: "coverage-summary".to_string(),
            kind: "coverage_summary".to_string(),
            required: true,
            description: Some(
                "Target and operation coverage summary with declared, executable, and proven counts"
                    .to_string(),
            ),
            acceptable_artifact_kinds: vec!["json".to_string()],
        },
    ]
}

pub fn default_fuzz_gates() -> Vec<FuzzGate> {
    vec![
        FuzzGate {
            schema: FUZZ_GATE_SCHEMA.to_string(),
            id: "no-open-findings".to_string(),
            kind: "finding_count".to_string(),
            metric: "open_findings".to_string(),
            operator: FuzzThresholdOperator::Equal,
            value: 0.0,
            unit: Some("count".to_string()),
            description: Some("Result envelope has no open findings".to_string()),
        },
        FuzzGate {
            schema: FUZZ_GATE_SCHEMA.to_string(),
            id: "has-case-evidence".to_string(),
            kind: "artifact_presence".to_string(),
            metric: "case_log_artifacts".to_string(),
            operator: FuzzThresholdOperator::GreaterThanOrEqual,
            value: 1.0,
            unit: Some("count".to_string()),
            description: Some(
                "Result envelope links at least one case-level proof artifact".to_string(),
            ),
        },
        FuzzGate {
            schema: FUZZ_GATE_SCHEMA.to_string(),
            id: "target-coverage-complete".to_string(),
            kind: "coverage_completeness".to_string(),
            metric: "target_coverage_ratio".to_string(),
            operator: FuzzThresholdOperator::GreaterThanOrEqual,
            value: 1.0,
            unit: Some("ratio".to_string()),
            description: Some(
                "Coverage summary proves every declared target, or explicitly declares zero targets"
                    .to_string(),
            ),
        },
        FuzzGate {
            schema: FUZZ_GATE_SCHEMA.to_string(),
            id: "operation-coverage-complete".to_string(),
            kind: "coverage_completeness".to_string(),
            metric: "operation_coverage_ratio".to_string(),
            operator: FuzzThresholdOperator::GreaterThanOrEqual,
            value: 1.0,
            unit: Some("ratio".to_string()),
            description: Some(
                "Coverage summary proves every declared operation, or explicitly declares zero operations"
                    .to_string(),
            ),
        },
    ]
}

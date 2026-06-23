use std::collections::BTreeMap;

use serde_json::Value;

use crate::core::fuzz::*;
use crate::core::lifecycle::{LifecycleContract, LifecycleResultMetadata};

#[test]
fn campaign_serializes_seeds_coverage_findings_artifacts_thresholds_and_provenance() {
    let campaign = FuzzCampaign {
        schema: FUZZ_CAMPAIGN_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: "campaign-1".to_string(),
        title: Some("generic campaign".to_string()),
        safety_class: FuzzSafetyClass::ReadOnly,
        surfaces: Vec::new(),
        targets: vec![FuzzTarget {
            schema: FUZZ_TARGET_SCHEMA.to_string(),
            id: "target-1".to_string(),
            kind: "api".to_string(),
            label: None,
            locator: Some("https://example.test/orders".to_string()),
            operations: vec![FuzzOperation {
                id: "operation-1".to_string(),
                kind: "read".to_string(),
                family: Some(FuzzOperationFamily::Read),
                target_id: Some("target-1".to_string()),
                label: None,
                tags: Vec::new(),
            }],
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }],
        workloads: vec![FuzzWorkload {
            schema: FUZZ_WORKLOAD_SCHEMA.to_string(),
            id: "workload-1".to_string(),
            label: None,
            safety_class: FuzzSafetyClass::ReadOnly,
            surface_ids: vec!["surface-1".to_string()],
            operations: vec!["read".to_string()],
            seed_ids: vec!["seed-1".to_string()],
            case_budget: Some(100),
            duration_budget_seconds: Some(60),
            thresholds: Vec::new(),
            lifecycle: Some(LifecycleContract {
                schema: crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA.to_string(),
                version: crate::core::lifecycle::LIFECYCLE_CONTRACT_VERSION,
                phases: vec![crate::core::lifecycle::LifecyclePhaseContract {
                    id: "snapshot".to_string(),
                    phase: crate::core::lifecycle::LifecyclePhaseKind::Snapshot,
                    label: None,
                    extension_hook: Some("runtime.snapshot".to_string()),
                    command: None,
                    timeout_seconds: None,
                    required: Some(true),
                }],
                metadata: BTreeMap::new(),
            }),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }],
        cases: vec![FuzzCase {
            schema: FUZZ_CASE_SCHEMA.to_string(),
            id: "case-1".to_string(),
            target_id: Some("target-1".to_string()),
            operation_id: Some("operation-1".to_string()),
            workload_id: Some("workload-1".to_string()),
            seed_id: Some("seed-1".to_string()),
            replay_id: Some("replay-1".to_string()),
            input: serde_json::json!({ "path": "/orders" }),
            expected: Value::Null,
            observed: Value::Null,
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }],
        seeds: vec![FuzzSeed {
            schema: FUZZ_SEED_SCHEMA.to_string(),
            id: "seed-1".to_string(),
            kind: "literal".to_string(),
            label: None,
            value: Some("sample".to_string()),
            artifact: None,
            tags: Vec::new(),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }],
        coverage: vec![FuzzCoverage {
            schema: FUZZ_COVERAGE_SCHEMA.to_string(),
            id: "coverage-1".to_string(),
            metric: "operation".to_string(),
            covered: 8,
            total: 10,
            ratio: Some(0.8),
            gaps: vec![FuzzCoverageGap {
                id: "gap-1".to_string(),
                label: None,
                surface_id: Some("surface-1".to_string()),
                target_id: Some("target-1".to_string()),
                operation: Some("write".to_string()),
                operation_id: Some("operation-1".to_string()),
            }],
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }],
        coverage_summary: Some(FuzzCoverageSummary {
            schema: FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
            declared_targets: 1,
            executable_targets: 1,
            proven_targets: 1,
            declared_operations: 1,
            executable_operations: 1,
            proven_operations: 1,
            skipped_targets: Vec::new(),
            skipped_operations: Vec::new(),
            surface_summaries: vec![FuzzCoverageGroupSummary {
                id: "surface-1".to_string(),
                kind: "api".to_string(),
                label: Some("API".to_string()),
                declared_targets: 1,
                executable_targets: 1,
                proven_targets: 1,
                declared_operations: 1,
                executable_operations: 1,
                proven_operations: 1,
                skipped_targets: Vec::new(),
                skipped_operations: Vec::new(),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            kind_summaries: vec![FuzzCoverageGroupSummary {
                id: "read".to_string(),
                kind: "operation_kind".to_string(),
                label: None,
                declared_targets: 1,
                executable_targets: 1,
                proven_targets: 1,
                declared_operations: 1,
                executable_operations: 1,
                proven_operations: 1,
                skipped_targets: Vec::new(),
                skipped_operations: Vec::new(),
                metadata: Value::Null,
                extra: BTreeMap::new(),
            }],
            artifact_ids: vec!["artifact-1".to_string()],
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }),
        findings: vec![FuzzFinding {
            schema: FUZZ_FINDING_SCHEMA.to_string(),
            id: "finding-1".to_string(),
            title: "unexpected response".to_string(),
            severity: "high".to_string(),
            status: FuzzFindingStatus::Open,
            surface_id: Some("surface-1".to_string()),
            target_id: Some("target-1".to_string()),
            operation_id: Some("operation-1".to_string()),
            case_id: Some("case-1".to_string()),
            workload_id: Some("workload-1".to_string()),
            seed_id: Some("seed-1".to_string()),
            fingerprint: Some("abc123".to_string()),
            artifact_ids: vec!["artifact-1".to_string()],
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }],
        artifacts: vec![FuzzArtifact {
            schema: FUZZ_ARTIFACT_SCHEMA.to_string(),
            id: "artifact-1".to_string(),
            kind: "log".to_string(),
            artifact: None,
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }],
        thresholds: vec![FuzzThreshold {
            schema: FUZZ_THRESHOLD_SCHEMA.to_string(),
            id: "threshold-1".to_string(),
            metric: "coverage_ratio".to_string(),
            operator: FuzzThresholdOperator::GreaterThanOrEqual,
            value: 0.75,
            unit: None,
            safety_class: Some(FuzzSafetyClass::ReadOnly),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }],
        provenance: Some(FuzzProvenance {
            schema: FUZZ_PROVENANCE_SCHEMA.to_string(),
            producer: "runner".to_string(),
            producer_version: Some("1.0.0".to_string()),
            invocation: Some("run campaign-1".to_string()),
            run_id: Some("run-1".to_string()),
            source_ref: Some("abc123".to_string()),
            created_at: Some("2026-06-21T00:00:00Z".to_string()),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }),
        replay: Some(FuzzReplayMetadata {
            schema: FUZZ_REPLAY_SCHEMA.to_string(),
            id: "replay-1".to_string(),
            command: Some("homeboy fuzz run component --workload workload-1".to_string()),
            args: Vec::new(),
            env: vec!["HOMEBOY_FUZZ_SEED=sample".to_string()],
            seed: Some("sample".to_string()),
            artifact_id: Some("artifact-1".to_string()),
            metadata: Value::Null,
            extra: BTreeMap::new(),
        }),
        lifecycle: Some(LifecycleResultMetadata {
            schema: crate::core::lifecycle::LIFECYCLE_RESULT_SCHEMA.to_string(),
            version: crate::core::lifecycle::LIFECYCLE_CONTRACT_VERSION,
            phases: vec![crate::core::lifecycle::LifecyclePhaseResult {
                id: "snapshot".to_string(),
                phase: crate::core::lifecycle::LifecyclePhaseKind::Snapshot,
                status: crate::core::lifecycle::LifecyclePhaseStatus::Passed,
                snapshot_ref: Some("snapshot-1".to_string()),
                started_at: None,
                finished_at: None,
                message: None,
            }],
            snapshot_refs: vec![crate::core::lifecycle::LifecycleSnapshotRef {
                schema: crate::core::lifecycle::LIFECYCLE_SNAPSHOT_REF_SCHEMA.to_string(),
                id: "snapshot-1".to_string(),
                kind: "database".to_string(),
                phase_id: Some("snapshot".to_string()),
                artifact_id: Some("artifact-1".to_string()),
                artifact: None,
                locator: None,
                created_at: None,
                metadata: BTreeMap::new(),
            }],
            metadata: BTreeMap::new(),
        }),
        metadata: Value::Null,
        extra: BTreeMap::new(),
    };

    let value = serde_json::to_value(campaign).expect("campaign json");

    assert_eq!(value["schema"], FUZZ_CAMPAIGN_SCHEMA);
    assert_eq!(value["version"], FUZZ_CONTRACT_VERSION);
    assert_eq!(value["targets"][0]["schema"], FUZZ_TARGET_SCHEMA);
    assert_eq!(value["workloads"][0]["schema"], FUZZ_WORKLOAD_SCHEMA);
    assert_eq!(value["cases"][0]["schema"], FUZZ_CASE_SCHEMA);
    assert_eq!(value["seeds"][0]["schema"], FUZZ_SEED_SCHEMA);
    assert_eq!(value["coverage"][0]["schema"], FUZZ_COVERAGE_SCHEMA);
    assert_eq!(
        value["coverage_summary"]["schema"],
        FUZZ_COVERAGE_SUMMARY_SCHEMA
    );
    assert_eq!(
        value["coverage_summary"]["surface_summaries"][0]["id"],
        "surface-1"
    );
    assert_eq!(value["coverage_summary"]["kind_summaries"][0]["id"], "read");
    assert_eq!(value["findings"][0]["schema"], FUZZ_FINDING_SCHEMA);
    assert_eq!(value["artifacts"][0]["schema"], FUZZ_ARTIFACT_SCHEMA);
    assert_eq!(value["thresholds"][0]["schema"], FUZZ_THRESHOLD_SCHEMA);
    assert_eq!(value["provenance"]["schema"], FUZZ_PROVENANCE_SCHEMA);
    assert_eq!(value["replay"]["schema"], FUZZ_REPLAY_SCHEMA);
    assert_eq!(
        value["workloads"][0]["lifecycle"]["schema"],
        crate::core::lifecycle::LIFECYCLE_CONTRACT_SCHEMA
    );
    assert_eq!(
        value["lifecycle"]["snapshot_refs"][0]["schema"],
        crate::core::lifecycle::LIFECYCLE_SNAPSHOT_REF_SCHEMA
    );
}

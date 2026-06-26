#![cfg(test)]

use super::super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
use super::execution::{
    default_runner_contract, fuzz_artifact_ref_validation, fuzz_campaign_contract,
    fuzz_evidence_followups, fuzz_max_duration, fuzz_postprocess_error,
    fuzz_run_artifact_validation_error, fuzz_run_outcome, fuzz_runner_env,
    persist_fuzz_run_evidence, run_fuzz_artifact_postprocess, FuzzRunEvidenceInput,
};
use super::planning::plan_inventory_selection;
use super::replay::run_replay;
use super::report::{
    evaluate_fuzz_gates, fuzz_coverage_completeness, fuzz_performance_hotspots, gate_status,
    run_report, run_validate, FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND,
};
use super::types::{
    FuzzCommand, FuzzDiscoverArgs, FuzzExecutionOutput, FuzzGateProfileArg, FuzzListOutput,
    FuzzOutput, FuzzPlanArgs, FuzzPlanStrategy, FuzzReplayArgs, FuzzReportArgs, FuzzRunArgs,
    FuzzRunOutput, FuzzRunnerContract, FuzzValidateArgs, FuzzWorkloadOutput,
};
use super::workloads::{
    fuzz_workloads, resolve_component_id, rig_component_for_fuzz, select_workload, FuzzRigContext,
};
use super::{run_contract, run_discover, FuzzArgs};
use clap::Parser;
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::FuzzConfig;
use homeboy::core::fuzz::{
    FuzzCampaign, FuzzCoverageSummary, FuzzFinding, FuzzFindingStatus, FuzzTargetInventory,
};
use homeboy::core::lifecycle::{
    LifecyclePhaseKind, LifecyclePhaseResult, LifecyclePhaseStatus, LifecycleResultMetadata,
    LIFECYCLE_CONTRACT_VERSION, LIFECYCLE_RESULT_SCHEMA,
};
use homeboy::core::observation::{ObservationStore, RunRecord};
use homeboy::core::rig::RigSpec;
use homeboy::test_support::with_isolated_home;
use std::fs;
use std::path::{Path, PathBuf};

mod execution_tests;
mod gate_tests;
mod planning_tests;
mod replay_tests;
mod report_tests;
mod workload_tests;

#[derive(Parser)]
struct FuzzCli {
    #[command(flatten)]
    args: FuzzArgs,
}

fn zero_coverage_summary() -> FuzzCoverageSummary {
    FuzzCoverageSummary {
        schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
        declared_targets: 0,
        executable_targets: 0,
        proven_targets: 0,
        declared_operations: 0,
        executable_operations: 0,
        proven_operations: 0,
        skipped_targets: Vec::new(),
        skipped_operations: Vec::new(),
        surface_summaries: Vec::new(),
        kind_summaries: Vec::new(),
        artifact_ids: Vec::new(),
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    }
}

fn planner_args() -> FuzzPlanArgs {
    FuzzPlanArgs {
        run: FuzzRunArgs {
            comp: PositionalComponentArgs {
                component: Some("component-a".to_string()),
                path: None,
            },
            rig: None,
            extension_override: ExtensionOverrideArgs::default(),
            setting_args: SettingArgs::default(),
            workload_id: Some("api-fuzz".to_string()),
            run_id: Some("proof-1".to_string()),
            seed: None,
            inventory: None,
            require_case_log: false,
            require_coverage_summary: false,
            require_result_envelope: false,
            max_duration: None,
            gate_profile: FuzzGateProfileArg::Measurement,
            args: Vec::new(),
        },
        request_id: None,
        strategy: FuzzPlanStrategy::All,
        operations: Vec::new(),
        operation_families: Vec::new(),
        case_budget: None,
        duration_budget_seconds: None,
    }
}

fn planner_inventory() -> FuzzTargetInventory {
    FuzzTargetInventory::from_value(serde_json::json!({
        "schema": "homeboy/fuzz-target-inventory/v1",
        "version": 1,
        "id": "component-a-inventory",
        "targets": [
            {
                "schema": "homeboy/fuzz-target/v1",
                "id": "api.users",
                "kind": "api",
                "operations": [
                    { "id": "api.users.read", "kind": "GET", "family": "read" },
                    { "id": "api.users.create", "kind": "POST", "family": "create" }
                ]
            }
        ],
        "workloads": [
            {
                "schema": "homeboy/fuzz-workload/v1",
                "id": "api-fuzz",
                "safety_class": "isolated_mutation",
                "seed_ids": ["seed-a"],
                "case_budget": 25,
                "duration_budget_seconds": 120
            }
        ],
        "seeds": [
            {
                "schema": "homeboy/fuzz-seed/v1",
                "id": "seed-a",
                "kind": "corpus",
                "value": "inline-corpus"
            }
        ],
        "provenance": {
            "producer": "inventory-test",
            "run_id": "inventory-discovery"
        }
    }))
    .expect("inventory parses")
}

fn write_inventory(path: &Path, inventory: &FuzzTargetInventory) {
    fs::write(
        path,
        serde_json::to_string(inventory).expect("serialize inventory"),
    )
    .expect("write inventory");
}

fn empty_fuzz_campaign() -> FuzzCampaign {
    FuzzCampaign {
        schema: homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA.to_string(),
        version: homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
        id: "campaign-1".to_string(),
        title: None,
        safety_class: homeboy::core::fuzz::FuzzSafetyClass::ReadOnly,
        surfaces: Vec::new(),
        targets: Vec::new(),
        workloads: Vec::new(),
        cases: Vec::new(),
        seeds: Vec::new(),
        coverage: Vec::new(),
        coverage_summary: None,
        findings: Vec::new(),
        artifacts: Vec::new(),
        thresholds: Vec::new(),
        lifecycle: None,
        provenance: None,
        replay: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    }
}

fn artifact_complete_fuzz_campaign() -> FuzzCampaign {
    let mut campaign = empty_fuzz_campaign();
    campaign.coverage_summary = Some(zero_coverage_summary());
    campaign.artifacts = vec![homeboy::core::fuzz::FuzzArtifact {
        schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
        id: "case-log".to_string(),
        kind: "case_log".to_string(),
        artifact: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    }];
    campaign
}

fn fuzz_run_args_with_run_id(run_id: &str) -> FuzzRunArgs {
    FuzzRunArgs {
        comp: PositionalComponentArgs {
            component: Some("component-a".to_string()),
            path: None,
        },
        rig: Some("package-fuzz".to_string()),
        extension_override: ExtensionOverrideArgs { extensions: vec![] },
        setting_args: SettingArgs {
            setting: vec![],
            setting_json: vec![],
        },
        workload_id: Some("parser".to_string()),
        run_id: Some(run_id.to_string()),
        seed: Some("1234".to_string()),
        inventory: None,
        require_case_log: false,
        require_coverage_summary: false,
        require_result_envelope: false,
        max_duration: None,
        gate_profile: FuzzGateProfileArg::Measurement,
        args: vec![],
    }
}

fn seed_fuzz_run(run_id: &str) {
    let store = ObservationStore::open_initialized().expect("store");
    let now = chrono::Utc::now().to_rfc3339();
    store
        .upsert_imported_run(&RunRecord {
            id: run_id.to_string(),
            kind: "fuzz".to_string(),
            component_id: Some("component-a".to_string()),
            started_at: now.clone(),
            finished_at: Some(now),
            status: "pass".to_string(),
            command: Some(format!("homeboy fuzz run component-a --run-id {run_id}")),
            cwd: None,
            homeboy_version: Some("test-version".to_string()),
            git_sha: None,
            rig_id: Some("package-fuzz".to_string()),
            metadata_json: serde_json::json!({}),
        })
        .expect("seed fuzz run");
}

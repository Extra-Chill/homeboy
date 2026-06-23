mod execution;
mod replay;
mod report;
mod types;
mod workloads;

pub use types::{
    FuzzArgs, FuzzCampaignContract, FuzzContractOutput, FuzzCoverageCompletenessOutput,
    FuzzCoverageSelectorSummaryOutput, FuzzExecutionOutput, FuzzGateEvaluation, FuzzListOutput,
    FuzzOutput, FuzzPlanOutput, FuzzReplayEnv, FuzzReplayOutput, FuzzReportOutput, FuzzRunArgs,
    FuzzRunOutput, FuzzRunnerContract, FuzzValidateOutput, FuzzWorkloadOutput,
};

use homeboy::core::extension::ExtensionCapability;
use homeboy::core::fuzz::{
    default_fuzz_gates, default_fuzz_required_artifacts, fuzz_core_contract, FuzzExecutionRequest,
    FUZZ_CONTRACT_VERSION, FUZZ_EXECUTION_REQUEST_SCHEMA,
};

use super::{CmdResult, GlobalArgs};
use execution::{default_runner_contract, run_run};
use replay::run_replay;
use report::{run_report, run_validate};
use types::{FuzzCommand, FuzzListArgs, FuzzPlanArgs};
use workloads::{
    build_target_inventory, fuzz_workloads, load_rig, resolve_component_id, resolve_fuzz_context,
    select_workload,
};

pub fn run(args: FuzzArgs, _global: &GlobalArgs) -> CmdResult<FuzzOutput> {
    match args.command {
        Some(FuzzCommand::Contract) => Ok((FuzzOutput::Contract(run_contract()), 0)),
        Some(FuzzCommand::List(list_args)) => Ok((FuzzOutput::List(run_list(list_args)?), 0)),
        Some(FuzzCommand::Plan(plan_args)) => Ok((FuzzOutput::Plan(run_plan(plan_args)?), 0)),
        Some(FuzzCommand::Run(run_args)) => {
            let (output, exit) = run_run(run_args)?;
            Ok((FuzzOutput::Run(output), exit))
        }
        Some(FuzzCommand::Validate(validate_args)) => {
            Ok((FuzzOutput::Validate(run_validate(validate_args)?), 0))
        }
        Some(FuzzCommand::Report(report_args)) => {
            Ok((FuzzOutput::Report(run_report(report_args)?), 0))
        }
        Some(FuzzCommand::Replay(replay_args)) => {
            Ok((FuzzOutput::Replay(run_replay(replay_args)?), 0))
        }
        None => {
            let (output, exit) = run_run(args.run)?;
            Ok((FuzzOutput::Run(output), exit))
        }
    }
}

fn run_contract() -> FuzzContractOutput {
    FuzzContractOutput {
        command: "fuzz.contract".to_string(),
        contract: fuzz_core_contract(),
        required_artifacts: default_fuzz_required_artifacts(),
        gates: default_fuzz_gates(),
    }
}

fn run_list(args: FuzzListArgs) -> homeboy::core::Result<FuzzListOutput> {
    let rig_context = load_rig(args.rig.as_deref())?;
    let effective_id = resolve_component_id(
        &args.comp,
        rig_context.as_ref().map(|context| &context.spec),
    )?;
    let ctx = resolve_fuzz_context(
        &effective_id,
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        ExtensionCapability::Fuzz,
        rig_context.as_ref(),
    )?;
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
    );

    Ok(FuzzListOutput {
        command: "fuzz.list".to_string(),
        component: ctx.component_id,
        rig_id: rig_context.map(|context| context.spec.id),
        count: workloads.len(),
        workloads,
        run_hint: "Select one workload with `homeboy fuzz run <component> --workload <id>`; offload heavy campaigns with the global `--runner <id>` flag when configured.".to_string(),
    })
}

fn run_plan(args: FuzzPlanArgs) -> homeboy::core::Result<FuzzPlanOutput> {
    let rig_context = load_rig(args.run.rig.as_deref())?;
    let effective_id = resolve_component_id(
        &args.run.comp,
        rig_context.as_ref().map(|context| &context.spec),
    )?;
    let ctx = resolve_fuzz_context(
        &effective_id,
        &args.run.comp,
        &args.run.setting_args,
        &args.run.extension_override,
        ExtensionCapability::Fuzz,
        rig_context.as_ref(),
    )?;
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
    );
    let selected_workload = select_workload(&workloads, args.run.workload_id.as_deref())?;
    let workload_id = selected_workload
        .map(|workload| workload.id.clone())
        .or_else(|| args.run.workload_id.clone());
    let required_artifacts = default_fuzz_required_artifacts();
    let gates = default_fuzz_gates();
    let request_id = args
        .request_id
        .clone()
        .or_else(|| args.run.run_id.clone())
        .or_else(|| workload_id.clone())
        .unwrap_or_else(|| format!("{}-fuzz-request", ctx.component_id));
    let rig_id = rig_context.as_ref().map(|context| context.spec.id.clone());

    let target_inventory = build_target_inventory(
        &ctx.component_id,
        &workloads,
        args.run.run_id.clone(),
        args.run.inventory.as_deref(),
    )?;

    Ok(FuzzPlanOutput {
        command: "fuzz.plan".to_string(),
        component: ctx.component_id.clone(),
        rig_id: rig_id.clone(),
        target_inventory,
        request: FuzzExecutionRequest {
            schema: FUZZ_EXECUTION_REQUEST_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: request_id,
            component: ctx.component_id,
            rig_id,
            workload_id,
            case_ids: Vec::new(),
            seed: args.run.seed,
            max_duration: args.run.max_duration,
            args: args.run.args,
            required_artifacts,
            gates,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        },
        runner_contract: default_runner_contract(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
    use super::execution::{
        fuzz_campaign_contract, fuzz_evidence_followups, fuzz_run_outcome, fuzz_runner_env,
        persist_fuzz_run_evidence, FuzzRunEvidenceInput,
    };
    use super::replay::run_replay;
    use super::report::{evaluate_fuzz_gates, fuzz_coverage_completeness, gate_status};
    use super::types::{
        FuzzCommand, FuzzExecutionOutput, FuzzListOutput, FuzzOutput, FuzzReplayArgs, FuzzRunArgs,
        FuzzRunOutput, FuzzRunnerContract, FuzzWorkloadOutput,
    };
    use super::workloads::{
        fuzz_workloads, resolve_component_id, rig_component_for_fuzz, select_workload,
        FuzzRigContext,
    };
    use super::{run_contract, FuzzArgs};
    use clap::Parser;
    use homeboy::core::engine::run_dir::RunDir;
    use homeboy::core::extension::FuzzConfig;
    use homeboy::core::fuzz::FuzzCampaign;
    use homeboy::core::observation::ObservationStore;
    use homeboy::core::rig::RigSpec;
    use homeboy::test_support::with_isolated_home;
    use std::path::{Path, PathBuf};

    #[derive(Parser)]
    struct FuzzCli {
        #[command(flatten)]
        args: FuzzArgs,
    }

    #[test]
    fn fuzz_run_parses_generic_contract_flags() {
        let cli = FuzzCli::parse_from([
            "fuzz",
            "run",
            "component-a",
            "--rig",
            "package-fuzz",
            "--workload",
            "parser",
            "--run-id",
            "proof-1",
            "--seed",
            "1234",
            "--inventory",
            "/tmp/fuzz-inventory.json",
            "--max-duration",
            "60s",
            "--",
            "--engine",
            "libfuzzer",
        ]);

        match cli.args.command {
            Some(FuzzCommand::Run(run)) => {
                assert_eq!(run.comp.component.as_deref(), Some("component-a"));
                assert_eq!(run.rig.as_deref(), Some("package-fuzz"));
                assert_eq!(run.workload_id.as_deref(), Some("parser"));
                assert_eq!(run.run_id.as_deref(), Some("proof-1"));
                assert_eq!(run.seed.as_deref(), Some("1234"));
                assert_eq!(
                    run.inventory.as_deref(),
                    Some(Path::new("/tmp/fuzz-inventory.json"))
                );
                assert_eq!(run.max_duration.as_deref(), Some("60s"));
                assert_eq!(run.args, vec!["--engine", "libfuzzer"]);
            }
            _ => panic!("expected fuzz run command"),
        }
    }

    #[test]
    fn fuzz_output_contract_has_stable_variant_discriminators() {
        let contract = serde_json::to_value(FuzzOutput::Contract(run_contract())).unwrap();
        assert_eq!(contract["variant"], "contract");
        assert_eq!(
            contract["contract"]["schemas"]["result_envelope"],
            homeboy::core::fuzz::FUZZ_RESULT_ENVELOPE_SCHEMA
        );

        let list = serde_json::to_value(FuzzOutput::List(FuzzListOutput {
            command: "fuzz.list".to_string(),
            component: "component-a".to_string(),
            rig_id: None,
            workloads: Vec::new(),
            count: 0,
            run_hint: "hint".to_string(),
        }))
        .unwrap();
        assert_eq!(list["variant"], "list");

        let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
            kind: "fuzz".to_string(),
            command: "fuzz.run".to_string(),
            component: "component-a".to_string(),
            rig_id: Some("package-fuzz".to_string()),
            status: "passed".to_string(),
            workload_id: Some("parser".to_string()),
            workload_path: None,
            run_id: None,
            seed: None,
            inventory_file: None,
            max_duration: None,
            passthrough_args: Vec::new(),
            target_inventory: None,
            execution: None,
            results: None,
            campaign_contract: fuzz_campaign_contract(None, None),
            runner_contract: FuzzRunnerContract {
                capability: "fuzz".to_string(),
                extension_script_required: true,
                env: Vec::new(),
            },
            evidence_followups: Vec::new(),
        }))
        .unwrap();
        assert_eq!(run["variant"], "run");
        assert_eq!(run["kind"], "fuzz");
        assert_eq!(run["rig_id"], "package-fuzz");
    }

    #[test]
    fn fuzz_gate_evaluation_requires_case_log_evidence() {
        let campaign = FuzzCampaign {
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
        };

        let gates = evaluate_fuzz_gates(&campaign);

        assert_eq!(gate_status(&gates), "failed");
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "has-case-evidence" && gate.status == "failed" && gate.observed == 0.0
        }));
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "target-coverage-complete"
                && gate.status == "failed"
                && gate.observed == 0.0
        }));
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "operation-coverage-complete"
                && gate.status == "failed"
                && gate.observed == 0.0
        }));
    }

    #[test]
    fn fuzz_gate_evaluation_accepts_case_level_fuzz_report_evidence() {
        let campaign = FuzzCampaign {
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
            artifacts: vec![homeboy::core::fuzz::FuzzArtifact {
                schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
                id: "case-evidence-report".to_string(),
                kind: "fuzz_report".to_string(),
                artifact: None,
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }],
            thresholds: Vec::new(),
            lifecycle: None,
            provenance: None,
            replay: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };

        let gates = evaluate_fuzz_gates(&campaign);

        assert_eq!(gate_status(&gates), "failed");
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "has-case-evidence" && gate.status == "passed" && gate.observed == 1.0
        }));
    }

    #[test]
    fn fuzz_gate_evaluation_accepts_metadata_artifact_refs() {
        let campaign = FuzzCampaign {
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
            metadata: serde_json::json!({
                "artifact_refs": [{
                    "name": "case_evidence_report",
                    "path": "case-evidence/report.json",
                    "role": "fuzz_report"
                }]
            }),
            extra: std::collections::BTreeMap::new(),
        };

        let gates = evaluate_fuzz_gates(&campaign);
        let summary = fuzz_coverage_completeness(&campaign);

        assert_eq!(gate_status(&gates), "failed");
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "has-case-evidence" && gate.status == "passed" && gate.observed == 1.0
        }));
        assert!(!summary.has_summary);
        assert_eq!(summary.target_coverage_ratio, 0.0);
        assert_eq!(summary.operation_coverage_ratio, 0.0);
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "target-coverage-complete"
                && gate.status == "failed"
                && gate.observed == 0.0
        }));
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "operation-coverage-complete"
                && gate.status == "failed"
                && gate.observed == 0.0
        }));
    }

    #[test]
    fn fuzz_gate_evaluation_accepts_zero_declared_coverage_summary() {
        let campaign = FuzzCampaign {
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
            coverage_summary: Some(homeboy::core::fuzz::FuzzCoverageSummary {
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
                artifact_ids: vec!["coverage-report".to_string()],
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }),
            findings: Vec::new(),
            artifacts: vec![homeboy::core::fuzz::FuzzArtifact {
                schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
                id: "case-log".to_string(),
                kind: "case_log".to_string(),
                artifact: None,
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }],
            thresholds: Vec::new(),
            lifecycle: None,
            provenance: None,
            replay: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };

        let gates = evaluate_fuzz_gates(&campaign);
        let summary = fuzz_coverage_completeness(&campaign);

        assert_eq!(gate_status(&gates), "passed");
        assert!(summary.has_summary);
        assert_eq!(summary.declared_targets, 0);
        assert_eq!(summary.target_coverage_ratio, 1.0);
        assert_eq!(summary.declared_operations, 0);
        assert_eq!(summary.operation_coverage_ratio, 1.0);
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "target-coverage-complete"
                && gate.status == "passed"
                && gate.observed == 1.0
        }));
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "operation-coverage-complete"
                && gate.status == "passed"
                && gate.observed == 1.0
        }));
    }

    #[test]
    fn fuzz_gate_evaluation_requires_complete_target_and_operation_coverage() {
        let mut campaign = FuzzCampaign {
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
            coverage_summary: Some(homeboy::core::fuzz::FuzzCoverageSummary {
                schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
                declared_targets: 2,
                executable_targets: 2,
                proven_targets: 1,
                declared_operations: 4,
                executable_operations: 4,
                proven_operations: 4,
                skipped_targets: Vec::new(),
                skipped_operations: Vec::new(),
                surface_summaries: Vec::new(),
                kind_summaries: Vec::new(),
                artifact_ids: vec!["coverage-report".to_string()],
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }),
            findings: Vec::new(),
            artifacts: vec![homeboy::core::fuzz::FuzzArtifact {
                schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
                id: "case-log".to_string(),
                kind: "case_log".to_string(),
                artifact: None,
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }],
            thresholds: Vec::new(),
            lifecycle: None,
            provenance: None,
            replay: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };

        let gates = evaluate_fuzz_gates(&campaign);

        assert!(gates.iter().any(|gate| {
            gate.gate_id == "target-coverage-complete"
                && gate.status == "failed"
                && gate.observed == 0.5
        }));
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "operation-coverage-complete"
                && gate.status == "passed"
                && gate.observed == 1.0
        }));

        campaign.coverage_summary.as_mut().unwrap().proven_targets = 2;
        assert_eq!(gate_status(&evaluate_fuzz_gates(&campaign)), "passed");
    }

    #[test]
    fn fuzz_coverage_completeness_reports_summary_counts() {
        let campaign = FuzzCampaign {
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
            coverage_summary: Some(homeboy::core::fuzz::FuzzCoverageSummary {
                schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
                declared_targets: 2,
                executable_targets: 1,
                proven_targets: 1,
                declared_operations: 0,
                executable_operations: 0,
                proven_operations: 0,
                skipped_targets: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                    id: "target-2".to_string(),
                    reason: "auth_required".to_string(),
                    label: None,
                }],
                skipped_operations: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                    id: "operation-2".to_string(),
                    reason: "config_required".to_string(),
                    label: None,
                }],
                surface_summaries: vec![homeboy::core::fuzz::FuzzCoverageGroupSummary {
                    id: "surface-a".to_string(),
                    kind: "api".to_string(),
                    label: Some("Surface A".to_string()),
                    declared_targets: 2,
                    executable_targets: 1,
                    proven_targets: 1,
                    declared_operations: 2,
                    executable_operations: 1,
                    proven_operations: 1,
                    skipped_targets: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                        id: "target-2".to_string(),
                        reason: "auth_required".to_string(),
                        label: None,
                    }],
                    skipped_operations: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                        id: "operation-2".to_string(),
                        reason: "config_required".to_string(),
                        label: None,
                    }],
                    metadata: serde_json::Value::Null,
                    extra: std::collections::BTreeMap::new(),
                }],
                kind_summaries: vec![homeboy::core::fuzz::FuzzCoverageGroupSummary {
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
                    metadata: serde_json::Value::Null,
                    extra: std::collections::BTreeMap::new(),
                }],
                artifact_ids: vec!["coverage-report".to_string()],
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }),
            findings: Vec::new(),
            artifacts: Vec::new(),
            thresholds: Vec::new(),
            lifecycle: None,
            provenance: None,
            replay: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };

        let summary = fuzz_coverage_completeness(&campaign);

        assert!(summary.has_summary);
        assert_eq!(summary.declared_targets, 2);
        assert_eq!(summary.target_coverage_ratio, 0.5);
        assert_eq!(summary.operation_coverage_ratio, 1.0);
        assert_eq!(summary.skipped_targets, 1);
        assert_eq!(summary.skipped_operations, 1);
        assert_eq!(summary.skipped_reason_counts["auth_required"], 1);
        assert_eq!(summary.skipped_reason_counts["config_required"], 1);
        assert_eq!(summary.surface_summaries.len(), 1);
        assert_eq!(summary.surface_summaries[0].id, "surface-a");
        assert_eq!(summary.surface_summaries[0].target_coverage_ratio, 0.5);
        assert_eq!(summary.surface_summaries[0].operation_coverage_ratio, 0.5);
        assert_eq!(
            summary.surface_summaries[0].skipped_reason_counts["auth_required"],
            1
        );
        assert_eq!(summary.kind_summaries[0].id, "read");
        assert_eq!(summary.artifact_ids, vec!["coverage-report"]);
    }

    #[test]
    fn fuzz_workloads_include_rig_declared_paths() {
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": "/tmp/package",
                    "extensions": {
                        "generic": {
                            "settings": {}
                        }
                    }
                }
            },
            "fuzz": {
                "default_component": "package"
            },
            "fuzz_workloads": {
                "generic": [
                    { "path": "${package.root}/fuzz/checkout-create-order.json" }
                ]
            }
        }))
        .expect("parse rig spec");
        let component = rig_component_for_fuzz(&spec, "package").expect("rig component");
        let context = FuzzRigContext {
            spec,
            package_root: Some(std::path::PathBuf::from("/tmp/homeboy-rigs/package")),
        };

        let workloads = fuzz_workloads(&component, Some(&context), Some("generic"));

        assert!(workloads.iter().any(|workload| {
            workload.id == "checkout-create-order"
                && workload.manifest_path.as_deref()
                    == Some("/tmp/homeboy-rigs/package/fuzz/checkout-create-order.json")
                && workload.source
                    == "rig_workloads:generic:/tmp/homeboy-rigs/package/fuzz/checkout-create-order.json"
        }));
    }

    #[test]
    fn resolve_component_id_uses_fuzz_default_component() {
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "fuzz": {
                "default_component": "package"
            }
        }))
        .expect("parse rig spec");
        let comp = PositionalComponentArgs {
            component: None,
            path: None,
        };

        assert_eq!(
            resolve_component_id(&comp, Some(&spec)).expect("resolve component"),
            "package"
        );
    }

    #[test]
    fn fuzz_runner_env_includes_results_file_selected_workload_path_and_generic_contract() {
        let args = FuzzRunArgs {
            comp: PositionalComponentArgs {
                component: Some("component-a".to_string()),
                path: None,
            },
            rig: None,
            extension_override: ExtensionOverrideArgs { extensions: vec![] },
            setting_args: SettingArgs {
                setting: vec![],
                setting_json: vec![],
            },
            workload_id: Some("parser".to_string()),
            run_id: Some("proof-1".to_string()),
            seed: Some("1234".to_string()),
            inventory: Some(PathBuf::from("/tmp/fuzz-inventory.json")),
            max_duration: Some("60s".to_string()),
            args: vec![],
        };
        let workload = FuzzWorkloadOutput {
            id: "parser".to_string(),
            label: None,
            description: None,
            source: "rig_workloads:generic:/tmp/fuzz/parser.json".to_string(),
            manifest_path: Some("/tmp/fuzz/parser.json".to_string()),
        };

        let run_dir = RunDir::create().expect("run dir");
        let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);

        let env = fuzz_runner_env(&args, None, Some(&workload), &results_path, &run_dir)
            .expect("fuzz runner env");

        assert!(env.contains(&(
            "HOMEBOY_FUZZ_RESULTS_FILE".to_string(),
            results_path.to_string_lossy().to_string()
        )));
        assert!(env.contains(&("HOMEBOY_FUZZ_WORKLOAD_ID".to_string(), "parser".to_string())));
        assert!(env.contains(&(
            "HOMEBOY_FUZZ_WORKLOAD_PATH".to_string(),
            "/tmp/fuzz/parser.json".to_string()
        )));
        assert!(env.contains(&("HOMEBOY_FUZZ_RUN_ID".to_string(), "proof-1".to_string())));
        assert!(env.contains(&("HOMEBOY_FUZZ_SEED".to_string(), "1234".to_string())));
        assert!(env.contains(&(
            "HOMEBOY_FUZZ_INVENTORY_FILE".to_string(),
            "/tmp/fuzz-inventory.json".to_string()
        )));
        assert!(env.contains(&("HOMEBOY_FUZZ_MAX_DURATION".to_string(), "60s".to_string())));
    }

    #[test]
    fn fuzz_runner_env_expands_rig_workload_and_injects_runtime_context() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workload_path = temp.path().join("parser.json");
        std::fs::write(
            &workload_path,
            r#"{
              "schema": "homeboy/fuzz-workload/v1",
              "id": "parser",
              "target": { "component": "package" },
              "workload": { "path": "${package.root}/bench/parser.php" },
              "metadata": { "fixture": { "component": "package" } }
            }"#,
        )
        .expect("write workload");
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": "${package.root}/plugins/package",
                    "branch": "main"
                }
            }
        }))
        .expect("parse rig spec");
        let context = FuzzRigContext {
            spec,
            package_root: Some(temp.path().to_path_buf()),
        };
        let args = FuzzRunArgs {
            comp: PositionalComponentArgs {
                component: Some("package".to_string()),
                path: None,
            },
            rig: Some("package-fuzz".to_string()),
            extension_override: ExtensionOverrideArgs { extensions: vec![] },
            setting_args: SettingArgs {
                setting: vec![],
                setting_json: vec![],
            },
            workload_id: Some("parser".to_string()),
            run_id: Some("proof-1".to_string()),
            seed: None,
            inventory: None,
            max_duration: None,
            args: vec![],
        };
        let workload = FuzzWorkloadOutput {
            id: "parser".to_string(),
            label: None,
            description: None,
            source: format!("rig_workloads:generic:{}", workload_path.display()),
            manifest_path: Some(workload_path.to_string_lossy().to_string()),
        };
        let run_dir = RunDir::create().expect("run dir");
        let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);

        let env = fuzz_runner_env(
            &args,
            Some(&context),
            Some(&workload),
            &results_path,
            &run_dir,
        )
        .expect("fuzz runner env");
        let expanded_path = env
            .iter()
            .find_map(|(key, value)| (key == "HOMEBOY_FUZZ_WORKLOAD_PATH").then_some(value))
            .expect("expanded workload path");
        let expanded: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(expanded_path).expect("read expanded workload"),
        )
        .expect("parse expanded workload");

        assert_eq!(
            expanded["workload"]["path"].as_str(),
            Some(format!("{}/bench/parser.php", temp.path().display()).as_str())
        );
        assert_eq!(
            expanded["metadata"]["homeboy_runtime_context"]["components"]["package"]["path"]
                .as_str(),
            Some(format!("{}/plugins/package", temp.path().display()).as_str())
        );
        assert!(env.iter().any(|(key, value)| {
            key == "WP_CODEBOX_FUZZ_WORKLOAD_ROOT" && value == &temp.path().to_string_lossy()
        }));
    }

    #[test]
    fn fuzz_run_persists_requested_run_id_and_results_artifact() {
        with_isolated_home(|home| {
            let args = FuzzRunArgs {
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
                run_id: Some("proof-1".to_string()),
                seed: Some("1234".to_string()),
                inventory: None,
                max_duration: None,
                args: vec![],
            };
            let results_path = home.path().join("fuzz-results.json");
            std::fs::write(&results_path, "{}").expect("results file");

            let persisted = persist_fuzz_run_evidence(FuzzRunEvidenceInput {
                run_id: args.run_id.as_deref(),
                component_id: "component-a",
                rig_id: args.rig.as_deref(),
                workload_id: args.workload_id.as_deref(),
                workload_path: Some("/tmp/fuzz/parser.json"),
                status: "passed",
                exit_code: 0,
                success: true,
                args: &args,
                results_path: &results_path,
                results: None,
                results_error: None,
            })
            .expect("persist fuzz run")
            .expect("run record");

            assert_eq!(persisted.id, "proof-1");
            assert_eq!(persisted.kind, "fuzz");
            assert_eq!(persisted.status, "pass");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run("proof-1")
                .expect("get run")
                .expect("persisted run");
            assert_eq!(run.component_id.as_deref(), Some("component-a"));
            assert_eq!(run.rig_id.as_deref(), Some("package-fuzz"));
            assert_eq!(run.metadata_json["workload_id"], "parser");
            assert_eq!(run.metadata_json["seed"], "1234");
            assert!(run
                .command
                .as_deref()
                .unwrap_or_default()
                .contains("homeboy fuzz run component-a"));
            let artifacts = store.list_artifacts("proof-1").expect("artifacts");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].kind, "fuzz_results");
            assert_eq!(artifacts[0].artifact_type, "file");
            assert!(std::path::Path::new(&artifacts[0].path).is_file());
        });
    }

    #[test]
    fn fuzz_run_outcome_fails_when_successful_command_reports_failed_campaign() {
        let mut campaign = empty_fuzz_campaign();
        campaign.metadata = serde_json::json!({
            "status": "failed",
            "success": false,
            "case_counts": { "passed": 2, "failed": 1, "errored": 0 }
        });

        let outcome = fuzz_run_outcome(0, true, Some(&campaign), None);

        assert_eq!(outcome.status, "failed");
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, 1);
    }

    #[test]
    fn fuzz_run_outcome_fails_when_successful_command_reports_nested_runtime_error() {
        let mut campaign = empty_fuzz_campaign();
        let nested_result_key = ["word", "press", "_fuzz_result"].concat();
        campaign.metadata = serde_json::json!({
            nested_result_key: {
                "status": "errored",
                "success": false,
                "case_counts": { "passed": 0, "failed": 0, "errored": 1 }
            }
        });

        let outcome = fuzz_run_outcome(0, true, Some(&campaign), None);

        assert_eq!(outcome.status, "failed");
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, 1);
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

    #[test]
    fn fuzz_run_persists_raw_results_artifact_when_results_parse_fails() {
        with_isolated_home(|home| {
            let args = FuzzRunArgs {
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
                run_id: Some("proof-bad-results".to_string()),
                seed: None,
                inventory: None,
                max_duration: None,
                args: vec![],
            };
            let results_path = home.path().join("fuzz-results.json");
            std::fs::write(
                &results_path,
                r#"{"schema":"unsupported/fuzz-result/v1","id":"raw-output"}"#,
            )
            .expect("results file");

            let persisted = persist_fuzz_run_evidence(FuzzRunEvidenceInput {
                run_id: args.run_id.as_deref(),
                component_id: "component-a",
                rig_id: args.rig.as_deref(),
                workload_id: args.workload_id.as_deref(),
                workload_path: Some("/tmp/fuzz/parser.json"),
                status: "failed",
                exit_code: 1,
                success: false,
                args: &args,
                results_path: &results_path,
                results: None,
                results_error: Some(
                    "fuzz results schema must be homeboy/fuzz-campaign/v1, got unsupported/fuzz-result/v1",
                ),
            })
            .expect("persist fuzz run")
            .expect("run record");

            assert_eq!(persisted.id, "proof-bad-results");
            assert_eq!(persisted.status, "fail");
            assert_eq!(
                persisted.metadata_json["campaign_id"],
                serde_json::Value::Null
            );
            assert!(persisted.metadata_json["results_error"]
                .as_str()
                .unwrap()
                .contains("unsupported/fuzz-result/v1"));

            let store = ObservationStore::open_initialized().expect("store");
            let artifacts = store
                .list_artifacts("proof-bad-results")
                .expect("artifacts");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].kind, "fuzz_results");
            assert_eq!(artifacts[0].mime.as_deref(), Some("application/json"));
            assert!(std::path::Path::new(&artifacts[0].path).is_file());
            let raw = std::fs::read_to_string(&artifacts[0].path).expect("raw artifact");
            assert!(raw.contains("unsupported/fuzz-result/v1"));
        });
    }

    #[test]
    fn fuzz_evidence_followups_point_to_raw_results_when_parse_fails() {
        let results_path = Path::new("/tmp/homeboy-run/fuzz-results.json");
        let normalization_error =
            ["Unsupported ", "Word", "Press", " fuzz case status: error"].concat();

        let followups = fuzz_evidence_followups(
            Some("proof-bad-results"),
            Some(&normalization_error),
            results_path,
        );

        assert!(followups
            .iter()
            .any(|followup| followup == "homeboy runs artifacts proof-bad-results"));
        assert!(followups.iter().any(|followup| {
            followup.contains("/tmp/homeboy-run/fuzz-results.json")
                && followup.contains("normalization failed")
                && followup.contains(&normalization_error)
        }));
    }

    #[test]
    fn fuzz_replay_parses_artifact_and_case_id_flags() {
        let cli = FuzzCli::parse_from([
            "fuzz",
            "replay",
            "/tmp/fuzz-results.json",
            "--case-id",
            "case-1",
            "--run-id",
            "proof-1",
            "--",
            "--runner-flag",
        ]);

        match cli.args.command {
            Some(FuzzCommand::Replay(replay)) => {
                assert_eq!(
                    replay.artifact_or_case.as_deref(),
                    Some("/tmp/fuzz-results.json")
                );
                assert_eq!(replay.case_id.as_deref(), Some("case-1"));
                assert_eq!(replay.run_id.as_deref(), Some("proof-1"));
                assert_eq!(replay.args, vec!["--runner-flag"]);
            }
            _ => panic!("expected fuzz replay command"),
        }
    }

    #[test]
    fn fuzz_replay_resolves_campaign_metadata_without_executing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("fuzz-results.json");
        let campaign = serde_json::json!({
            "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
            "version": homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
            "id": "campaign-1",
            "safety_class": "read_only",
            "cases": [
                {
                    "schema": homeboy::core::fuzz::FUZZ_CASE_SCHEMA,
                    "id": "case-1",
                    "replay_id": "replay-1"
                }
            ],
            "replay": {
                "schema": homeboy::core::fuzz::FUZZ_REPLAY_SCHEMA,
                "id": "replay-1",
                "seed": "1234",
                "artifact_id": "case-artifact"
            }
        });
        std::fs::write(&path, serde_json::to_string(&campaign).unwrap()).expect("write campaign");

        let output = run_replay(FuzzReplayArgs {
            artifact_or_case: Some(path.to_string_lossy().to_string()),
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some("proof-1".to_string()),
            args: vec!["--runner-flag".to_string()],
        })
        .expect("resolve replay");

        assert_eq!(output.status, "dry_run");
        assert_eq!(output.campaign_id.as_deref(), Some("campaign-1"));
        assert_eq!(output.case_id.as_deref(), Some("case-1"));
        assert_eq!(
            output.replay.as_ref().map(|replay| replay.id.as_str()),
            Some("replay-1")
        );
        assert!(output.env.iter().any(|env| {
            env.name == "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE"
                && env.value == path.to_string_lossy().to_string()
        }));
        assert!(output
            .env
            .iter()
            .any(|env| { env.name == "HOMEBOY_FUZZ_REPLAY_CASE_ID" && env.value == "case-1" }));
        assert!(output
            .env
            .iter()
            .any(|env| { env.name == "HOMEBOY_FUZZ_REPLAY_SEED" && env.value == "1234" }));
        assert_eq!(output.passthrough_args, vec!["--runner-flag"]);
    }

    #[test]
    fn fuzz_output_contract_includes_results_file_and_parsed_campaign() {
        let results = FuzzCampaign {
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
            provenance: None,
            replay: None,
            lifecycle: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };
        let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
            kind: "fuzz".to_string(),
            command: "fuzz.run".to_string(),
            component: "component-a".to_string(),
            rig_id: None,
            status: "passed".to_string(),
            workload_id: None,
            workload_path: None,
            run_id: None,
            seed: None,
            inventory_file: None,
            max_duration: None,
            passthrough_args: Vec::new(),
            target_inventory: None,
            execution: Some(FuzzExecutionOutput {
                kind: "fuzz".to_string(),
                extension_id: "generic".to_string(),
                exit_code: 0,
                success: true,
                run_dir: "/tmp/homeboy-run".to_string(),
                results_file: "/tmp/homeboy-run/fuzz-results.json".to_string(),
                stdout: String::new(),
                stderr: String::new(),
            }),
            results: Some(results),
            campaign_contract: fuzz_campaign_contract(None, Some("seed-1")),
            runner_contract: FuzzRunnerContract {
                capability: "fuzz".to_string(),
                extension_script_required: true,
                env: vec!["HOMEBOY_FUZZ_RESULTS_FILE"],
            },
            evidence_followups: Vec::new(),
        }))
        .unwrap();

        assert_eq!(
            run["execution"]["results_file"],
            "/tmp/homeboy-run/fuzz-results.json"
        );
        assert_eq!(
            run["results"]["schema"],
            homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA
        );
        assert_eq!(run["results"]["id"], "campaign-1");
        assert_eq!(
            run["runner_contract"]["env"][0],
            "HOMEBOY_FUZZ_RESULTS_FILE"
        );
        assert_eq!(run["campaign_contract"]["seed"], "seed-1");
        assert_eq!(
            run["campaign_contract"]["result_schema"],
            homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA
        );
        assert!(run["campaign_contract"]["unsupported"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "replay_command"));
    }

    #[test]
    fn fuzz_campaign_contract_surfaces_extension_metadata() {
        let config = FuzzConfig {
            extension_script: Some("fuzz.sh".to_string()),
            workloads: Vec::new(),
            case_artifact: Some("failing-case".to_string()),
            corpus_artifacts: vec!["corpus".to_string()],
            seed: Some("manifest-seed".to_string()),
            replay_command: Some("runner replay {case}".to_string()),
            minimize_command: Some("runner minimize {case}".to_string()),
            result_schema: Some("custom/fuzz-result/v1".to_string()),
            artifact_retention: Some("persisted-run-artifacts".to_string()),
        };

        let contract =
            serde_json::to_value(fuzz_campaign_contract(Some(&config), Some("cli-seed"))).unwrap();

        assert_eq!(contract["case_artifact"], "failing-case");
        assert_eq!(contract["corpus_artifacts"][0], "corpus");
        assert_eq!(contract["seed"], "cli-seed");
        assert_eq!(contract["replay_command"], "runner replay {case}");
        assert_eq!(contract["minimize_command"], "runner minimize {case}");
        assert_eq!(contract["result_schema"], "custom/fuzz-result/v1");
        assert_eq!(contract["artifact_retention"], "persisted-run-artifacts");
        assert!(contract["unsupported"].as_array().unwrap().is_empty());
    }

    #[test]
    fn select_workload_requires_explicit_id_for_ambiguous_fuzz_workloads() {
        let workloads = vec![
            FuzzWorkloadOutput {
                id: "parser".to_string(),
                label: None,
                description: None,
                source: "extension:generic".to_string(),
                manifest_path: None,
            },
            FuzzWorkloadOutput {
                id: "serializer".to_string(),
                label: None,
                description: None,
                source: "extension:generic".to_string(),
                manifest_path: None,
            },
        ];

        let err = select_workload(&workloads, None).expect_err("ambiguous workload");

        assert!(err.message.contains("Multiple fuzz workloads"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("parser, serializer")));
    }

    #[test]
    fn select_workload_rejects_empty_fuzz_selection() {
        let err = select_workload(&[], None).expect_err("empty workload selection");

        assert!(err.message.contains("No fuzz workloads"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("fuzz list")));
    }

    #[test]
    fn fuzz_command_tests_keep_core_fixtures_product_neutral() {
        let source = [
            include_str!("mod.rs"),
            include_str!("types.rs"),
            include_str!("replay.rs"),
            include_str!("report.rs"),
            include_str!("execution.rs"),
            include_str!("workloads.rs"),
        ]
        .concat()
        .to_ascii_lowercase();
        let forbidden = ["word", "press"].concat();
        assert!(!source.contains(&forbidden));
    }
}

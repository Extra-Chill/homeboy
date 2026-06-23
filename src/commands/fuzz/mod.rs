mod compare;
mod execution;
mod replay;
mod report;
mod types;
mod workloads;

pub use types::{
    FuzzArgs, FuzzCampaignContract, FuzzContractOutput, FuzzCoverageCompletenessOutput,
    FuzzCoverageSelectorSummaryOutput, FuzzDiscoverOutput, FuzzDiscoverSummary,
    FuzzExecutionOutput, FuzzGateEvaluation, FuzzListOutput, FuzzOutput, FuzzPlanOutput,
    FuzzReplayEnv, FuzzReplayOutput, FuzzReportOutput, FuzzRunArgs, FuzzRunOutput,
    FuzzRunnerContract, FuzzValidateOutput, FuzzWorkloadOutput,
};

use homeboy::core::extension::ExtensionCapability;
use homeboy::core::fuzz::{
    default_fuzz_gates, default_fuzz_required_artifacts, fuzz_core_contract, FuzzExecutionRequest,
    FuzzOperation, FuzzOperationFamily, FuzzProvenance, FuzzSafetyClass, FuzzTargetInventory,
    FUZZ_CONTRACT_VERSION, FUZZ_EXECUTION_REQUEST_SCHEMA, FUZZ_PROVENANCE_SCHEMA,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

use super::{CmdResult, GlobalArgs};
use compare::run_compare;
use execution::{default_runner_contract, run_run};
use homeboy::core::fuzz::{merge_fuzz_target_inventory, parse_fuzz_target_inventory_file};
use replay::run_replay;
use report::{run_report, run_validate};
use types::{FuzzCommand, FuzzDiscoverArgs, FuzzListArgs, FuzzPlanArgs, FuzzPlanStrategy};
use workloads::{
    build_target_inventory, fuzz_workloads, load_rig, resolve_component_id, resolve_fuzz_context,
    select_workload,
};

pub fn run(args: FuzzArgs, _global: &GlobalArgs) -> CmdResult<FuzzOutput> {
    match args.command {
        Some(FuzzCommand::Contract) => Ok((FuzzOutput::Contract(run_contract()), 0)),
        Some(FuzzCommand::Discover(discover_args)) => {
            Ok((FuzzOutput::Discover(run_discover(discover_args)?), 0))
        }
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
        Some(FuzzCommand::Compare(compare_args)) => {
            Ok((FuzzOutput::Compare(run_compare(compare_args)?), 0))
        }
        Some(FuzzCommand::Replay(replay_args)) => {
            let (output, exit) = run_replay(replay_args)?;
            Ok((FuzzOutput::Replay(output), exit))
        }
        None => {
            let (output, exit) = run_run(args.run)?;
            Ok((FuzzOutput::Run(output), exit))
        }
    }
}

fn run_discover(args: FuzzDiscoverArgs) -> homeboy::core::Result<FuzzDiscoverOutput> {
    let mut inventory_files = Vec::new();
    let mut merged: Option<FuzzTargetInventory> = None;

    for path in &args.inventories {
        let discovered = parse_fuzz_target_inventory_file(path)?;
        inventory_files.push(path.display().to_string());
        if let Some(base) = &mut merged {
            merge_fuzz_target_inventory(base, discovered);
        } else {
            merged = Some(discovered);
        }
    }

    let mut target_inventory = merged.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "inventory",
            "at least one --inventory artifact is required",
            None,
            None,
        )
    })?;
    if let Some(id) = args.inventory_id.as_deref() {
        target_inventory.id = id.trim().to_string();
    }
    target_inventory.provenance = Some(FuzzProvenance {
        schema: FUZZ_PROVENANCE_SCHEMA.to_string(),
        producer: "homeboy fuzz discover".to_string(),
        producer_version: None,
        invocation: Some("artifact-ingest".to_string()),
        run_id: None,
        source_ref: Some(args.source_label.clone()),
        created_at: None,
        metadata: json!({
            "inventory_files": inventory_files.clone(),
            "source_label": args.source_label.clone(),
            "discovery_mode": "artifact"
        }),
        extra: BTreeMap::new(),
    });

    Ok(FuzzDiscoverOutput {
        command: "fuzz.discover".to_string(),
        status: "ok".to_string(),
        source_label: args.source_label,
        inventory_files,
        summary: FuzzDiscoverSummary {
            surfaces: target_inventory.surfaces.len(),
            targets: target_inventory.targets.len(),
            workloads: target_inventory.workloads.len(),
            seeds: target_inventory.seeds.len(),
        },
        target_inventory,
    })
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
    let rig_context = load_rig(args.rig.as_deref(), &args.setting_args)?;
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
    let rig_context = load_rig(args.run.rig.as_deref(), &args.run.setting_args)?;
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
    let planning_metadata = plan_inventory_selection(&args, &target_inventory)?;

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
            metadata: planning_metadata,
            extra: std::collections::BTreeMap::new(),
        },
        runner_contract: default_runner_contract(),
    })
}

fn plan_inventory_selection(
    args: &FuzzPlanArgs,
    inventory: &FuzzTargetInventory,
) -> homeboy::core::Result<serde_json::Value> {
    let filters = operation_filters(args)?;
    let workload = args.run.workload_id.as_deref().and_then(|id| {
        inventory
            .workloads
            .iter()
            .find(|workload| workload.id == id)
    });
    let workload_surface_ids = workload
        .map(|workload| {
            workload
                .surface_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let workload_seed_ids = workload
        .map(|workload| workload.seed_ids.iter().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let workload_operations = workload
        .map(|workload| workload.operations.iter().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let surface_safety = inventory_surface_safety(inventory);

    let mut selected_target_ids = BTreeSet::new();
    let mut selected_families = BTreeSet::new();
    let mut selected_operations = Vec::new();
    let mut skipped_targets = Vec::new();
    let mut skipped_operations = Vec::new();
    let mut isolation_required = false;

    for target in &inventory.targets {
        let mut target_selected = false;
        let target_operations = target
            .operations
            .iter()
            .map(|operation| {
                (
                    operation,
                    operation.target_id.as_deref().unwrap_or(&target.id),
                )
            })
            .chain(inventory.surfaces.iter().flat_map(|surface| {
                surface.operations.iter().filter_map(|operation| {
                    let operation_target = operation
                        .target_id
                        .as_deref()
                        .or(surface.target.as_deref())?;
                    (operation_target == target.id).then_some((operation, operation_target))
                })
            }))
            .collect::<Vec<_>>();

        if target_operations.is_empty() {
            skipped_targets.push(json!({
                "id": target.id,
                "reason": "unsupported",
                "detail": "target declares no operations"
            }));
            continue;
        }

        for (operation, _) in target_operations {
            let safety_class = surface_safety
                .get(&target.id)
                .copied()
                .unwrap_or(FuzzSafetyClass::ReadOnly);
            let family = operation.family;
            let skip_reason = operation_skip_reason(
                operation,
                family,
                safety_class,
                args.strategy,
                &filters,
                &workload_operations,
            );

            if let Some(reason) = skip_reason {
                skipped_operations.push(json!({
                    "target_id": target.id,
                    "operation_id": operation.id,
                    "operation_kind": operation.kind,
                    "reason": reason,
                }));
                continue;
            }

            target_selected = true;
            selected_target_ids.insert(target.id.clone());
            if let Some(family) = family {
                selected_families.insert(operation_family_name(family));
                if matches!(
                    family,
                    FuzzOperationFamily::Create
                        | FuzzOperationFamily::Update
                        | FuzzOperationFamily::Delete
                        | FuzzOperationFamily::Submit
                ) {
                    isolation_required = true;
                }
            }
            if matches!(
                safety_class,
                FuzzSafetyClass::Idempotent | FuzzSafetyClass::IsolatedMutation
            ) {
                isolation_required = true;
            }
            selected_operations.push(json!({
                "target_id": target.id,
                "operation_id": operation.id,
                "operation_kind": operation.kind,
                "operation_family": family.map(operation_family_name),
            }));
        }

        if !target_selected {
            skipped_targets.push(json!({
                "id": target.id,
                "reason": "unsupported",
                "detail": "no operation matched the requested strategy or filters"
            }));
        }
    }

    let selected_seed_ids = if workload_seed_ids.is_empty() {
        inventory
            .seeds
            .iter()
            .map(|seed| seed.id.clone())
            .collect::<Vec<_>>()
    } else {
        inventory
            .seeds
            .iter()
            .filter(|seed| workload_seed_ids.contains(&seed.id))
            .map(|seed| seed.id.clone())
            .collect::<Vec<_>>()
    };
    let seed_refs = inventory
        .seeds
        .iter()
        .filter(|seed| selected_seed_ids.contains(&seed.id))
        .map(|seed| {
            json!({
                "id": seed.id,
                "kind": seed.kind,
                "artifact": seed.artifact,
                "has_inline_value": seed.value.is_some(),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "planner": {
            "strategy": args.strategy.as_str(),
            "operation_filters": args.operations,
            "operation_family_filters": args.operation_families,
        },
        "selection": {
            "target_ids": selected_target_ids.into_iter().collect::<Vec<_>>(),
            "operation_families": selected_families.into_iter().collect::<Vec<_>>(),
            "operations": selected_operations,
            "seed_ids": selected_seed_ids,
            "seed_refs": seed_refs,
        },
        "budgets": {
            "case_budget": args.case_budget.or_else(|| workload.and_then(|workload| workload.case_budget)),
            "duration_budget_seconds": args.duration_budget_seconds.or_else(|| workload.and_then(|workload| workload.duration_budget_seconds)),
            "max_duration": args.run.max_duration,
        },
        "isolation": {
            "required": isolation_required,
            "requirements": if isolation_required { vec!["isolated_mutation"] } else { Vec::<&str>::new() },
        },
        "required_artifact_ids": default_fuzz_required_artifacts().into_iter().map(|artifact| artifact.id).collect::<Vec<_>>(),
        "provenance": inventory.provenance,
        "skipped": {
            "targets": skipped_targets,
            "operations": skipped_operations,
        },
        "workload_scope": {
            "surface_ids": workload_surface_ids.into_iter().collect::<Vec<_>>(),
            "operation_filters": workload_operations.into_iter().collect::<Vec<_>>(),
        }
    }))
}

fn operation_filters(args: &FuzzPlanArgs) -> homeboy::core::Result<BTreeSet<String>> {
    args.operations
        .iter()
        .chain(args.operation_families.iter())
        .map(|filter| {
            let normalized = normalize_operation_filter(filter);
            if normalized.is_empty() {
                Err(homeboy::core::Error::validation_invalid_argument(
                    "operation",
                    "operation filters must be non-empty".to_string(),
                    Some(filter.clone()),
                    None,
                ))
            } else {
                Ok(normalized)
            }
        })
        .collect()
}

fn operation_skip_reason(
    operation: &FuzzOperation,
    family: Option<FuzzOperationFamily>,
    safety_class: FuzzSafetyClass,
    strategy: FuzzPlanStrategy,
    filters: &BTreeSet<String>,
    workload_operations: &BTreeSet<String>,
) -> Option<&'static str> {
    if matches!(safety_class, FuzzSafetyClass::Destructive) {
        return Some("destructive");
    }
    if family.is_none() {
        return Some("unsupported");
    }
    if !workload_operations.is_empty()
        && !workload_operations.contains(&operation.id)
        && !workload_operations.contains(&operation.kind)
        && !family
            .map(operation_family_name)
            .is_some_and(|name| workload_operations.contains(name))
    {
        return Some("unsupported");
    }
    if !filters.is_empty() && !operation_matches_filters(operation, family, filters) {
        return Some("unsupported");
    }
    if !strategy_matches_operation(strategy, family.expect("family checked above")) {
        return Some("unsupported");
    }
    None
}

fn strategy_matches_operation(strategy: FuzzPlanStrategy, family: FuzzOperationFamily) -> bool {
    match strategy {
        FuzzPlanStrategy::All | FuzzPlanStrategy::CoverageGaps => true,
        FuzzPlanStrategy::ReadOnly => matches!(
            family,
            FuzzOperationFamily::Read
                | FuzzOperationFamily::List
                | FuzzOperationFamily::Search
                | FuzzOperationFamily::Navigate
                | FuzzOperationFamily::Render
                | FuzzOperationFamily::Query
                | FuzzOperationFamily::Load
                | FuzzOperationFamily::BlockRender
        ),
        FuzzPlanStrategy::Crud => matches!(
            family,
            FuzzOperationFamily::Create | FuzzOperationFamily::Update | FuzzOperationFamily::Delete
        ),
    }
}

fn operation_matches_filters(
    operation: &FuzzOperation,
    family: Option<FuzzOperationFamily>,
    filters: &BTreeSet<String>,
) -> bool {
    filters.contains(&normalize_operation_filter(&operation.id))
        || filters.contains(&normalize_operation_filter(&operation.kind))
        || family
            .map(operation_family_name)
            .is_some_and(|name| filters.contains(name))
}

fn inventory_surface_safety(inventory: &FuzzTargetInventory) -> BTreeMap<String, FuzzSafetyClass> {
    let mut safety = BTreeMap::new();
    for surface in &inventory.surfaces {
        if let Some(target_id) = surface.target.as_ref() {
            safety.insert(target_id.clone(), surface.safety_class);
        }
        for operation in &surface.operations {
            if let Some(target_id) = operation.target_id.as_ref() {
                safety.insert(target_id.clone(), surface.safety_class);
            }
        }
    }
    safety
}

fn normalize_operation_filter(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn operation_family_name(family: FuzzOperationFamily) -> &'static str {
    match family {
        FuzzOperationFamily::Read => "read",
        FuzzOperationFamily::Create => "create",
        FuzzOperationFamily::Update => "update",
        FuzzOperationFamily::Delete => "delete",
        FuzzOperationFamily::List => "list",
        FuzzOperationFamily::Search => "search",
        FuzzOperationFamily::Navigate => "navigate",
        FuzzOperationFamily::Render => "render",
        FuzzOperationFamily::Query => "query",
        FuzzOperationFamily::Load => "load",
        FuzzOperationFamily::Submit => "submit",
        FuzzOperationFamily::BlockRender => "block_render",
        FuzzOperationFamily::PerformanceProbe => "performance_probe",
    }
}

#[cfg(test)]
mod tests {
    use super::super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
    use super::execution::{
        fuzz_campaign_contract, fuzz_evidence_followups, fuzz_run_outcome, fuzz_runner_env,
        persist_fuzz_run_evidence, FuzzRunEvidenceInput,
    };
    use super::replay::run_replay;
    use super::report::{
        evaluate_fuzz_gates, fuzz_coverage_completeness, gate_status, run_report, run_validate,
        FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND,
    };
    use super::types::{
        FuzzCommand, FuzzDiscoverArgs, FuzzExecutionOutput, FuzzListOutput, FuzzOutput,
        FuzzPlanArgs, FuzzPlanStrategy, FuzzReplayArgs, FuzzReportArgs, FuzzRunArgs, FuzzRunOutput,
        FuzzRunnerContract, FuzzValidateArgs, FuzzWorkloadOutput,
    };
    use super::workloads::{
        fuzz_workloads, resolve_component_id, rig_component_for_fuzz, select_workload,
        FuzzRigContext,
    };
    use super::{run_contract, FuzzArgs};
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
                max_duration: None,
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

    #[test]
    fn fuzz_discover_merges_inventory_artifacts_with_provenance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first.json");
        let second = temp.path().join("second.json");
        let mut second_inventory = planner_inventory();
        second_inventory.id = "second-inventory".to_string();
        second_inventory.targets[0].id = "api.orders".to_string();
        second_inventory.workloads.clear();
        second_inventory.seeds.clear();
        write_inventory(&first, &planner_inventory());
        write_inventory(&second, &second_inventory);

        let output = super::run_discover(FuzzDiscoverArgs {
            inventories: vec![first.clone(), second.clone()],
            inventory_id: Some("merged-inventory".to_string()),
            source_label: "unit-test".to_string(),
        })
        .expect("discover inventory");

        assert_eq!(output.command, "fuzz.discover");
        assert_eq!(output.status, "ok");
        assert_eq!(output.target_inventory.id, "merged-inventory");
        assert_eq!(output.summary.targets, 2);
        assert_eq!(output.summary.workloads, 1);
        assert_eq!(output.summary.seeds, 1);
        assert_eq!(
            output.inventory_files,
            vec![first.display().to_string(), second.display().to_string()]
        );
        let provenance = output.target_inventory.provenance.expect("provenance");
        assert_eq!(provenance.producer, "homeboy fuzz discover");
        assert_eq!(provenance.source_ref.as_deref(), Some("unit-test"));
        assert_eq!(
            provenance.metadata["discovery_mode"],
            serde_json::json!("artifact")
        );
    }

    #[test]
    fn fuzz_discover_requires_inventory_artifacts() {
        let error = super::run_discover(FuzzDiscoverArgs {
            inventories: Vec::new(),
            inventory_id: None,
            source_label: "artifact".to_string(),
        })
        .expect_err("empty discovery should fail");

        assert!(error
            .to_string()
            .contains("at least one --inventory artifact is required"));
    }

    #[test]
    fn fuzz_plan_selects_inventory_targets_operations_seeds_and_budgets() {
        let args = planner_args();
        let metadata = super::plan_inventory_selection(&args, &planner_inventory()).unwrap();

        assert_eq!(
            metadata["selection"]["target_ids"].as_array().unwrap(),
            &vec![serde_json::json!("api.users")]
        );
        assert_eq!(
            metadata["selection"]["operation_families"]
                .as_array()
                .unwrap(),
            &vec![serde_json::json!("create"), serde_json::json!("read")]
        );
        assert_eq!(
            metadata["selection"]["operations"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            metadata["selection"]["seed_ids"].as_array().unwrap(),
            &vec![serde_json::json!("seed-a")]
        );
        assert_eq!(metadata["budgets"]["case_budget"], serde_json::json!(25));
        assert_eq!(
            metadata["budgets"]["duration_budget_seconds"],
            serde_json::json!(120)
        );
        assert_eq!(metadata["isolation"]["required"], serde_json::json!(true));
    }

    #[test]
    fn fuzz_plan_operation_filter_limits_inventory_selection() {
        let mut args = planner_args();
        args.operations = vec!["read".to_string()];

        let metadata = super::plan_inventory_selection(&args, &planner_inventory()).unwrap();

        assert_eq!(
            metadata["selection"]["operation_families"]
                .as_array()
                .unwrap(),
            &vec![serde_json::json!("read")]
        );
        assert_eq!(
            metadata["selection"]["operations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            metadata["skipped"]["operations"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn fuzz_plan_budget_flags_override_inventory_workload_budgets() {
        let mut args = planner_args();
        args.case_budget = Some(5);
        args.duration_budget_seconds = Some(10);
        args.run.max_duration = Some("30s".to_string());

        let metadata = super::plan_inventory_selection(&args, &planner_inventory()).unwrap();

        assert_eq!(metadata["budgets"]["case_budget"], serde_json::json!(5));
        assert_eq!(
            metadata["budgets"]["duration_budget_seconds"],
            serde_json::json!(10)
        );
        assert_eq!(
            metadata["budgets"]["max_duration"],
            serde_json::json!("30s")
        );
    }

    #[test]
    fn fuzz_plan_skips_unsupported_inventory_operations() {
        let inventory = FuzzTargetInventory::from_value(serde_json::json!({
            "schema": "homeboy/fuzz-target-inventory/v1",
            "version": 1,
            "id": "component-a-inventory",
            "targets": [
                {
                    "schema": "homeboy/fuzz-target/v1",
                    "id": "custom.surface",
                    "kind": "api",
                    "operations": [
                        { "id": "custom.surface.invoke", "kind": "domain-specific" }
                    ]
                }
            ]
        }))
        .expect("inventory parses");

        let metadata = super::plan_inventory_selection(&planner_args(), &inventory).unwrap();

        assert!(metadata["selection"]["operations"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            metadata["skipped"]["operations"][0]["reason"],
            serde_json::json!("unsupported")
        );
        assert_eq!(metadata["skipped"]["targets"].as_array().unwrap().len(), 1);
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
    fn fuzz_validate_accepts_case_log_artifact() {
        let dir = tempfile::tempdir().expect("temp dir");
        let results_file = dir.path().join("fuzz-results.json");
        let case_log = dir.path().join("case-log.jsonl");
        std::fs::write(
            &results_file,
            serde_json::json!({
                "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
                "id": "campaign-1",
                "safety_class": "read_only",
                "coverage_summary": {
                    "declared_targets": 0,
                    "executable_targets": 0,
                    "proven_targets": 0,
                    "declared_operations": 0,
                    "executable_operations": 0,
                    "proven_operations": 0
                },
                "artifacts": [{
                    "id": "case-log",
                    "kind": "case_log"
                }]
            })
            .to_string(),
        )
        .expect("write campaign");
        std::fs::write(
            &case_log,
            r#"{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:abc","status":"passed","duration_ms":5}"#,
        )
        .expect("write case log");

        let output = run_validate(FuzzValidateArgs {
            results_file,
            case_logs: vec![case_log.clone()],
        })
        .expect("validate fuzz campaign and case log");

        assert_eq!(output.status, "passed");
        assert_eq!(output.case_log_entries, 1);
        assert_eq!(
            output.case_log_files,
            vec![case_log.to_string_lossy().to_string()]
        );
    }

    #[test]
    fn fuzz_validate_rejects_invalid_case_log_artifact() {
        let dir = tempfile::tempdir().expect("temp dir");
        let results_file = dir.path().join("fuzz-results.json");
        let case_log = dir.path().join("case-log.jsonl");
        std::fs::write(
            &results_file,
            serde_json::json!({
                "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
                "id": "campaign-1",
                "safety_class": "read_only"
            })
            .to_string(),
        )
        .expect("write campaign");
        std::fs::write(
            &case_log,
            r#"{"schema":"homeboy/fuzz-case-log/v1","case_id":"case-1","target_id":"target-1","operation_id":"operation-1","input_hash":"sha256:abc","status":"skipped","duration_ms":5}"#,
        )
        .expect("write invalid case log");

        let err = match run_validate(FuzzValidateArgs {
            results_file,
            case_logs: vec![case_log],
        }) {
            Ok(_) => panic!("invalid case log should fail validation"),
            Err(err) => err,
        };

        assert!(err.message.contains("skip_reason"));
    }

    #[test]
    fn fuzz_compare_parses_envelope_paths() {
        let cli = FuzzCli::parse_from([
            "fuzz",
            "compare",
            "baseline-envelope.json",
            "candidate-envelope.json",
        ]);

        match cli.args.command {
            Some(FuzzCommand::Compare(compare)) => {
                assert_eq!(compare.baseline, PathBuf::from("baseline-envelope.json"));
                assert_eq!(compare.candidate, PathBuf::from("candidate-envelope.json"));
            }
            _ => panic!("expected fuzz compare command"),
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
            coverage_summary: Some(zero_coverage_summary()),
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

        assert_eq!(gate_status(&gates), "passed");
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
            coverage_summary: Some(zero_coverage_summary()),
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
    fn fuzz_coverage_completeness_fails_closed_without_summary() {
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

        assert_eq!(gate_status(&gates), "failed");
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
        let override_env = homeboy::core::rig::expand::rig_component_path_override_env_name(
            "package-fuzz",
            "package",
        );
        let override_path = temp.path().join("runner/plugins/package");
        unsafe {
            std::env::set_var(&override_env, override_path.to_string_lossy().to_string());
        }

        let env = fuzz_runner_env(
            &args,
            Some(&context),
            Some(&workload),
            &results_path,
            &run_dir,
        )
        .expect("fuzz runner env");
        unsafe {
            std::env::remove_var(&override_env);
        }
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
            Some(override_path.to_string_lossy().as_ref())
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
    fn fuzz_run_outcome_fails_when_successful_command_reports_open_finding() {
        let mut campaign = empty_fuzz_campaign();
        campaign.findings = vec![FuzzFinding {
            schema: homeboy::core::fuzz::FUZZ_FINDING_SCHEMA.to_string(),
            id: "finding-1".to_string(),
            title: "runner surfaced a failing case".to_string(),
            severity: "high".to_string(),
            status: FuzzFindingStatus::Open,
            surface_id: None,
            target_id: None,
            operation_id: None,
            case_id: Some("case-1".to_string()),
            workload_id: None,
            seed_id: None,
            fingerprint: None,
            artifact_ids: Vec::new(),
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        }];

        let outcome = fuzz_run_outcome(0, true, Some(&campaign), None);

        assert_eq!(outcome.status, "failed");
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, 1);
    }

    #[test]
    fn fuzz_run_outcome_fails_when_successful_command_reports_failed_lifecycle_phase() {
        let mut campaign = empty_fuzz_campaign();
        campaign.lifecycle = Some(LifecycleResultMetadata {
            schema: LIFECYCLE_RESULT_SCHEMA.to_string(),
            version: LIFECYCLE_CONTRACT_VERSION,
            phases: vec![LifecyclePhaseResult {
                id: "prepare".to_string(),
                phase: LifecyclePhaseKind::Prepare,
                status: LifecyclePhaseStatus::Failed,
                snapshot_ref: None,
                started_at: None,
                finished_at: None,
                message: Some("runtime prepare failed".to_string()),
            }],
            snapshot_refs: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        });

        let outcome = fuzz_run_outcome(0, true, Some(&campaign), None);

        assert_eq!(outcome.status, "failed");
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, 1);
    }

    #[test]
    fn fuzz_run_outcome_fails_when_workload_reports_invariant_failure_count() {
        let mut campaign = empty_fuzz_campaign();
        campaign.metadata = serde_json::json!({
            "wordpress_fuzz_result": {
                "status": "passed",
                "success": true,
                "cases": [
                    {
                        "status": "passed",
                        "metadata": {
                            "observations": [
                                {
                                    "payload": {
                                        "metrics": {
                                            "side_effect_invariant_failure_count": 1
                                        }
                                    }
                                }
                            ]
                        }
                    }
                ]
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
            max_duration: None,
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
    fn fuzz_report_persists_result_envelope_artifact_for_run_id() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            seed_fuzz_run("report-run-1");
            let results_path = home.path().join("fuzz-campaign.json");
            std::fs::write(
                &results_path,
                serde_json::to_string(&empty_fuzz_campaign()).expect("serialize campaign"),
            )
            .expect("results file");

            let output = run_report(FuzzReportArgs {
                results_file: results_path,
                run: fuzz_run_args_with_run_id("report-run-1"),
                output_envelope: None,
                envelope_id: None,
            })
            .expect("fuzz report");

            assert_eq!(output.envelope_file, None);
            assert_eq!(output.envelope.id, "report-run-1");
            let store = ObservationStore::open_initialized().expect("store");
            let artifacts = store.list_artifacts("report-run-1").expect("artifacts");
            let envelope_artifact = artifacts
                .iter()
                .find(|artifact| artifact.kind == FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND)
                .expect("fuzz result envelope artifact");
            assert_eq!(envelope_artifact.artifact_type, "file");
            assert_eq!(envelope_artifact.mime.as_deref(), Some("application/json"));
            assert_eq!(
                envelope_artifact.metadata_json["envelope_id"],
                "report-run-1"
            );
            let persisted =
                std::fs::read_to_string(&envelope_artifact.path).expect("artifact file");
            assert!(persisted.contains(homeboy::core::fuzz::FUZZ_RESULT_ENVELOPE_SCHEMA));

            let artifact_index =
                homeboy::core::observation::evidence_report::evidence_artifact_index(&artifacts);
            assert_eq!(artifact_index.count, 1);
            assert_eq!(artifact_index.file_count, 1);
            assert_eq!(
                artifact_index.artifacts[0].kind,
                FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND
            );
            assert!(artifact_index.artifacts[0].fetch_command.is_some());
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn fuzz_report_fails_required_artifact_gate_when_replay_data_is_missing() {
        with_isolated_home(|home| {
            let results_path = home.path().join("fuzz-campaign.json");
            std::fs::write(
                &results_path,
                serde_json::to_string(&artifact_complete_fuzz_campaign())
                    .expect("serialize campaign"),
            )
            .expect("results file");

            let output = run_report(FuzzReportArgs {
                results_file: results_path,
                run: fuzz_run_args_with_run_id("report-run-missing-replay"),
                output_envelope: None,
                envelope_id: None,
            })
            .expect("fuzz report");

            assert_eq!(output.status, "failed");
            assert!(output.gates.iter().any(|gate| {
                gate.gate_id == "has-required-artifact-replay-data"
                    && gate.status == "failed"
                    && gate.observed == 0.0
            }));
            assert!(output.gates.iter().any(|gate| {
                gate.gate_id == "has-required-artifact-result-envelope" && gate.status == "passed"
            }));
            assert_eq!(output.envelope.status, output.status);
        });
    }

    #[test]
    fn fuzz_report_passes_required_artifact_gates_with_seed_replay_data() {
        with_isolated_home(|home| {
            let mut campaign = artifact_complete_fuzz_campaign();
            campaign.seeds = vec![homeboy::core::fuzz::FuzzSeed {
                schema: homeboy::core::fuzz::FUZZ_SEED_SCHEMA.to_string(),
                id: "seed-1".to_string(),
                kind: "literal".to_string(),
                label: None,
                value: Some("seed-value".to_string()),
                artifact: None,
                tags: Vec::new(),
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }];
            let results_path = home.path().join("fuzz-campaign.json");
            std::fs::write(
                &results_path,
                serde_json::to_string(&campaign).expect("serialize campaign"),
            )
            .expect("results file");

            let output = run_report(FuzzReportArgs {
                results_file: results_path,
                run: fuzz_run_args_with_run_id("report-run-with-replay"),
                output_envelope: None,
                envelope_id: None,
            })
            .expect("fuzz report");

            assert_eq!(output.status, "passed");
            assert!(output.gates.iter().any(|gate| {
                gate.gate_id == "has-required-artifact-replay-data"
                    && gate.status == "passed"
                    && gate.observed == 1.0
            }));
        });
    }

    #[test]
    fn fuzz_report_records_existing_output_envelope_path_as_artifact() {
        with_isolated_home(|home| {
            seed_fuzz_run("report-run-output");
            let results_path = home.path().join("fuzz-campaign.json");
            let envelope_path = home.path().join("fuzz-envelope.json");
            std::fs::write(
                &results_path,
                serde_json::to_string(&empty_fuzz_campaign()).expect("serialize campaign"),
            )
            .expect("results file");

            run_report(FuzzReportArgs {
                results_file: results_path,
                run: fuzz_run_args_with_run_id("report-run-output"),
                output_envelope: Some(envelope_path.clone()),
                envelope_id: Some("custom-envelope".to_string()),
            })
            .expect("fuzz report");

            assert!(envelope_path.is_file());
            let store = ObservationStore::open_initialized().expect("store");
            let artifacts = store
                .list_artifacts("report-run-output")
                .expect("artifacts");
            let envelope_artifact = artifacts
                .iter()
                .find(|artifact| artifact.kind == FUZZ_RESULT_ENVELOPE_ARTIFACT_KIND)
                .expect("fuzz result envelope artifact");
            assert_eq!(
                envelope_artifact.metadata_json["envelope_id"],
                "custom-envelope"
            );
            assert!(std::path::Path::new(&envelope_artifact.path).is_file());
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
            "--component",
            "component-a",
            "--case-id",
            "case-1",
            "--run-id",
            "proof-1",
            "--",
            "--runner-flag",
        ]);

        match cli.args.command {
            Some(FuzzCommand::Replay(replay)) => {
                assert_eq!(replay.component.as_deref(), Some("component-a"));
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
            component: None,
            path: None,
            rig: None,
            extension_override: ExtensionOverrideArgs::default(),
            setting_args: SettingArgs::default(),
            artifact_or_case: Some(path.to_string_lossy().to_string()),
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some("proof-1".to_string()),
            dry_run: true,
            args: vec!["--runner-flag".to_string()],
        })
        .expect("resolve replay");
        let (output, exit) = output;

        assert_eq!(exit, 0);
        assert_eq!(output.status, "dry_run");
        assert_eq!(output.campaign_id.as_deref(), Some("campaign-1"));
        assert_eq!(output.case_id.as_deref(), Some("case-1"));
        assert_eq!(
            output.replay.as_ref().map(|replay| replay.id.as_str()),
            Some("replay-1")
        );
        assert!(output.env.iter().any(|env| {
            env.name == "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE" && env.value == path.to_string_lossy()
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
    fn fuzz_replay_executes_manifest_replay_command_with_env() {
        with_isolated_home(|home| {
            let component_dir = tempfile::tempdir().expect("component dir");
            write_fuzz_extension(
                home.path(),
                "fixture-fuzz",
                Some(
                    r#"sh -c 'printf %s:%s:%s "$HOMEBOY_FUZZ_REPLAY_CASE_ID" "$HOMEBOY_FUZZ_REPLAY_SEED" "$1"' replay-runner {case}"#,
                ),
            );
            write_fuzz_rig(
                home,
                "fixture-rig",
                "component-a",
                component_dir.path(),
                "fixture-fuzz",
            );
            let path = write_replay_campaign(component_dir.path());

            let (output, exit) = run_replay(FuzzReplayArgs {
                component: Some("component-a".to_string()),
                path: None,
                rig: Some("fixture-rig".to_string()),
                extension_override: ExtensionOverrideArgs::default(),
                setting_args: SettingArgs::default(),
                artifact_or_case: Some(path.to_string_lossy().to_string()),
                artifact: None,
                case_id: Some("case-1".to_string()),
                run_id: Some("proof-1".to_string()),
                dry_run: false,
                args: vec!["--extra".to_string()],
            })
            .expect("execute replay");

            assert_eq!(exit, 0);
            assert_eq!(output.status, "passed");
            assert!(output.replay_command.as_deref().unwrap().contains("case-1"));
            assert!(output.env.iter().any(|env| {
                env.name == "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE"
                    && env.value == path.to_string_lossy()
            }));
            let execution = output.execution.expect("execution");
            assert_eq!(execution.extension_id, "fixture-fuzz");
            assert_eq!(execution.stdout, "case-1:1234:case-1");
        });
    }

    #[test]
    fn fuzz_replay_reports_unsupported_when_manifest_has_no_replay_command() {
        with_isolated_home(|home| {
            let component_dir = tempfile::tempdir().expect("component dir");
            write_fuzz_extension(home.path(), "fixture-fuzz", None);
            write_fuzz_rig(
                home,
                "fixture-rig",
                "component-a",
                component_dir.path(),
                "fixture-fuzz",
            );
            let path = write_replay_campaign(component_dir.path());

            let (output, exit) = run_replay(FuzzReplayArgs {
                component: Some("component-a".to_string()),
                path: None,
                rig: Some("fixture-rig".to_string()),
                extension_override: ExtensionOverrideArgs::default(),
                setting_args: SettingArgs::default(),
                artifact_or_case: Some(path.to_string_lossy().to_string()),
                artifact: None,
                case_id: Some("case-1".to_string()),
                run_id: Some("proof-1".to_string()),
                dry_run: false,
                args: Vec::new(),
            })
            .expect("resolve unsupported replay");

            assert_eq!(exit, 1);
            assert_eq!(output.status, "unsupported");
            assert!(output
                .message
                .contains("does not declare fuzz.replay_command"));
            assert!(output.execution.is_none());
        });
    }

    fn write_fuzz_extension(home: &Path, id: &str, replay_command: Option<&str>) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).expect("extension dir");
        let mut fuzz = serde_json::json!({
            "workloads": [{ "id": "replay-fixture" }]
        });
        if let Some(command) = replay_command {
            fuzz["replay_command"] = serde_json::Value::String(command.to_string());
        }
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": id,
                "version": "0.0.0",
                "fuzz": fuzz
            })
            .to_string(),
        )
        .expect("write fuzz extension manifest");
    }

    fn write_fuzz_rig(
        home: &tempfile::TempDir,
        rig_id: &str,
        component_id: &str,
        path: &Path,
        extension_id: &str,
    ) {
        let rig_dir = home.path().join(".config/homeboy/rigs");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join(format!("{rig_id}.json")),
            format!(
                r#"{{
                    "components": {{
                        "{component_id}": {{
                            "path": "{}",
                            "extensions": {{ "{extension_id}": {{}} }}
                        }}
                    }},
                    "fuzz": {{ "default_component": "{component_id}" }}
                }}"#,
                path.display()
            ),
        )
        .expect("write fuzz rig");
    }

    fn write_replay_campaign(dir: &Path) -> PathBuf {
        let path = dir.join("fuzz-results.json");
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
        fs::write(&path, serde_json::to_string(&campaign).unwrap()).expect("write campaign");
        path
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

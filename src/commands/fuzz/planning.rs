use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

use homeboy::core::fuzz::{
    default_fuzz_gates, default_fuzz_required_artifacts, FuzzExecutionRequest, FuzzOperation,
    FuzzOperationFamily, FuzzSafetyClass, FuzzTargetInventory, FUZZ_CONTRACT_VERSION,
    FUZZ_EXECUTION_REQUEST_SCHEMA,
};

use super::execution::default_runner_contract;
use super::types::{FuzzPlanArgs, FuzzPlanOutput, FuzzPlanStrategy};
use super::workloads::{
    build_target_inventory, fuzz_workloads, load_rig, resolve_component_id, resolve_fuzz_context,
    select_workload,
};
use homeboy::core::extension::ExtensionCapability;

pub(super) fn run_plan(args: FuzzPlanArgs) -> homeboy::core::Result<FuzzPlanOutput> {
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
pub(super) fn plan_inventory_selection(
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

use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

use homeboy::core::fuzz::{
    fuzz_gate_profile_contract, parse_fuzz_action_model_file, parse_fuzz_exploration_policy_file,
    FuzzExecutionRequest, FuzzOperation, FuzzOperationFamily, FuzzSafetyClass,
    FuzzSamplingCorpusRef, FuzzSamplingReplayDeterminism, FuzzSamplingRequest, FuzzSamplingStratum,
    FuzzTargetInventory, FUZZ_CONTRACT_VERSION, FUZZ_EXECUTION_REQUEST_SCHEMA,
    FUZZ_SAMPLING_REQUEST_SCHEMA,
};

use super::execution::fuzz_runner_contract;
use super::types::{FuzzPlanArgs, FuzzPlanOutput, FuzzPlanStrategy};
use super::workloads::{
    build_target_inventory, fuzz_workloads, load_rig, resolve_component_id, resolve_fuzz_context,
    select_workload,
};
use homeboy::core::extension::ExtensionCapability;

#[cfg(test)]
pub(super) const TEST_VERIFIED_FUZZ_ISOLATION_ENV: &str = "HOMEBOY_TEST_VERIFIED_FUZZ_ISOLATION";
pub(super) const RUNNER_HOSTED_EXEC_ENV: &str = "HOMEBOY_RUNNER_HOSTED_EXEC";

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
    let fuzz_config = ctx
        .extension_id
        .as_deref()
        .and_then(|extension_id| homeboy::core::extension::load_extension(extension_id).ok())
        .and_then(|manifest| manifest.fuzz);
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
    );
    let selected_workload = select_workload(&workloads, args.run.workload_id.as_deref())?;
    let workload_id = selected_workload
        .map(|workload| workload.id.clone())
        .or_else(|| args.run.workload_id.clone());
    let (required_artifacts, gates) = fuzz_gate_profile_contract(args.run.gate_profile.as_core());
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
    let sampling: FuzzSamplingRequest = serde_json::from_value(
        planning_metadata
            .get("sampling")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Null),
    )
    .map_err(|err| {
        homeboy::core::Error::validation_invalid_argument(
            "sampling",
            format!("invalid fuzz sampling request: {err}"),
            None,
            None,
        )
    })?;

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
            sampling,
            metadata: planning_metadata,
            extra: std::collections::BTreeMap::new(),
        },
        runner_contract: fuzz_runner_contract(fuzz_config.as_ref()),
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
    let workload_safety_class = workload.map(|workload| workload.safety_class);
    let surface_safety = inventory_surface_safety(inventory);
    let isolation_proof = verified_fuzz_isolation_proof();
    let destructive_allowed = args.run.allow_destructive
        && args.run.isolation.requests_isolation()
        && isolation_proof.verified;

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
            let surface_safety_class = surface_safety
                .get(&target.id)
                .copied()
                .unwrap_or(FuzzSafetyClass::ReadOnly);
            let safety_class = effective_safety_class(surface_safety_class, workload_safety_class);
            let family = operation.family;
            let skip_reason = operation_skip_reason(
                operation,
                family,
                safety_class,
                args.strategy,
                destructive_allowed,
                &filters,
                &workload_operations,
            );

            if let Some(reason) = skip_reason {
                skipped_operations.push(json!({
                    "target_id": target.id,
                    "operation_id": operation.id,
                    "operation_kind": operation.kind,
                    "reason": reason,
                    "detail": skip_reason_detail(reason, args, &isolation_proof),
                }));
                continue;
            }

            target_selected = true;
            selected_target_ids.insert(target.id.clone());
            if let Some(family) = family {
                selected_families.insert(operation_family_name(family).to_string());
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
        .map(|seed| FuzzSamplingCorpusRef {
            id: seed.id.clone(),
            kind: seed.kind.clone(),
            artifact: seed
                .artifact
                .as_ref()
                .and_then(|artifact| serde_json::to_value(artifact).ok()),
            has_inline_value: seed.value.is_some(),
        })
        .collect::<Vec<_>>();

    let (profile_artifacts, profile_gates) =
        fuzz_gate_profile_contract(args.run.gate_profile.as_core());
    let target_ids = selected_target_ids.into_iter().collect::<Vec<_>>();
    let operation_families = selected_families.into_iter().collect::<Vec<_>>();
    let case_budget = args
        .case_budget
        .or_else(|| workload.and_then(|workload| workload.case_budget));
    let duration_budget_seconds = args
        .duration_budget_seconds
        .or_else(|| workload.and_then(|workload| workload.duration_budget_seconds));
    let seed_source = if args.run.seed.is_some() {
        "caller"
    } else if selected_seed_ids.is_empty() {
        "none"
    } else {
        "corpus"
    };
    let replay_batch_id = args
        .run
        .run_id
        .clone()
        .or_else(|| args.run.workload_id.clone())
        .map(|id| format!("{id}-sampling"));
    let action_model = args
        .action_model
        .as_deref()
        .map(parse_fuzz_action_model_file)
        .transpose()?;
    let exploration_policy = args
        .exploration_policy
        .as_deref()
        .map(parse_fuzz_exploration_policy_file)
        .transpose()?;

    let sampling = FuzzSamplingRequest {
        schema: FUZZ_SAMPLING_REQUEST_SCHEMA.to_string(),
        strategy: args.strategy.as_str().to_string(),
        seed: args.run.seed.clone(),
        case_budget,
        duration_budget_seconds,
        target_strata: vec![FuzzSamplingStratum {
            id: "selected-targets".to_string(),
            kind: "target".to_string(),
            values: target_ids.clone(),
        }],
        operation_strata: vec![
            FuzzSamplingStratum {
                id: "selected-operation-families".to_string(),
                kind: "operation_family".to_string(),
                values: operation_families.clone(),
            },
            FuzzSamplingStratum {
                id: "selected-operations".to_string(),
                kind: "operation".to_string(),
                values: selected_operations
                    .iter()
                    .filter_map(|operation| operation.get("operation_id"))
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect(),
            },
        ],
        corpus_refs: seed_refs.clone(),
        replay: FuzzSamplingReplayDeterminism {
            deterministic: true,
            seed_source: seed_source.to_string(),
            replay_batch_id,
        },
        metadata: json!({
            "operation_filters": args.operations,
            "operation_family_filters": args.operation_families,
            "gate_profile": args.run.gate_profile.as_str(),
            "allow_destructive": args.run.allow_destructive,
            "isolation": args.run.isolation.as_str(),
            "destructive_allowed": destructive_allowed,
            "verified_isolation": isolation_proof.verified,
            "isolation_proof_source": isolation_proof.source,
        }),
        extra: BTreeMap::new(),
    };

    let mut metadata = json!({
        "planner": {
            "strategy": args.strategy.as_str(),
            "operation_filters": args.operations,
            "operation_family_filters": args.operation_families,
            "gate_profile": args.run.gate_profile.as_str(),
            "allow_destructive": args.run.allow_destructive,
            "isolation": args.run.isolation.as_str(),
            "destructive_allowed": destructive_allowed,
            "verified_isolation": isolation_proof.verified,
            "isolation_proof_source": isolation_proof.source,
        },
        "selection": {
            "target_ids": target_ids,
            "operation_families": operation_families,
            "operations": selected_operations,
            "seed_ids": selected_seed_ids,
            "seed_refs": seed_refs,
        },
        "budgets": {
            "case_budget": case_budget,
            "duration_budget_seconds": duration_budget_seconds,
            "max_duration": args.run.max_duration,
        },
        "sampling": sampling,
        "isolation": {
            "required": isolation_required,
            "mode": args.run.isolation.as_str(),
            "allow_destructive": args.run.allow_destructive,
            "destructive_allowed": destructive_allowed,
            "verified": isolation_proof.verified,
            "proof_source": isolation_proof.source,
            "requirements": if isolation_required { vec!["isolated_mutation"] } else { Vec::<&str>::new() },
        },
        "required_artifact_ids": profile_artifacts.into_iter().map(|artifact| artifact.id).collect::<Vec<_>>(),
        "gate_ids": profile_gates.into_iter().map(|gate| gate.id).collect::<Vec<_>>(),
        "provenance": inventory.provenance,
        "skipped": {
            "targets": skipped_targets,
            "operations": skipped_operations,
        },
        "workload_scope": {
            "surface_ids": workload_surface_ids.into_iter().collect::<Vec<_>>(),
            "operation_filters": workload_operations.into_iter().collect::<Vec<_>>(),
        }
    });

    if let Some(action_model) = action_model {
        metadata["action_model"] = serde_json::to_value(action_model).map_err(|err| {
            homeboy::core::Error::validation_invalid_argument(
                "action_model",
                format!("invalid fuzz action model contract: {err}"),
                None,
                None,
            )
        })?;
    }
    if let Some(exploration_policy) = exploration_policy {
        metadata["exploration_policy"] =
            serde_json::to_value(exploration_policy).map_err(|err| {
                homeboy::core::Error::validation_invalid_argument(
                    "exploration_policy",
                    format!("invalid fuzz exploration policy contract: {err}"),
                    None,
                    None,
                )
            })?;
    }

    Ok(metadata)
}

#[derive(Clone, Copy)]
struct FuzzIsolationProof {
    verified: bool,
    source: &'static str,
}

fn verified_fuzz_isolation_proof() -> FuzzIsolationProof {
    if lab_offload_metadata_verifies_isolation() {
        return FuzzIsolationProof {
            verified: true,
            source: "lab_offload_metadata",
        };
    }
    if std::env::var_os(RUNNER_HOSTED_EXEC_ENV).is_some() {
        return FuzzIsolationProof {
            verified: true,
            source: "runner_hosted_exec",
        };
    }
    #[cfg(test)]
    if std::env::var(TEST_VERIFIED_FUZZ_ISOLATION_ENV)
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
    {
        return FuzzIsolationProof {
            verified: true,
            source: "test_env",
        };
    }
    FuzzIsolationProof {
        verified: false,
        source: "none",
    }
}

fn lab_offload_metadata_verifies_isolation() -> bool {
    let Ok(raw) = std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV) else {
        return false;
    };
    let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let status = metadata.get("status").and_then(serde_json::Value::as_str);
    let remote_workspace = metadata
        .get("remote_workspace")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    matches!(status, Some("offloaded" | "success" | "completed")) || remote_workspace
}

fn effective_safety_class(
    surface: FuzzSafetyClass,
    workload: Option<FuzzSafetyClass>,
) -> FuzzSafetyClass {
    workload
        .map(|workload| max_safety_class(surface, workload))
        .unwrap_or(surface)
}

fn max_safety_class(a: FuzzSafetyClass, b: FuzzSafetyClass) -> FuzzSafetyClass {
    if safety_rank(a) >= safety_rank(b) {
        a
    } else {
        b
    }
}

fn safety_rank(class: FuzzSafetyClass) -> u8 {
    match class {
        FuzzSafetyClass::ReadOnly => 0,
        FuzzSafetyClass::Idempotent => 1,
        FuzzSafetyClass::IsolatedMutation => 2,
        FuzzSafetyClass::Destructive => 3,
    }
}

fn skip_reason_detail(
    reason: &str,
    args: &FuzzPlanArgs,
    isolation_proof: &FuzzIsolationProof,
) -> &'static str {
    if reason != "destructive" {
        return "operation is outside the selected strategy, filters, or supported operation families";
    }
    if !args.run.allow_destructive {
        return "destructive fuzz requires --allow-destructive";
    }
    if !args.run.isolation.requests_isolation() {
        return "destructive fuzz requires --isolation isolated";
    }
    if !isolation_proof.verified {
        return "destructive fuzz requires verified generic isolation proof from Lab/offloaded runner metadata";
    }
    "destructive fuzz is not allowed for this operation"
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
    destructive_allowed: bool,
    filters: &BTreeSet<String>,
    workload_operations: &BTreeSet<String>,
) -> Option<&'static str> {
    if matches!(safety_class, FuzzSafetyClass::Destructive) && !destructive_allowed {
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
        FuzzOperationFamily::PerformanceProbe => "performance_probe",
    }
}

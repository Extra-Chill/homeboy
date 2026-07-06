use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

use homeboy::core::fuzz::{
    fuzz_gate_profile_contract, parse_fuzz_action_model_file, parse_fuzz_exploration_policy_file,
    parse_fuzz_sequence_plan_file, FuzzExecutionRequest, FuzzOperation, FuzzOperationFamily,
    FuzzRequiredArtifact, FuzzSafetyClass, FuzzSamplingCorpusRef, FuzzSamplingReplayDeterminism,
    FuzzSamplingRequest, FuzzSamplingStratum, FuzzTargetInventory, IsolationProof,
    FUZZ_CONTRACT_VERSION, FUZZ_EXECUTION_REQUEST_SCHEMA, FUZZ_REQUIRED_ARTIFACT_SCHEMA,
    FUZZ_SAMPLING_REQUEST_SCHEMA, FUZZ_SEQUENCE_PLAN_SCHEMA,
};

use super::execution::fuzz_runner_contract;
use super::execution::run_run;
use super::types::{
    FuzzCampaignDispatchRecordOutput, FuzzCampaignRunOutput, FuzzPlanArgs, FuzzPlanOutput,
    FuzzPlanStrategy,
};
use super::types_extra::{
    FuzzCampaignPlanEntryOutput, FuzzCampaignPlanIsolationOutput, FuzzCampaignPlanOutput,
};
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
    let isolation_proof = load_or_default_isolation_proof(&args, &ctx.component_id)?;
    let sequence_plan = load_sequence_plan(args.run.sequence_plan.as_deref())?;
    let planning_metadata =
        plan_inventory_selection(&args, &target_inventory, isolation_proof.as_ref())?;
    let planning_metadata = with_sequence_plan_metadata(
        planning_metadata,
        args.run.sequence_plan.as_deref(),
        sequence_plan.as_ref(),
    )?;
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

    let request = FuzzExecutionRequest {
        schema: FUZZ_EXECUTION_REQUEST_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: request_id,
        component: ctx.component_id.clone(),
        rig_id: rig_id.clone(),
        workload_id,
        case_ids: Vec::new(),
        seed: args.run.seed.clone(),
        max_duration: args.run.max_duration.clone(),
        args: args.run.args.clone(),
        required_artifacts,
        gates,
        sampling,
        sequence_plan,
        isolation_proof,
        metadata: planning_metadata,
        extra: std::collections::BTreeMap::new(),
    };
    let campaign_plan = build_campaign_plan(&args, &ctx.component_id, rig_id.as_deref(), &request)?;

    Ok(FuzzPlanOutput {
        command: "fuzz.plan".to_string(),
        component: ctx.component_id.clone(),
        rig_id: rig_id.clone(),
        target_inventory,
        request,
        campaign_plan,
        runner_contract: fuzz_runner_contract(fuzz_config.as_ref()),
    })
}

pub(super) fn run_campaign(
    mut args: FuzzPlanArgs,
) -> homeboy::core::Result<(FuzzCampaignRunOutput, i32)> {
    if args.execute && args.dry_run {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "execute",
            "fuzz campaign execution cannot combine --execute and --dry-run".to_string(),
            None,
            None,
        ));
    }
    args.execute = args.execute || !args.dry_run;
    let plan_output = run_plan(args.clone())?;
    let Some(plan) = plan_output.campaign_plan else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "campaign-workload",
            "fuzz campaign execution requires --campaign-manifest or at least one --campaign-workload".to_string(),
            None,
            None,
        ));
    };

    let mut dispatch_records = Vec::new();
    let mut run_ids = Vec::new();
    let mut result_refs = Vec::new();
    let mut exit_code = 0;
    let store = if args.resume {
        Some(homeboy::core::observation::ObservationStore::open_initialized()?)
    } else {
        None
    };

    for entry in &plan.entries {
        run_ids.push(entry.run_id.clone());
        if let Some(store) = store.as_ref() {
            if store.get_run(&entry.run_id)?.is_some() {
                dispatch_records.push(campaign_dispatch_record(
                    entry,
                    "skipped_existing",
                    None,
                    None,
                    Vec::new(),
                    Some("resume skipped existing persisted run id".to_string()),
                ));
                continue;
            }
        }
        if !args.execute {
            dispatch_records.push(campaign_dispatch_record(
                entry,
                "planned",
                None,
                None,
                Vec::new(),
                Some("dry run; not executed".to_string()),
            ));
            continue;
        }

        let mut run_args = args.run.clone();
        run_args.workload_id = Some(entry.workload_id.clone());
        run_args.run_id = Some(entry.run_id.clone());
        let (run_output, entry_exit) = run_run(run_args)?;
        if entry_exit != 0 && exit_code == 0 {
            exit_code = entry_exit;
        }
        let entry_refs = run_output.evidence_refs.clone();
        result_refs.extend(entry_refs.iter().cloned());
        dispatch_records.push(campaign_dispatch_record(
            entry,
            &run_output.status,
            Some(entry_exit),
            entry_refs.first().map(|evidence| evidence.target.clone()),
            entry_refs,
            None,
        ));
    }

    let status = campaign_run_status(&dispatch_records, args.execute);
    Ok((
        FuzzCampaignRunOutput {
            command: if args.execute {
                "fuzz.run-campaign"
            } else {
                "fuzz.run-campaign.dry-run"
            }
            .to_string(),
            status: status.to_string(),
            execute: args.execute,
            dry_run: !args.execute,
            resume: args.resume,
            plan,
            dispatch_records,
            run_ids,
            result_refs,
            next_steps: campaign_next_steps(&status),
        },
        exit_code,
    ))
}

fn campaign_dispatch_record(
    entry: &FuzzCampaignPlanEntryOutput,
    status: &str,
    exit_code: Option<i32>,
    result_ref: Option<String>,
    evidence_refs: Vec<homeboy::core::artifact_ref::EvidenceRef>,
    message: Option<String>,
) -> FuzzCampaignDispatchRecordOutput {
    FuzzCampaignDispatchRecordOutput {
        index: entry.index,
        entry_id: entry.id.clone(),
        workload_id: entry.workload_id.clone(),
        run_id: entry.run_id.clone(),
        status: status.to_string(),
        command: entry.command.clone(),
        lab_runner: entry.lab_runner.clone(),
        tracker_refs: entry.tracker_refs.clone(),
        exit_code,
        result_ref,
        evidence_refs,
        message,
    }
}

fn campaign_run_status(
    records: &[FuzzCampaignDispatchRecordOutput],
    executed: bool,
) -> &'static str {
    if !executed {
        return "planned";
    }
    if records
        .iter()
        .any(|record| matches!(record.status.as_str(), "failed" | "timeout"))
    {
        "failed"
    } else if records
        .iter()
        .all(|record| record.status == "skipped_existing")
    {
        "skipped_existing"
    } else {
        "completed"
    }
}

fn campaign_next_steps(status: &str) -> Vec<String> {
    match status {
        "planned" => vec!["Run the campaign with `homeboy fuzz plan --execute ...` or `homeboy fuzz run-campaign ...`.".to_string()],
        "failed" => vec!["Inspect failed entry run ids with `homeboy runs show <run-id>` and `homeboy fuzz inspect <run-id>`.".to_string()],
        _ => vec!["Inspect persisted campaign entry evidence with `homeboy runs show <run-id>` and `homeboy runs evidence <run-id>`.".to_string()],
    }
}

const FUZZ_CAMPAIGN_PLAN_SCHEMA: &str = "homeboy/fuzz-campaign-plan/v1";

#[derive(Deserialize)]
struct FuzzCampaignManifest {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    workloads: Vec<FuzzCampaignManifestWorkload>,
    #[serde(default)]
    workload_ids: Vec<String>,
    #[serde(default)]
    lab_runner: Option<String>,
    #[serde(default)]
    required_artifacts: Vec<FuzzCampaignManifestArtifact>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FuzzCampaignManifestWorkload {
    Id(String),
    Object { id: String },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FuzzCampaignManifestArtifact {
    Id(String),
    Object { id: String, kind: Option<String> },
}

pub(super) fn build_campaign_plan(
    args: &FuzzPlanArgs,
    component: &str,
    rig_id: Option<&str>,
    base_request: &FuzzExecutionRequest,
) -> homeboy::core::Result<Option<FuzzCampaignPlanOutput>> {
    if args.campaign_manifest.is_none() && args.campaign_workloads.is_empty() {
        return Ok(None);
    }

    let manifest = load_campaign_manifest(args.campaign_manifest.as_deref())?;
    let mut workload_ids = manifest_workload_ids(manifest.as_ref());
    workload_ids.extend(args.campaign_workloads.iter().cloned());
    workload_ids.sort();
    workload_ids.dedup();
    if workload_ids.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "campaign-workload",
            "campaign planning requires at least one workload id".to_string(),
            None,
            None,
        ));
    }

    let manifest_artifacts = manifest
        .as_ref()
        .map(|manifest| manifest.required_artifacts.as_slice())
        .unwrap_or(&[]);
    let artifact_requirements = campaign_artifact_requirements(
        &base_request.required_artifacts,
        manifest_artifacts,
        &args.required_artifacts,
    );
    let lab_runner = args.lab_runner.clone().or_else(|| {
        manifest
            .as_ref()
            .and_then(|manifest| manifest.lab_runner.clone())
    });
    let campaign_id = args
        .request_id
        .clone()
        .or_else(|| manifest.as_ref().and_then(|manifest| manifest.id.clone()))
        .or_else(|| args.run.run_id.clone())
        .unwrap_or_else(|| format!("{component}-fuzz-campaign"));

    let entries = workload_ids
        .iter()
        .enumerate()
        .map(|(index, workload_id)| {
            let run_id = format!("{campaign_id}-{workload_id}");
            let mut request = base_request.clone();
            request.id = run_id.clone();
            request.workload_id = Some(workload_id.clone());
            request.required_artifacts = artifact_requirements.clone();
            request.metadata = campaign_entry_metadata(&request.metadata, &campaign_id, index);
            FuzzCampaignPlanEntryOutput {
                index,
                id: format!("{campaign_id}:{workload_id}"),
                workload_id: workload_id.clone(),
                run_id: run_id.clone(),
                lab_runner: lab_runner.clone(),
                tracker_refs: args.run.tracker_refs.clone(),
                artifact_requirements: artifact_requirements.clone(),
                command: campaign_run_command(args, component, workload_id, &run_id),
                request,
            }
        })
        .collect::<Vec<_>>();

    Ok(Some(FuzzCampaignPlanOutput {
        schema: FUZZ_CAMPAIGN_PLAN_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: campaign_id,
        component: component.to_string(),
        rig_id: rig_id.map(str::to_string),
        source_manifest: args
            .campaign_manifest
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        lab_runner,
        isolation: FuzzCampaignPlanIsolationOutput {
            mode: effective_isolation_mode(&args).to_string(),
            allow_destructive: args.run.allow_destructive,
            proof_required: args.run.allow_destructive || args.run.isolation.requests_isolation(),
            proof_file: args
                .run
                .isolation_proof
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
        },
        tracker_refs: args.run.tracker_refs.clone(),
        artifact_requirements,
        entries,
    }))
}

fn load_campaign_manifest(
    path: Option<&std::path::Path>,
) -> homeboy::core::Result<Option<FuzzCampaignManifest>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "campaign-manifest",
            format!("failed to read fuzz campaign manifest: {error}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    serde_json::from_str(&raw).map(Some).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "campaign-manifest",
            format!("invalid fuzz campaign manifest JSON: {error}"),
            Some(path.display().to_string()),
            None,
        )
    })
}

fn manifest_workload_ids(manifest: Option<&FuzzCampaignManifest>) -> Vec<String> {
    let Some(manifest) = manifest else {
        return Vec::new();
    };
    manifest
        .workloads
        .iter()
        .map(|workload| match workload {
            FuzzCampaignManifestWorkload::Id(id) => id.clone(),
            FuzzCampaignManifestWorkload::Object { id } => id.clone(),
        })
        .chain(manifest.workload_ids.iter().cloned())
        .filter_map(|id| {
            let trimmed = id.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

fn campaign_artifact_requirements(
    base: &[FuzzRequiredArtifact],
    manifest: &[FuzzCampaignManifestArtifact],
    extra: &[String],
) -> Vec<FuzzRequiredArtifact> {
    let mut artifacts = base.to_vec();
    artifacts.extend(manifest.iter().filter_map(|artifact| match artifact {
        FuzzCampaignManifestArtifact::Id(id) => required_artifact_from_id(id, None),
        FuzzCampaignManifestArtifact::Object { id, kind } => {
            required_artifact_from_id(id, kind.as_deref())
        }
    }));
    artifacts.extend(
        extra
            .iter()
            .filter_map(|id| required_artifact_from_id(id, None)),
    );
    artifacts.sort_by(|a, b| a.id.cmp(&b.id).then(a.kind.cmp(&b.kind)));
    artifacts.dedup_by(|a, b| a.id == b.id && a.kind == b.kind);
    artifacts
}

fn required_artifact_from_id(id: &str, kind: Option<&str>) -> Option<FuzzRequiredArtifact> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    Some(FuzzRequiredArtifact {
        schema: FUZZ_REQUIRED_ARTIFACT_SCHEMA.to_string(),
        id: id.to_string(),
        kind: kind.unwrap_or(id).trim().replace('-', "_"),
        required: true,
        description: None,
        acceptable_artifact_kinds: Vec::new(),
    })
}

fn campaign_entry_metadata(
    base: &serde_json::Value,
    campaign_id: &str,
    index: usize,
) -> serde_json::Value {
    let mut metadata = base.clone();
    if !metadata.is_object() {
        metadata = json!({});
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "campaign_plan".to_string(),
            json!({
                "schema": FUZZ_CAMPAIGN_PLAN_SCHEMA,
                "id": campaign_id,
                "entry_index": index,
            }),
        );
    }
    metadata
}

fn campaign_run_command(
    args: &FuzzPlanArgs,
    component: &str,
    workload_id: &str,
    run_id: &str,
) -> Vec<String> {
    let mut command = vec![
        "homeboy".to_string(),
        "fuzz".to_string(),
        "run".to_string(),
        component.to_string(),
    ];
    if let Some(rig) = args.run.rig.as_ref() {
        command.extend(["--rig".to_string(), rig.clone()]);
    }
    command.extend([
        "--workload".to_string(),
        workload_id.to_string(),
        "--run-id".to_string(),
        run_id.to_string(),
    ]);
    for tracker_ref in &args.run.tracker_refs {
        command.extend([
            "--tracker-ref".to_string(),
            format!("{}:{}", tracker_ref.kind, tracker_ref.id),
        ]);
    }
    if let Some(seed) = args.run.seed.as_ref() {
        command.extend(["--seed".to_string(), seed.clone()]);
    }
    if let Some(inventory) = args.run.inventory.as_ref() {
        command.extend([
            "--inventory".to_string(),
            inventory.to_string_lossy().to_string(),
        ]);
    }
    if let Some(sequence_plan) = args.run.sequence_plan.as_ref() {
        command.extend([
            "--sequence-plan".to_string(),
            sequence_plan.to_string_lossy().to_string(),
        ]);
    }
    command.extend([
        "--gate-profile".to_string(),
        args.run.gate_profile.as_str().to_string(),
    ]);
    if args.run.require_case_log {
        command.push("--require-case-log".to_string());
    }
    if args.run.require_coverage_summary {
        command.push("--require-coverage-summary".to_string());
    }
    if args.run.require_result_envelope {
        command.push("--require-result-envelope".to_string());
    }
    if let Some(max_duration) = args.run.max_duration.as_ref() {
        command.extend(["--max-duration".to_string(), max_duration.clone()]);
    }
    if args.run.allow_destructive {
        command.push("--allow-destructive".to_string());
    }
    if effective_isolation_requested(args) {
        command.extend([
            "--isolation".to_string(),
            effective_isolation_mode(args).to_string(),
        ]);
    }
    if let Some(isolation_proof) = args.run.isolation_proof.as_ref() {
        command.extend([
            "--isolation-proof".to_string(),
            isolation_proof.to_string_lossy().to_string(),
        ]);
    }
    if !args.run.args.is_empty() {
        command.push("--".to_string());
        command.extend(args.run.args.clone());
    }
    command
}

pub(super) fn load_sequence_plan(
    path: Option<&std::path::Path>,
) -> homeboy::core::Result<Option<homeboy::core::fuzz::FuzzSequencePlan>> {
    path.map(parse_fuzz_sequence_plan_file).transpose()
}

pub(super) fn with_sequence_plan_metadata(
    mut metadata: serde_json::Value,
    path: Option<&std::path::Path>,
    plan: Option<&homeboy::core::fuzz::FuzzSequencePlan>,
) -> homeboy::core::Result<serde_json::Value> {
    let Some(plan) = plan else {
        return Ok(metadata);
    };
    if !metadata.is_object() {
        metadata = serde_json::json!({});
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "sequence_plan_ref".to_string(),
            serde_json::json!({
                "schema": FUZZ_SEQUENCE_PLAN_SCHEMA,
                "id": plan.id,
                "path": path.map(|path| path.to_string_lossy().to_string()),
                "case_count": plan.cases.len(),
                "source": "--sequence-plan"
            }),
        );
    }
    Ok(metadata)
}

pub(super) fn plan_inventory_selection(
    args: &FuzzPlanArgs,
    inventory: &FuzzTargetInventory,
    isolation_proof: Option<&IsolationProof>,
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
    let isolation_requested = effective_isolation_requested(args);
    let destructive_allowed =
        args.run.allow_destructive && isolation_requested && isolation_proof.is_some();

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
                isolation_requested,
                &filters,
                &workload_operations,
            );

            if let Some(reason) = skip_reason {
                skipped_operations.push(json!({
                    "target_id": target.id,
                    "operation_id": operation.id,
                    "operation_kind": operation.kind,
                    "reason": reason,
                    "detail": skip_reason_detail(reason, args, isolation_proof),
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
        .run
        .action_model
        .as_deref()
        .map(parse_fuzz_action_model_file)
        .transpose()?;
    let exploration_policy = args
        .run
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
            "isolation": effective_isolation_mode(args),
            "destructive_allowed": destructive_allowed,
            "verified_isolation": isolation_proof.is_some(),
            "isolation_proof_schema": isolation_proof.map(|proof| proof.schema.as_str()),
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
            "isolation": effective_isolation_mode(args),
            "destructive_allowed": destructive_allowed,
            "verified_isolation": isolation_proof.is_some(),
            "isolation_proof_schema": isolation_proof.map(|proof| proof.schema.as_str()),
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
            "mode": effective_isolation_mode(args),
            "allow_destructive": args.run.allow_destructive,
            "destructive_allowed": destructive_allowed,
            "verified": isolation_proof.is_some(),
            "proof_schema": isolation_proof.map(|proof| proof.schema.as_str()),
            "runtime_kind": isolation_proof.map(|proof| proof.runtime_kind.as_str()),
            "mutation_boundary": isolation_proof.map(|proof| proof.mutation_boundary.as_str()),
            "verified_by": isolation_proof.map(|proof| proof.verified_by.as_str()),
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

pub(super) fn load_isolation_proof(
    path: Option<&std::path::Path>,
) -> homeboy::core::Result<Option<IsolationProof>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "isolation-proof",
            format!("failed to read isolation proof: {error}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "isolation-proof",
            format!("invalid isolation proof JSON: {error}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    IsolationProof::from_value(value)
        .map(Some)
        .map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "isolation-proof",
                error,
                Some(path.display().to_string()),
                None,
            )
        })
}

pub(super) fn load_or_default_isolation_proof(
    args: &FuzzPlanArgs,
    component_id: &str,
) -> homeboy::core::Result<Option<IsolationProof>> {
    if let Some(proof) = load_isolation_proof(args.run.isolation_proof.as_deref())? {
        return Ok(Some(proof));
    }
    if !args.run.allow_destructive {
        return Ok(None);
    }

    IsolationProof::from_value(serde_json::json!({
        "schema": homeboy::core::fuzz::ISOLATION_PROOF_SCHEMA,
        "version": homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
        "runtime_kind": "homeboy-fuzz-run-dir",
        "provider_ref": {
            "component": component_id,
            "source": "auto-generated"
        },
        "disposable": true,
        "snapshot_ref": "homeboy-run-dir",
        "reset_supported": true,
        "teardown_required": true,
        "mutation_boundary": "HOMEBOY fuzz runner declared mutation boundary",
        "proof_artifacts": [
            {
                "kind": "generated-contract",
                "ref": "homeboy:auto-isolation-proof"
            }
        ],
        "verified_by": "homeboy fuzz --allow-destructive",
        "metadata": {
            "reason": "--allow-destructive implies isolated fuzz mode for this fuzz run"
        }
    }))
    .map(Some)
    .map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "failed to build default fuzz isolation proof: {error}"
        ))
    })
}

pub(super) fn effective_isolation_requested(args: &FuzzPlanArgs) -> bool {
    args.run.allow_destructive || args.run.isolation.requests_isolation()
}

pub(super) fn effective_isolation_mode(args: &FuzzPlanArgs) -> &'static str {
    if args.run.allow_destructive || args.run.isolation.requests_isolation() {
        "isolated"
    } else {
        "shared"
    }
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
    isolation_proof: Option<&IsolationProof>,
) -> &'static str {
    if reason == "unsafe" {
        return "mutation-capable fuzz operations require --isolation isolated";
    }
    if reason != "destructive" {
        return "operation is outside the selected strategy, filters, or supported operation families";
    }
    if !args.run.allow_destructive {
        return "destructive fuzz requires --allow-destructive";
    }
    if isolation_proof.is_none() {
        return "destructive fuzz requires verified isolation proof";
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
    isolation_requested: bool,
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
    if requires_isolated_mutation(family, safety_class) && !isolation_requested {
        return Some("unsafe");
    }
    None
}

fn requires_isolated_mutation(
    family: Option<FuzzOperationFamily>,
    safety_class: FuzzSafetyClass,
) -> bool {
    matches!(
        family,
        Some(
            FuzzOperationFamily::Create
                | FuzzOperationFamily::Update
                | FuzzOperationFamily::Delete
                | FuzzOperationFamily::Submit
        )
    ) || matches!(
        safety_class,
        FuzzSafetyClass::Idempotent | FuzzSafetyClass::IsolatedMutation
    )
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

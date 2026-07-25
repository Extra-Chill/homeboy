use serde_json::json;
use std::collections::BTreeMap;

use homeboy::fuzz::{
    default_fuzz_gates, default_fuzz_required_artifacts, fuzz_core_contract,
    fuzz_gate_profile_contract, merge_fuzz_target_inventory, parse_fuzz_target_inventory_file,
    FuzzGateProfile, FuzzProvenance, FuzzTargetInventory, FUZZ_PROVENANCE_SCHEMA,
};
use homeboy_extension::ExtensionCapability;

use super::super::{CmdResult, GlobalArgs};
use super::compare::run_compare;
use super::doctor::run_doctor;
use super::execution::run_run;
use super::inspect::run_inspect;
use super::planning::{run_campaign, run_plan};
use super::replay::{run_minimize, run_replay};
use super::report::{run_report, run_validate};
use super::stable::run_stable;
use super::types::{
    FuzzArgs, FuzzCommand, FuzzContractOutput, FuzzDiscoverArgs, FuzzDiscoverOutput,
    FuzzDiscoverSummary, FuzzListArgs, FuzzListComponentMapping, FuzzListDiagnostics,
    FuzzListOutput, FuzzOutput,
};
use super::workloads::{fuzz_workloads, load_rig, resolve_component_id, resolve_fuzz_context};

pub fn run(args: FuzzArgs, _global: &GlobalArgs) -> CmdResult<FuzzOutput> {
    match args.command {
        Some(FuzzCommand::Contract) => Ok((FuzzOutput::Contract(run_contract()), 0)),
        Some(FuzzCommand::Doctor(doctor_args)) => {
            Ok((FuzzOutput::Doctor(run_doctor(doctor_args)?), 0))
        }
        Some(FuzzCommand::Discover(discover_args)) => {
            Ok((FuzzOutput::Discover(run_discover(discover_args)?), 0))
        }
        Some(FuzzCommand::List(list_args)) => Ok((FuzzOutput::List(run_list(list_args)?), 0)),
        Some(FuzzCommand::Plan(plan_args)) if plan_args.execute || plan_args.dry_run => {
            let (output, exit) = run_campaign(plan_args)?;
            Ok((FuzzOutput::RunCampaign(output), exit))
        }
        Some(FuzzCommand::Plan(plan_args)) => Ok((FuzzOutput::Plan(run_plan(plan_args)?), 0)),
        Some(FuzzCommand::Stable(stable_args)) => {
            Ok((FuzzOutput::StablePlan(run_stable(stable_args.command)?), 0))
        }
        Some(FuzzCommand::RunCampaign(mut plan_args)) => {
            if !plan_args.dry_run {
                plan_args.execute = true;
            }
            let (output, exit) = run_campaign(plan_args)?;
            Ok((FuzzOutput::RunCampaign(output), exit))
        }
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
        Some(FuzzCommand::Minimize(minimize_args)) => {
            let (output, exit) = run_minimize(minimize_args)?;
            Ok((FuzzOutput::Minimize(output), exit))
        }
        Some(FuzzCommand::Inspect(inspect_args)) => {
            let output = run_inspect(inspect_args)?;
            let exit = i32::from(output.inspection_status != "ok");
            Ok((FuzzOutput::Inspect(output), exit))
        }
        None => {
            let (output, exit) = run_run(args.run)?;
            Ok((FuzzOutput::Run(output), exit))
        }
    }
}

pub(super) fn run_discover(args: FuzzDiscoverArgs) -> homeboy::core::Result<FuzzDiscoverOutput> {
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

pub(super) fn run_contract() -> FuzzContractOutput {
    let mut gate_profiles = BTreeMap::new();
    for (name, profile) in [
        ("measurement", FuzzGateProfile::Measurement),
        ("evidence", FuzzGateProfile::Evidence),
        ("coverage-complete", FuzzGateProfile::CoverageComplete),
        ("strict", FuzzGateProfile::Strict),
    ] {
        let (required_artifacts, gates) = fuzz_gate_profile_contract(profile);
        gate_profiles.insert(
            name.to_string(),
            super::types::FuzzContractGateProfileOutput {
                required_artifacts,
                gates,
            },
        );
    }

    FuzzContractOutput {
        command: "fuzz.contract".to_string(),
        contract: fuzz_core_contract(),
        required_artifacts: default_fuzz_required_artifacts(),
        gates: default_fuzz_gates(),
        gate_profiles,
    }
}

pub(super) fn run_list(args: FuzzListArgs) -> homeboy::core::Result<FuzzListOutput> {
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
    let mut ambiguity = Vec::new();
    if let Some(context) = rig_context.as_ref() {
        let extensions = homeboy::rig::extension_ids_for_workloads(
            &context.spec,
            homeboy::rig::RigWorkloadKind::Fuzz,
        );
        if extensions.len() > 1 && ctx.extension_id.is_none() {
            ambiguity.push(format!(
                "rig declares multiple fuzz workload extensions: {}",
                extensions.join(", ")
            ));
        }
    }
    let mut workload_ids = workloads
        .iter()
        .map(|workload| workload.id.as_str())
        .collect::<Vec<_>>();
    workload_ids.sort_unstable();
    for duplicate in workload_ids
        .windows(2)
        .filter_map(|ids| (ids[0] == ids[1]).then_some(ids[0]))
    {
        ambiguity.push(format!("duplicate declared fuzz workload id: {duplicate}"));
    }
    let rig_package = rig_context
        .as_ref()
        .and_then(|context| homeboy::rig::package_evidence(&context.spec.id));
    if let Some(package) = rig_package
        .as_ref()
        .filter(|package| !package.freshness_verified)
    {
        ambiguity.push(format!(
            "rig package freshness is {:?}: {}",
            package.freshness,
            package
                .freshness_message
                .as_deref()
                .unwrap_or("source revision could not be verified")
        ));
    }

    Ok(FuzzListOutput {
        command: "fuzz.list".to_string(),
        component: ctx.component_id.clone(),
        rig_id: rig_context.map(|context| context.spec.id),
        rig_package,
        component_mapping: FuzzListComponentMapping {
            requested_component: args.comp.id().map(str::to_string),
            resolved_component: effective_id,
            path: ctx.component.local_path.clone(),
        },
        extension_provider: ctx.extension_id.clone(),
        count: workloads.len(),
        workloads,
        diagnostics: FuzzListDiagnostics {
            completeness: if ambiguity.is_empty() {
                "complete".to_string()
            } else if ambiguity
                .iter()
                .any(|message| message.starts_with("rig package freshness"))
            {
                "incomplete".to_string()
            } else {
                "ambiguous".to_string()
            },
            ambiguity,
            executable_availability: if args.remote_discovery {
                "remote_discovery_requested".to_string()
            } else {
                "not_requested".to_string()
            },
        },
        run_hint: "Select one workload with `homeboy fuzz run <component> --workload <id>`; offload heavy campaigns with the global `--runner <id>` flag when configured.".to_string(),
    })
}

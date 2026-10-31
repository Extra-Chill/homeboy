//! `homeboy workload`: run, list, compare, replay, minimize and inspect any
//! extension-declared workload kind (#15840).
//!
//! The CLI is a thin projection of `homeboy_core::extension::workload`. It
//! never names a kind: `--kind` is whatever an installed extension declares
//! under `workloads.<kind>`.

mod args;
mod output;

#[cfg(test)]
mod tests;

use homeboy_core::extension::workload::{
    self as engine,
    case::{CaseMode, CaseRequest},
    compare::{WorkloadAdapter, WorkloadComparison},
    list,
    resolve::{resolve_kind, validate_kind, ResolvedWorkloadKind, WorkloadTarget},
    run::WorkloadRunRequest,
    safety::{self, DestructiveRequest},
    stored, RigWorkloadFile,
};
use homeboy_core::observation::ObservationStore;
use homeboy_engine_primitives::workload_compare::compare;

pub use args::WorkloadArgs;
pub(crate) use args::{
    WorkloadCaseArgs, WorkloadCommand, WorkloadCompareArgs, WorkloadInspectArgs, WorkloadListArgs,
    WorkloadRunArgs, WorkloadTargetArgs,
};
pub use output::WorkloadOutput;
use output::{
    WorkloadCaseOutput, WorkloadCompareOutput, WorkloadInspectOutput, WorkloadListOutput,
    WorkloadRunOutput,
};

use super::CmdResult;

pub fn run(args: WorkloadArgs) -> CmdResult<WorkloadOutput> {
    match args.command {
        WorkloadCommand::List(args) => Ok((WorkloadOutput::List(run_list(args)?), 0)),
        WorkloadCommand::Run(args) => {
            let (output, exit) = run_run(args)?;
            Ok((WorkloadOutput::Run(output), exit))
        }
        WorkloadCommand::Compare(args) => {
            let output = run_compare(args)?;
            let exit = i32::from(output.comparison.regressed());
            Ok((WorkloadOutput::Compare(output), exit))
        }
        WorkloadCommand::Replay(args) => {
            let output = run_case(args, CaseMode::Replay)?;
            let exit = output.outcome.exit_code;
            Ok((WorkloadOutput::Replay(output), exit))
        }
        WorkloadCommand::Minimize(args) => {
            let output = run_case(args, CaseMode::Minimize)?;
            let exit = output.outcome.exit_code;
            Ok((WorkloadOutput::Minimize(output), exit))
        }
        WorkloadCommand::Inspect(args) => Ok((WorkloadOutput::Inspect(run_inspect(args)?), 0)),
    }
}

/// A resolved kind plus the rig context it resolved against, when one was
/// given: the rig owns out-of-tree workload files the engine reads as data.
struct ResolvedTarget {
    kind: ResolvedWorkloadKind,
    rig: Option<homeboy::rig::RigSourceContext>,
}

/// Resolve the kind's provider for a component or a rig component.
fn resolve_target(
    kind: &str,
    target: &WorkloadTargetArgs,
) -> homeboy::core::Result<ResolvedTarget> {
    validate_kind(kind)?;
    let mut resolved_target = WorkloadTarget {
        component_id: target.comp.component.clone(),
        path_override: target.comp.path.clone(),
        extension_overrides: target.extension_override.extensions.clone(),
        component_override: None,
    };
    let mut rig = None;
    if let Some(rig_id) = target.rig.as_deref() {
        let context =
            homeboy::rig::RigSourceContext::load(&homeboy::core::paths::homeboy()?, rig_id)?;
        let component_id = match target.comp.component.clone() {
            Some(id) => id,
            None => {
                let mut ids = context.spec.components.keys().cloned().collect::<Vec<_>>();
                if ids.len() != 1 {
                    ids.sort();
                    return Err(homeboy::core::Error::validation_invalid_argument(
                        "component",
                        format!(
                            "rig `{rig_id}` declares {} components; pass the component to run",
                            ids.len()
                        ),
                        None,
                        Some(ids),
                    ));
                }
                ids.remove(0)
            }
        };
        let mut component = homeboy::rig::resolve_component(&context.spec, &component_id)?;
        if let Some(extensions) = context
            .spec
            .components
            .get(&component_id)
            .and_then(|rig_component| rig_component.extensions.clone())
        {
            component.extensions = Some(extensions);
        }
        if resolved_target.path_override.is_none() {
            resolved_target.path_override = Some(component.local_path.clone());
        }
        resolved_target.component_id = Some(component_id);
        resolved_target.component_override = Some(component);
        rig = Some(context);
    }
    Ok(ResolvedTarget {
        kind: resolve_kind(kind, resolved_target)?,
        rig,
    })
}

/// The rig's out-of-tree workload files for this kind and its owning
/// extension: id = file stem, the rule the legacy fuzz command uses.
///
/// Resolved here, in the CLI, because `homeboy-rig` depends on
/// `homeboy-core` and the engine must stay rig-agnostic.
fn rig_workload_files(resolved: &ResolvedTarget) -> Vec<RigWorkloadFile> {
    let Some(rig) = resolved.rig.as_ref() else {
        return Vec::new();
    };
    homeboy::rig::workload_path_expansions_for_kind(
        &rig.spec,
        &resolved.kind.kind,
        rig.package_root.as_deref(),
        resolved.kind.extension_id(),
    )
    .into_iter()
    .map(|expansion| RigWorkloadFile {
        id: expansion
            .expanded_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("rig-workload")
            .to_string(),
        path: expansion.expanded_path,
    })
    .collect()
}

fn json_settings(
    target: &WorkloadTargetArgs,
) -> homeboy::core::Result<Vec<(String, serde_json::Value)>> {
    let mut settings = target.setting_args.settings_profile_json_overrides()?;
    settings.extend(target.setting_args.settings_json_overrides()?);
    Ok(settings)
}

fn destructive_request(allow: bool, proof: Option<&std::path::Path>) -> DestructiveRequest {
    DestructiveRequest {
        allow_destructive: allow,
        isolation_proof: proof.map(std::path::Path::to_path_buf),
    }
}

fn run_list(args: WorkloadListArgs) -> homeboy::core::Result<WorkloadListOutput> {
    let resolved = resolve_target(&args.kind, &args.target)?;
    let rig_files = rig_workload_files(&resolved);
    let mut workloads = list::declared_workloads(&resolved.kind);
    if let Some(rig) = resolved.rig.as_ref() {
        workloads.extend(list::rig_workloads(&rig_files, &rig.spec.id));
    }
    let mut diagnostics = Vec::new();
    if list::has_script_probe(&resolved.kind) {
        if args.no_script_probe {
            diagnostics.push("script list probe skipped (--no-script-probe)".to_string());
        } else {
            workloads.extend(list::script_workloads(
                &resolved.kind,
                &args.target.setting_args.setting,
                &json_settings(&args.target)?,
            )?);
        }
    }
    let duplicates = list::duplicate_ids(&workloads);
    diagnostics.extend(
        duplicates
            .iter()
            .map(|id| format!("workload id `{id}` is declared more than once")),
    );
    Ok(WorkloadListOutput {
        command: "workload.list".to_string(),
        kind: resolved.kind.kind.clone(),
        component: resolved.kind.context.component_id.clone(),
        extension_id: resolved.kind.extension_id().to_string(),
        count: workloads.len(),
        workloads,
        capabilities: output::capabilities(&resolved.kind.config),
        diagnostics,
    })
}

fn run_command_label(args: &WorkloadRunArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "workload".to_string(),
        "run".to_string(),
        "--kind".to_string(),
        args.kind.clone(),
    ];
    if let Some(component) = &args.target.comp.component {
        parts.push(component.clone());
    }
    if let Some(rig) = &args.target.rig {
        parts.extend(["--rig".to_string(), rig.clone()]);
    }
    if let Some(workload) = &args.workload {
        parts.extend(["--workload".to_string(), workload.clone()]);
    }
    homeboy::core::engine::shell::quote_args(&parts)
}

fn run_run(args: WorkloadRunArgs) -> homeboy::core::Result<(WorkloadRunOutput, i32)> {
    let resolved = resolve_target(&args.kind, &args.target)?;
    let rig_files = rig_workload_files(&resolved);
    // The selected rig workload file can raise the kind's safety class; the
    // gate refuses before any run row exists.
    let effective_safety = engine::safety::effective_safety_class(
        resolved.kind.config.safety_class,
        engine::selected_rig_workload(&rig_files, args.workload.as_deref())
            .map(|file| file.path.as_path()),
    );
    let authorization = safety::authorize(
        &resolved.kind.kind,
        effective_safety.class,
        &destructive_request(args.allow_destructive, args.isolation_proof.as_deref()),
    )?;
    let timeout = args
        .max_duration
        .as_deref()
        .map(|raw| homeboy::core::duration::parse_duration("max_duration", raw))
        .transpose()?;
    let settings_json = json_settings(&args.target)?;
    let outcome = engine::run::run(WorkloadRunRequest {
        resolved: &resolved.kind,
        workload_id: args.workload.as_deref(),
        run_id: args.run_id.as_deref(),
        seed: args.seed.as_deref(),
        rig_id: args.target.rig.as_deref(),
        rig_workloads: &rig_files,
        settings: &args.target.setting_args.setting,
        settings_json: &settings_json,
        script_args: &args.args,
        timeout,
        authorization: authorization.as_ref(),
        baseline: homeboy_engine_primitives::baseline::BaselineFlags {
            baseline: args.baseline_args.baseline,
            ignore_baseline: args.baseline_args.ignore_baseline,
            ratchet: args.baseline_args.ratchet,
        },
        command: run_command_label(&args),
    })?;
    let exit = outcome.exit_code;
    Ok((
        WorkloadRunOutput {
            command: "workload.run".to_string(),
            kind: resolved.kind.kind.clone(),
            component: resolved.kind.context.component_id.clone(),
            extension_id: resolved.kind.extension_id().to_string(),
            safety_class: effective_safety.class.as_str().to_string(),
            hints: output::run_hints(&resolved.kind.kind, &outcome),
            failure: output::run_failure(&resolved.kind.kind, &outcome),
            outcome,
        },
        exit,
    ))
}

fn run_compare(args: WorkloadCompareArgs) -> homeboy::core::Result<WorkloadCompareOutput> {
    validate_kind(&args.kind)?;
    let store = ObservationStore::open_initialized()?;
    let from = stored::load(&store, &args.from_run, Some(&args.kind))?;
    let to = stored::load(&store, &args.to_run, Some(&args.kind))?;
    let config = to.compare_config();
    let compared = compare::<WorkloadAdapter>(&from.result, &to.result, &config)?;
    let comparison = WorkloadComparison::new(compared.delta);
    let failure = comparison.regressed().then(|| output::WorkloadFailure {
        code: "workload.compare_regressed",
        message: format!(
            "`{}` run {} regressed against {}: {}",
            args.kind,
            to.run.id,
            from.run.id,
            comparison.regressions.join("; ")
        ),
    });
    Ok(WorkloadCompareOutput {
        command: "workload.compare".to_string(),
        kind: args.kind,
        from_run: from.run.id,
        to_run: to.run.id,
        compare: config,
        comparison,
        failure,
    })
}

fn run_case(args: WorkloadCaseArgs, mode: CaseMode) -> homeboy::core::Result<WorkloadCaseOutput> {
    validate_kind(&args.kind)?;
    let store = ObservationStore::open_initialized()?;
    let source = stored::load(&store, &args.run_id, Some(&args.kind))?;
    // Default to the component and checkout the source run executed against.
    let mut target = args.target.clone();
    if target.comp.component.is_none() && target.rig.is_none() {
        target.comp.component = source.run.component_id.clone();
    }
    if target.comp.path.is_none() && target.rig.is_none() {
        target.comp.path = source.source_path().map(str::to_string);
    }
    if target.extension_override.extensions.is_empty() {
        if let Some(extension_id) = source.extension_id() {
            target.extension_override.extensions = vec![extension_id.to_string()];
        }
    }
    let resolved = resolve_target(&args.kind, &target)?;
    let rig_files = rig_workload_files(&resolved);
    // Reuse the source run's recorded effective safety class: a workload
    // file that raised it at run time keeps the gate engaged here.
    let effective_safety = source.recorded_safety_class()?;
    let authorization = if args.dry_run {
        None
    } else {
        safety::authorize(
            &resolved.kind.kind,
            effective_safety,
            &destructive_request(args.allow_destructive, args.isolation_proof.as_deref()),
        )?
    };
    let settings_json = json_settings(&target)?;
    let timeout = args
        .max_duration
        .as_deref()
        .map(|raw| homeboy::core::duration::parse_duration("max_duration", raw))
        .transpose()?;
    let outcome = engine::case::execute(CaseRequest {
        mode,
        resolved: &resolved.kind,
        source: &source,
        case_id: &args.case_id,
        rig_workloads: &rig_files,
        settings: &target.setting_args.setting,
        settings_json: &settings_json,
        script_args: &args.args,
        timeout,
        authorization: authorization.as_ref(),
        dry_run: args.dry_run,
    })?;
    let failure = (outcome.exit_code != 0).then(|| output::WorkloadFailure {
        code: "workload.case_failed",
        message: {
            let mut message = format!(
                "{} of case {} from run {} exited {}",
                mode.as_str(),
                outcome.case_id,
                outcome.source_run_id,
                outcome.exit_code
            );
            if let Some(line) = outcome
                .stderr_tail
                .as_deref()
                .and_then(|stderr| stderr.lines().rev().find(|line| !line.trim().is_empty()))
            {
                message.push_str(": ");
                message.push_str(line.trim());
            }
            message
        },
    });
    Ok(WorkloadCaseOutput {
        command: format!("workload.{}", mode.as_str()),
        extension_id: resolved.kind.extension_id().to_string(),
        outcome,
        failure,
    })
}

fn run_inspect(args: WorkloadInspectArgs) -> homeboy::core::Result<WorkloadInspectOutput> {
    if let Some(kind) = args.kind.as_deref() {
        validate_kind(kind)?;
    }
    let store = ObservationStore::open_initialized()?;
    let stored = stored::load(&store, &args.run_id, args.kind.as_deref())?;
    let findings = store.list_findings_for_run(&stored.run.id)?;
    Ok(output::inspect_output(stored, findings, args.full))
}

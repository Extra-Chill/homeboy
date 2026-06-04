use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use homeboy::core::component::{Component, ScopedExtensionConfig};
use homeboy::core::engine::baseline::BaselineFlags;
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::{
    TraceAttachment, TraceCanonicalPolicy, TraceCommandOutput, TraceListWorkflowArgs,
    TraceOverlayRequest, TraceRunWorkflowArgs, TraceRunnerInputs, TraceSpanDefinition,
};
use homeboy::core::extension::ExtensionCapability;
use homeboy::core::observation::{
    NewRunRecord, NewTraceRunRecord, NewTraceSpanRecord, ObservationStore, RunStatus,
};
use homeboy::core::rig::{self, RigSpec};

use super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

mod aggregate;
#[cfg(test)]
mod aggregate_tests;
mod bundle;
mod compare_targets;
mod compare_variant;
mod experiment;
mod guardrails;
mod matrix;
mod metadata;
mod observations;
mod output;
mod overlay_locks;
mod probes;
#[cfg(test)]
mod profile_tests;
pub(super) mod repeat;
mod schedule;
#[cfg(test)]
mod test_fixture;

use compare_targets::run_compare_targets;
use compare_variant::run_compare_variant;
use experiment::{
    collect_trace_experiment_artifacts_for_plan, run_trace_experiment_setup_for_plan,
    run_trace_experiment_teardown_for_plan, trace_experiment_env, trace_experiment_plan_for_args,
    trace_experiment_settings,
};
use guardrails::run_trace_guardrails_for_args;
use matrix::TraceMatrixAxis;
use metadata::trace_span_metadata_for_args;
use observations::record_trace_artifacts;
use overlay_locks::run_overlay_locks;

#[cfg(test)]
use output::render_aggregate_markdown;
use output::{
    attach_span_metadata, classification_summaries, render_matrix_markdown,
    render_scenario_matrix_markdown, render_trace_aggregate_evidence_markdown,
    render_trace_compare_evidence_markdown, render_trace_run_evidence_markdown, run_compare,
};
use probes::trace_probes_for_args;
use repeat::run_repeat;
pub(super) use schedule::{
    plan_trace_run_order, TraceRunPlanEntry, TraceSchedule, TraceVariantMatrixMode,
};

#[cfg(test)]
use matrix::{expand_variant_matrix, TraceVariantStackItem};
#[derive(Args, Clone)]
pub struct TraceArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,
    /// Target component for command-shaped trace modes like `compare-variant`.
    #[arg(long = "component", value_name = "COMPONENT_ID")]
    pub component_arg: Option<String>,
    /// Scenario ID to run, or `list` to discover available scenarios.
    pub scenario: Option<String>,
    /// Scenario ID for command-shaped trace modes like `compare-variant`.
    #[arg(long = "scenario", value_name = "SCENARIO_ID")]
    pub scenario_arg: Option<String>,
    /// After aggregate JSON when running `homeboy trace compare before.json after.json`.
    #[arg(value_name = "AFTER_JSON")]
    pub compare_after: Option<PathBuf>,
    /// Baseline path or git ref for `homeboy trace compare COMPONENT SCENARIO`.
    #[arg(long = "baseline-target", value_name = "PATH_OR_REF")]
    pub baseline_target: Option<String>,
    /// Candidate path or git ref for `homeboy trace compare COMPONENT SCENARIO`.
    #[arg(long, value_name = "PATH_OR_REF")]
    pub candidate: Option<String>,
    /// Run trace against a rig-pinned component path after `rig check` passes.
    #[arg(long, value_name = "RIG_ID")]
    pub rig: Option<String>,
    /// Use a named trace profile declared by a rig.
    #[arg(long, value_name = "PROFILE_ID")]
    pub profile: Option<String>,
    /// With `trace list`, list named trace profiles instead of scenarios.
    #[arg(long = "profiles")]
    pub profiles: bool,
    #[command(flatten)]
    pub setting_args: SettingArgs,
    /// Print compact machine-readable summary.
    #[arg(long)]
    pub json_summary: bool,

    /// Render a Markdown trace report instead of the JSON envelope.
    #[arg(long, value_parser = ["markdown"])]
    pub report: Option<String>,

    /// Bundle trace compare inputs, output, report, and overlay metadata under .homeboy/experiments/NAME.
    #[arg(long, value_name = "NAME")]
    pub experiment: Option<String>,

    /// Run the same trace scenario multiple times.
    #[arg(long, alias = "runs", value_name = "N", default_value_t = 1)]
    pub repeat: usize,

    /// Aggregate repeated trace output.
    #[arg(long, value_parser = ["spans"])]
    pub aggregate: Option<String>,

    /// Run order for repeated trace executions.
    #[arg(long, value_enum, default_value_t = TraceSchedule::Grouped)]
    pub schedule: TraceSchedule,

    /// Highlight a span in aggregate and compare reports. Repeatable.
    #[arg(long = "focus-span", value_name = "SPAN_ID")]
    pub focus_spans: Vec<String>,

    /// Add a span definition as `id:source.event:source.event`.
    #[arg(long = "span", value_name = "ID:FROM:TO", value_parser = extension_trace::spans::parse_span_definition)]
    pub spans: Vec<TraceSpanDefinition>,

    /// Add an ordered phase milestone as `[label:]source.event`.
    #[arg(long = "phase", value_name = "[LABEL:]SOURCE.EVENT", value_parser = extension_trace::spans::parse_phase_milestone)]
    pub phases: Vec<extension_trace::spans::TracePhaseMilestone>,

    /// Observe an already-running local target without managing its lifecycle. Repeatable.
    #[arg(long = "attach", value_name = "KIND:TARGET")]
    pub attachments: Vec<String>,

    /// Use a named phase preset declared by the selected rig/workload.
    #[arg(long = "phase-preset", value_name = "NAME")]
    pub phase_preset: Option<String>,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    /// Span regression tolerance as a percentage.
    #[arg(long, value_name = "PERCENT", default_value_t = extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT)]
    pub regression_threshold: f64,

    /// Minimum span slowdown in milliseconds before a regression can fail.
    #[arg(long, value_name = "MS", default_value_t = extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS)]
    pub regression_min_delta_ms: u64,

    /// Apply a patch file for this trace run, then reverse it afterward.
    #[arg(long = "overlay", value_name = "PATCH_FILE")]
    pub overlays: Vec<String>,

    /// Apply a named trace variant declared by the selected rig/workload.
    #[arg(long = "variant", value_name = "NAME")]
    pub variants: Vec<String>,

    /// Expand variants for `trace compare-variant`.
    #[arg(long = "matrix", value_enum, default_value_t = TraceVariantMatrixMode::None)]
    pub matrix: TraceVariantMatrixMode,

    /// Add a scenario matrix axis as `name=value1,value2`. Repeatable.
    #[arg(long = "axis", value_name = "NAME=VALUE[,VALUE...]", value_parser = matrix::parse_trace_matrix_axis)]
    pub axes: Vec<TraceMatrixAxis>,

    #[arg(skip)]
    pub matrix_env: Vec<(String, String)>,

    /// Directory where trace matrix modes write aggregate, compare, cell, and summary artifacts.
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Leave overlay changes in place after the trace run.
    #[arg(long)]
    pub keep_overlay: bool,

    /// Require canonical proof inputs and refuse dirty, stale, or arbitrary local toolchain state.
    #[arg(long, alias = "proof")]
    pub canonical: bool,

    /// Permit local toolchain overrides for development traces and mark evidence non-canonical.
    #[arg(long)]
    pub allow_local_toolchain: bool,

    /// Clean only stale trace overlay locks.
    #[arg(long)]
    pub stale: bool,

    /// Remove stale trace overlay locks even when touched files are dirty.
    #[arg(long)]
    pub force: bool,
}

pub fn is_markdown_mode(args: &TraceArgs) -> bool {
    args.report.as_deref() == Some("markdown")
}

pub fn run_markdown(args: TraceArgs, global: &GlobalArgs) -> CmdResult<String> {
    let (output, exit_code) = run(args, global)?;
    Ok((render_markdown_output(&output), exit_code))
}

pub fn run_markdown_with_json_artifact(
    args: TraceArgs,
    _global: &GlobalArgs,
) -> super::raw_output::RawCommandRun {
    let output_to_json = |output: &TraceCommandOutput| {
        serde_json::to_value(output).map_err(|err| {
            homeboy::core::Error::internal_json(
                err.to_string(),
                Some("serialize response".to_string()),
            )
        })
    };

    match run_outputs(args) {
        Ok(((stdout_output, artifact_output), exit_code)) => {
            let markdown = render_markdown_output(&stdout_output);
            super::raw_output::RawCommandRun {
                stdout_result: Ok(markdown),
                exit_code,
                output_file_result: Some(match artifact_output {
                    Some(ref output) => output_to_json(output),
                    None => output_to_json(&stdout_output),
                }),
            }
        }
        Err(err) => super::raw_output::RawCommandRun {
            stdout_result: Err(err),
            exit_code: 1,
            output_file_result: None,
        },
    }
}

fn render_markdown_output(output: &TraceCommandOutput) -> String {
    match output {
        TraceCommandOutput::Run(run_output) => render_trace_run_evidence_markdown(run_output),
        TraceCommandOutput::Summary(summary) => {
            format!(
                "# Trace Summary\n\n- **Component:** `{}`\n- **Status:** `{}`\n- **Exit code:** `{}`\n",
                summary.component, summary.status, summary.exit_code
            )
        }
        TraceCommandOutput::Aggregate(aggregate) => {
            render_trace_aggregate_evidence_markdown(aggregate)
        }
        TraceCommandOutput::Compare(compare) => render_trace_compare_evidence_markdown(compare),
        TraceCommandOutput::Matrix(matrix) => render_matrix_markdown(matrix),
        TraceCommandOutput::ScenarioMatrix(matrix) => render_scenario_matrix_markdown(matrix),
        TraceCommandOutput::List(list) => {
            if !list.profiles.is_empty() || list.command == "trace.list.profiles" {
                let mut markdown = "# Trace Profiles\n\n".to_string();
                for profile in &list.profiles {
                    markdown.push_str(&format!("- `{}`", profile.id));
                    markdown.push_str(&format!(" in rig `{}`", profile.rig_id));
                    if let Some(scenario) = profile.scenario.as_deref() {
                        markdown.push_str(&format!(": `{}`", scenario));
                    }
                    markdown.push('\n');
                }
                return markdown;
            }
            let mut markdown = format!("# Trace Scenarios: `{}`\n\n", list.component_id);
            for scenario in &list.scenarios {
                markdown.push_str(&format!("- `{}`", scenario.id));
                if let Some(summary) = scenario.summary.as_deref() {
                    markdown.push_str(&format!(": {}", summary));
                }
                markdown.push('\n');
            }
            markdown
        }
        TraceCommandOutput::OverlayLocks(locks) => {
            let mut markdown = format!("# Trace Overlay Locks\n\n- **Count:** `{}`\n- **Active:** `{}`\n- **Stale:** `{}`\n- **Unknown:** `{}`\n\n", locks.count, locks.active_count, locks.stale_count, locks.unknown_count);
            for lock in &locks.locks {
                markdown.push_str(&format!("- `{}`: `{:?}`\n", lock.lock_path, lock.status));
            }
            markdown
        }
    }
}

pub fn run(args: TraceArgs, _global: &GlobalArgs) -> CmdResult<TraceCommandOutput> {
    let ((stdout_output, _artifact_output), exit_code) = run_outputs(args)?;
    Ok((stdout_output, exit_code))
}

pub fn run_json_with_output_artifact(
    args: TraceArgs,
    _global: &GlobalArgs,
) -> (
    homeboy::core::Result<serde_json::Value>,
    i32,
    Option<homeboy::core::Result<serde_json::Value>>,
) {
    crate::commands::utils::tty::status("homeboy is working...");
    let output_to_json = |output: TraceCommandOutput| {
        serde_json::to_value(output).map_err(|err| {
            homeboy::core::Error::internal_json(
                err.to_string(),
                Some("serialize response".to_string()),
            )
        })
    };
    match run_outputs(args) {
        Ok(((stdout_output, artifact_output), exit_code)) => (
            output_to_json(stdout_output),
            exit_code,
            artifact_output.map(output_to_json),
        ),
        Err(err) => {
            let (json_result, exit_code) = crate::commands::utils::response::map_cmd_result_to_json::<
                TraceCommandOutput,
            >(Err(err));
            (json_result, exit_code, None)
        }
    }
}

fn run_outputs(mut args: TraceArgs) -> CmdResult<(TraceCommandOutput, Option<TraceCommandOutput>)> {
    if args.profiles && args.comp.component.as_deref() == Some("list") {
        let output = run_list_profiles(args.rig.as_deref())?;
        return Ok(((output, None), 0));
    }

    resolve_trace_profile_args(&mut args)?;

    if args.comp.component.as_deref() == Some("overlay-locks") {
        let (output, exit_code) = run_overlay_locks(args)?;
        return Ok(((output, None), exit_code));
    }

    if args.comp.component.as_deref() == Some("compare") {
        if args.baseline_target.is_some() || args.candidate.is_some() {
            let (output, exit_code) = run_compare_targets(args)?;
            return Ok(((output, None), exit_code));
        }
        let (output, exit_code) = run_compare(args)?;
        return Ok(((output, None), exit_code));
    }

    if args.comp.component.as_deref() == Some("matrix") {
        apply_matrix_target_args(&mut args);
        let (output, exit_code) = matrix::run_scenario_matrix(args)?;
        return Ok(((output, None), exit_code));
    }

    if args.comp.component.as_deref() == Some("compare-variant") {
        let (output, exit_code) = if args.matrix == TraceVariantMatrixMode::None {
            run_compare_variant(args)?
        } else {
            matrix::run_variant_matrix(args)?
        };
        return Ok(((output, None), exit_code));
    }

    if args.compare_after.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "AFTER_JSON",
            "extra positional argument is only supported by `homeboy trace compare before.json after.json`",
            None,
            None,
        ));
    }

    if args.repeat == 0 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--repeat",
            "repeat must be at least 1",
            None,
            None,
        ));
    }

    if trace_scenario(&args)? == "list" {
        let (output, exit_code) = run_list(args)?;
        return Ok(((output, None), exit_code));
    }

    if args.repeat > 1 || args.aggregate.as_deref() == Some("spans") {
        let (output, exit_code) = run_repeat(args)?;
        return Ok(((output, None), exit_code));
    }

    let summary_only = args.json_summary;
    let profile = resolved_profile_output_for_args(&args);
    let span_metadata = trace_span_metadata_for_args(&args)?;
    let execution = execute_trace_run(args)?;

    let (mut stdout_output, mut artifact_output, exit_code) =
        extension_trace::from_main_workflow_outputs(
            execution.workflow,
            execution.rig_state,
            summary_only,
        );
    extension_trace::attach_span_summary_metadata(&mut stdout_output, &span_metadata);
    attach_profile_output(&mut stdout_output, profile.clone());
    if let Some(output) = artifact_output.as_mut() {
        extension_trace::attach_span_summary_metadata(output, &span_metadata);
        attach_profile_output(output, profile);
    }
    Ok(((stdout_output, artifact_output), exit_code))
}

pub(super) fn apply_command_target_component(args: &mut TraceArgs) {
    args.comp.component = args.component_arg.clone();
}

fn apply_matrix_target_args(args: &mut TraceArgs) {
    let positional_component = args.scenario.take();
    let positional_scenario = args
        .compare_after
        .take()
        .map(|path| path.to_string_lossy().to_string());
    args.comp.component = args.component_arg.clone().or(positional_component);
    args.scenario = args.scenario_arg.clone().or(positional_scenario);
}

pub(super) fn required_trace_scenario(args: &TraceArgs) -> homeboy::core::Result<String> {
    args.scenario.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec!["trace scenario".to_string()])
    })
}

struct TraceRunExecution {
    workflow: extension_trace::TraceRunWorkflowResult,
    run_dir: RunDir,
    rig_state: Option<rig::RigStateSnapshot>,
}

fn execute_trace_run(args: TraceArgs) -> homeboy::core::Result<TraceRunExecution> {
    let scenario = required_trace_scenario(&args)?;
    let rig_context = load_rig_context(args.rig.as_deref())?;
    let effective_id = resolve_component_id(&args.comp, rig_context.as_ref().map(|c| &c.rig_spec))?;
    let path_override = args.comp.path.clone().or_else(|| {
        rig_context
            .as_ref()
            .and_then(|context| rig_component_path(&context.rig_spec, &effective_id))
    });
    let component_override = rig_context
        .as_ref()
        .and_then(|context| rig_component_for_trace(&context.rig_spec, &effective_id));

    let ctx = resolve_trace_execution_context(
        &effective_id,
        path_override.clone(),
        args.setting_args.setting.clone(),
        args.setting_args.setting_json.clone(),
        component_override,
    )?;
    if let Some(context) = rig_context.as_ref() {
        run_rig_workload_preflight(
            &context.rig_spec,
            ctx.extension_id.as_deref(),
            rig::RigWorkloadKind::Trace,
        )?;
    }
    let span_definitions = span_definitions_for_args(
        &args,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
        args.aggregate.as_deref() == Some("spans")
            && args.phase_preset.is_none()
            && args.phases.is_empty()
            && args.spans.is_empty(),
    )?;
    let component_path_for_overlays = path_override
        .clone()
        .unwrap_or_else(|| ctx.component.local_path.clone());
    let overlays = trace_overlays_for_args(
        &args,
        rig_context.as_ref(),
        &effective_id,
        &component_path_for_overlays,
    )?;
    let experiment_plan = trace_experiment_plan_for_args(&args, rig_context.as_ref())?;

    let rig_state = rig_context
        .as_ref()
        .map(|context| rig::snapshot_state(&context.rig_spec));
    let run_dir = RunDir::create()?;
    run_trace_experiment_setup_for_plan(experiment_plan.as_ref(), &run_dir)?;
    let scenario_id = scenario.clone();
    let rig_id = args.rig.clone();
    let requested_overlays = args.overlays.clone();
    let requested_variants = args.variants.clone();
    let component_path_for_observation = path_override
        .clone()
        .unwrap_or_else(|| ctx.component.local_path.clone());
    let observation = ObservationStore::open_initialized().ok().and_then(|store| {
        let cwd = std::env::current_dir().ok();
        store
            .start_run(
                NewRunRecord::builder("trace")
                    .component_id(ctx.component_id.clone())
                    .command(std::env::args().collect::<Vec<_>>().join(" "))
                    .optional_cwd_path(cwd.as_deref())
                    .current_homeboy_version()
                    .git_sha(homeboy::core::git::short_head_revision_at(Path::new(
                        &component_path_for_observation,
                    )))
                    .optional_rig_id(rig_id.clone())
                    .metadata(serde_json::json!({
                        "scenario_id": scenario_id,
                        "component_path": component_path_for_observation,
                        "requested_overlays": requested_overlays,
                        "requested_variants": requested_variants,
                        "span_definitions": span_definitions.clone(),
                        "phase_preset": args.phase_preset.clone(),
                        "phase_milestones": args.phases.clone().into_iter().map(|phase| {
                            serde_json::json!({ "label": phase.label, "key": phase.key })
                        }).collect::<Vec<_>>(),
                        "baseline": {
                            "baseline": args.baseline_args.baseline,
                            "ignore_baseline": args.baseline_args.ignore_baseline,
                            "ratchet": args.baseline_args.ratchet,
                            "regression_threshold_percent": args.regression_threshold,
                            "regression_min_delta_ms": args.regression_min_delta_ms
                        }
                    }))
                    .build(),
            )
            .ok()
            .map(|run| ActiveTraceObservation {
                store,
                run_id: run.id,
                component_id: ctx.component_id.clone(),
                rig_id: rig_id.clone(),
                scenario_id: scenario_id.clone(),
            })
    });
    let (extra_workloads, trace_dependencies, runner_capabilities) =
        trace_workload_inputs(rig_context.as_ref(), ctx.extension_id.as_deref());
    let experiment_settings = trace_experiment_settings(experiment_plan.as_ref())?;
    let mut experiment_env = trace_experiment_env(experiment_plan.as_ref())?;
    experiment_env.extend(args.matrix_env.clone());
    let trace_probes =
        trace_probes_for_args(&args, rig_context.as_ref(), ctx.extension_id.as_deref())?;
    let attachments = TraceAttachment::parse_all(&args.attachments)?;
    let resolved_settings = ctx.resolved_settings();
    let mut json_settings = experiment_settings;
    json_settings.extend(resolved_settings.json_overrides());
    json_settings.extend(
        resolved_settings
            .string_overrides()
            .into_iter()
            .map(|(key, value)| (key, serde_json::Value::String(value))),
    );
    let workflow = match extension_trace::run_trace_workflow(
        &ctx.component,
        TraceRunWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override,
            settings: resolved_settings.string_overrides(),
            runner_inputs: TraceRunnerInputs {
                json_settings,
                env: experiment_env.clone(),
                workload_paths: extra_workloads,
                probes: trace_probes,
                attachments,
                dependencies: trace_dependencies,
                runner_capabilities,
            },
            scenario_id,
            json_summary: args.json_summary,
            rig_id: args.rig.clone(),
            overlays,
            keep_overlay: args.keep_overlay,
            span_definitions,
            baseline_flags: BaselineFlags {
                baseline: args.baseline_args.baseline,
                ignore_baseline: args.baseline_args.ignore_baseline,
                ratchet: args.baseline_args.ratchet,
            },
            regression_threshold_percent: args.regression_threshold,
            regression_min_delta_ms: args.regression_min_delta_ms,
            canonical_policy: trace_canonical_policy(&args),
        },
        &run_dir,
        rig_state.clone(),
    ) {
        Ok(mut workflow) => {
            let artifact_result = collect_trace_experiment_artifacts_for_plan(
                experiment_plan.as_ref(),
                &run_dir,
                &mut workflow,
            );
            let teardown_result =
                run_trace_experiment_teardown_for_plan(experiment_plan.as_ref(), &run_dir);
            artifact_result?;
            teardown_result?;
            workflow
        }
        Err(error) => {
            let _ = run_trace_experiment_teardown_for_plan(experiment_plan.as_ref(), &run_dir);
            if let Some(observation) = observation.as_ref() {
                persist_trace_workflow_error(observation, &run_dir, &error);
            }
            return Err(error);
        }
    };
    if let Some(observation) = observation.as_ref() {
        persist_trace_workflow_result(observation, &run_dir, &workflow, rig_state.as_ref());
    }

    Ok(TraceRunExecution {
        workflow,
        run_dir,
        rig_state,
    })
}

fn trace_canonical_policy(args: &TraceArgs) -> TraceCanonicalPolicy {
    if args.allow_local_toolchain {
        TraceCanonicalPolicy::AllowLocalToolchain
    } else if args.canonical {
        TraceCanonicalPolicy::Canonical
    } else {
        TraceCanonicalPolicy::Development
    }
}

fn trace_scenario(args: &TraceArgs) -> homeboy::core::Result<&str> {
    args.scenario_arg
        .as_deref()
        .or(args.scenario.as_deref())
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "scenario",
                "trace requires a scenario positional argument or --scenario",
                None,
                None,
            )
        })
}

const DEFAULT_TRACE_PHASE_PRESET: &str = "default";

fn cli_span_definitions_for_args(
    args: &TraceArgs,
) -> homeboy::core::Result<Vec<TraceSpanDefinition>> {
    let mut definitions = args.spans.clone();
    let phase_definitions =
        extension_trace::spans::phase_span_definitions(&args.phases).map_err(|message| {
            homeboy::core::Error::validation_invalid_argument("--phase", message, None, None)
        })?;
    definitions.extend(phase_definitions);
    Ok(definitions)
}

fn span_definitions_for_args(
    args: &TraceArgs,
    rig_context: Option<&TraceRigContext>,
    extension_id: Option<&str>,
    use_default_preset: bool,
) -> homeboy::core::Result<Vec<TraceSpanDefinition>> {
    let mut definitions = cli_span_definitions_for_args(args)?;
    let Some(preset_name) = args.phase_preset.as_deref().or_else(|| {
        if use_default_preset {
            default_trace_phase_preset_for_args(args, rig_context, extension_id)
        } else {
            None
        }
    }) else {
        return Ok(definitions);
    };

    let preset_phases = trace_phase_preset_for_args(args, rig_context, extension_id, preset_name)?;
    let phase_definitions = extension_trace::spans::phase_span_definitions(&preset_phases)
        .map_err(|message| {
            homeboy::core::Error::validation_invalid_argument("--phase-preset", message, None, None)
        })?;
    definitions.extend(phase_definitions);
    Ok(definitions)
}

fn default_trace_phase_preset_for_args<'a>(
    args: &TraceArgs,
    rig_context: Option<&'a TraceRigContext>,
    extension_id: Option<&str>,
) -> Option<&'a str> {
    let scenario = trace_scenario(args).ok()?;
    let context = rig_context?;
    let extension_id = extension_id?;
    let workload = context
        .rig_spec
        .trace_workloads
        .get(extension_id)
        .and_then(|workloads| {
            workloads
                .iter()
                .find(|workload| trace_workload_scenario_id(workload.path()) == scenario)
        })?;
    workload.trace_default_phase_preset().or_else(|| {
        workload
            .trace_phase_preset(DEFAULT_TRACE_PHASE_PRESET)
            .map(|_| DEFAULT_TRACE_PHASE_PRESET)
    })
}

fn trace_phase_preset_for_args(
    args: &TraceArgs,
    rig_context: Option<&TraceRigContext>,
    extension_id: Option<&str>,
    preset_name: &str,
) -> homeboy::core::Result<Vec<extension_trace::spans::TracePhaseMilestone>> {
    let scenario = trace_scenario(args)?;
    let context = rig_context.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "--phase-preset",
            "phase presets require --rig so Homeboy can read rig/workload metadata",
            None,
            None,
        )
    })?;
    let extension_id = extension_id.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "--phase-preset",
            "phase presets require a resolved trace extension",
            None,
            None,
        )
    })?;

    let workloads = context
        .rig_spec
        .trace_workloads
        .get(extension_id)
        .map(|workloads| workloads.as_slice())
        .unwrap_or(&[]);
    let workload = workloads
        .iter()
        .find(|workload| trace_workload_scenario_id(workload.path()) == scenario);
    let phases = workload
        .and_then(|workload| workload.trace_phase_preset(preset_name))
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "--phase-preset",
                format!(
                    "trace phase preset '{}' is not declared for scenario '{}'",
                    preset_name, scenario
                ),
                None,
                None,
            )
        })?;

    phases
        .iter()
        .map(|phase| {
            extension_trace::spans::parse_phase_milestone(phase).map_err(|message| {
                homeboy::core::Error::validation_invalid_argument(
                    "--phase-preset",
                    message,
                    None,
                    None,
                )
            })
        })
        .collect()
}

fn trace_workload_scenario_id(path: &str) -> String {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    if let Some((stem, _)) = file_name.split_once(".trace.") {
        return stem.to_string();
    }
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name)
        .to_string()
}

fn run_list(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    let rig_context = load_rig_context(args.rig.as_deref())?;
    let effective_id = resolve_component_id(&args.comp, rig_context.as_ref().map(|c| &c.rig_spec))?;
    let path_override = args.comp.path.clone().or_else(|| {
        rig_context
            .as_ref()
            .and_then(|context| rig_component_path(&context.rig_spec, &effective_id))
    });
    let component_override = rig_context
        .as_ref()
        .and_then(|context| rig_component_for_trace(&context.rig_spec, &effective_id));

    let ctx = resolve_trace_execution_context(
        &effective_id,
        path_override.clone(),
        args.setting_args.setting.clone(),
        args.setting_args.setting_json.clone(),
        component_override,
    )?;
    if let Some(context) = rig_context.as_ref() {
        run_rig_workload_preflight(
            &context.rig_spec,
            ctx.extension_id.as_deref(),
            rig::RigWorkloadKind::Trace,
        )?;
    }

    let run_dir = RunDir::create()?;
    let (extra_workloads, trace_dependencies, runner_capabilities) =
        trace_workload_inputs(rig_context.as_ref(), ctx.extension_id.as_deref());
    let list = extension_trace::run_trace_list_workflow(
        &ctx.component,
        TraceListWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override,
            settings: ctx.resolved_settings().string_overrides(),
            runner_inputs: TraceRunnerInputs {
                json_settings: ctx.resolved_settings().json_overrides(),
                env: Vec::new(),
                workload_paths: extra_workloads,
                probes: Vec::new(),
                attachments: Vec::new(),
                dependencies: trace_dependencies,
                runner_capabilities,
            },
            rig_id: args.rig,
        },
        &run_dir,
    )?;

    Ok(extension_trace::from_list_workflow(effective_id, list))
}

struct TraceRigContext {
    rig_spec: RigSpec,
    rig_package_root: Option<PathBuf>,
    rig_config_root: Option<PathBuf>,
}

fn trace_workload_inputs(
    rig_context: Option<&TraceRigContext>,
    extension_id: Option<&str>,
) -> (Vec<PathBuf>, Vec<rig::TraceDependencySpec>, Vec<String>) {
    let Some((context, id)) = rig_context.zip(extension_id) else {
        return (Vec::new(), Vec::new(), Vec::new());
    };

    (
        rig::workloads_for_extension(
            &context.rig_spec,
            rig::RigWorkloadKind::Trace,
            context.rig_package_root.as_deref(),
            id,
        ),
        rig::trace_dependencies_for_extension(
            &context.rig_spec,
            context.rig_package_root.as_deref(),
            id,
        ),
        rig::runner_capabilities_for_extension(&context.rig_spec, id),
    )
}

#[derive(Clone)]
struct ResolvedTraceProfile {
    rig_id: String,
    profile: rig::TraceProfileSpec,
}

fn resolve_trace_profile_args(args: &mut TraceArgs) -> homeboy::core::Result<()> {
    let Some(profile_id) = args.profile.as_deref() else {
        return Ok(());
    };
    let resolved = resolve_trace_profile(profile_id, args.rig.as_deref())?;
    if args.comp.component.is_none() {
        args.comp.component = resolved.profile.component.clone();
    }
    if args.scenario.is_none() && args.scenario_arg.is_none() {
        args.scenario = resolved.profile.scenario.clone();
    }
    if args.rig.is_none() {
        args.rig = resolved
            .profile
            .rig
            .clone()
            .or_else(|| Some(resolved.rig_id.clone()));
    }
    if args.comp.component.is_none() {
        if let Some(rig_id) = args.rig.as_deref() {
            let rig_spec = rig::load(rig_id)?;
            if rig_spec.components.len() == 1 {
                args.comp.component = rig_spec.components.keys().next().cloned();
            }
        }
    }

    let mut settings = resolved.profile.string_settings();
    settings.extend(args.setting_args.setting.clone());
    args.setting_args.setting = settings;

    let mut setting_json = resolved.profile.json_settings();
    setting_json.extend(args.setting_args.setting_json.clone());
    args.setting_args.setting_json = setting_json;

    let mut overlays = resolved.profile.overlays.clone();
    overlays.extend(args.overlays.clone());
    args.overlays = overlays;

    let mut variants = resolved.profile.variants.clone();
    variants.extend(args.variants.clone());
    args.variants = variants;
    Ok(())
}

fn resolve_trace_profile(
    profile_id: &str,
    rig_id: Option<&str>,
) -> homeboy::core::Result<ResolvedTraceProfile> {
    if let Some(rig_id) = rig_id {
        let rig_spec = rig::load(rig_id)?;
        let profile = rig_spec.trace_profiles.get(profile_id).ok_or_else(|| {
            let available = rig_spec.trace_profiles.keys().cloned().collect::<Vec<_>>();
            homeboy::core::Error::validation_invalid_argument(
                "--profile",
                format!("unknown trace profile '{}' in rig '{}'", profile_id, rig_id),
                available
                    .is_empty()
                    .then_some("the selected rig declares no trace profiles".to_string()),
                (!available.is_empty()).then_some(available),
            )
        })?;
        return Ok(ResolvedTraceProfile {
            rig_id: rig_id.to_string(),
            profile: profile.clone(),
        });
    }

    let matches = rig::list()?
        .into_iter()
        .filter_map(|rig_spec| {
            rig_spec
                .trace_profiles
                .get(profile_id)
                .cloned()
                .map(|profile| ResolvedTraceProfile {
                    rig_id: rig_spec.id,
                    profile,
                })
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [resolved] => Ok(resolved.clone()),
        [] => Err(homeboy::core::Error::validation_invalid_argument(
            "--profile",
            format!("unknown trace profile '{}'", profile_id),
            Some("declare the profile in a rig spec or pass --rig to scope lookup".to_string()),
            None,
        )),
        many => Err(homeboy::core::Error::validation_invalid_argument(
            "--profile",
            format!(
                "trace profile '{}' is declared by multiple rigs",
                profile_id
            ),
            Some("pass --rig to choose one profile definition".to_string()),
            Some(many.iter().map(|profile| profile.rig_id.clone()).collect()),
        )),
    }
}

fn resolved_profile_output_for_args(
    args: &TraceArgs,
) -> Option<extension_trace::TraceResolvedProfileOutput> {
    let profile_id = args.profile.clone()?;
    let scenario = trace_scenario(args).ok()?.to_string();
    Some(extension_trace::TraceResolvedProfileOutput {
        id: profile_id,
        rig_id: args.rig.clone(),
        component: args.comp.component.clone().unwrap_or_default(),
        scenario,
        overlays: args.overlays.clone(),
        variants: args.variants.clone(),
        settings: args
            .setting_args
            .setting
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .chain(args.setting_args.setting_json.iter().cloned())
            .collect(),
    })
}

fn attach_profile_output(
    output: &mut TraceCommandOutput,
    profile: Option<extension_trace::TraceResolvedProfileOutput>,
) {
    let Some(profile) = profile else {
        return;
    };
    match output {
        TraceCommandOutput::Run(run) => run.profile = Some(profile),
        TraceCommandOutput::Summary(summary) => summary.profile = Some(profile),
        TraceCommandOutput::Aggregate(aggregate) => aggregate.profile = Some(profile),
        _ => {}
    }
}

fn run_list_profiles(rig_id: Option<&str>) -> homeboy::core::Result<TraceCommandOutput> {
    let rigs = match rig_id {
        Some(rig_id) => vec![rig::load(rig_id)?],
        None => rig::list()?,
    };
    let mut profiles = Vec::new();
    for rig_spec in rigs {
        for (id, profile) in rig_spec.trace_profiles {
            profiles.push(extension_trace::TraceProfileListItem {
                id,
                rig_id: rig_spec.id.clone(),
                component: profile.component,
                scenario: profile.scenario,
            });
        }
    }
    profiles.sort_by(|a, b| a.rig_id.cmp(&b.rig_id).then(a.id.cmp(&b.id)));
    Ok(TraceCommandOutput::List(extension_trace::TraceListOutput {
        command: "trace.list.profiles",
        component: "profiles".to_string(),
        component_id: "profiles".to_string(),
        count: profiles.len(),
        scenarios: Vec::new(),
        profiles,
    }))
}

fn load_rig_context(rig_id: Option<&str>) -> homeboy::core::Result<Option<TraceRigContext>> {
    let Some(rig_id) = rig_id else {
        return Ok(None);
    };
    let spec = rig::load(rig_id)?;
    let package_root =
        rig::read_source_metadata(&spec.id).map(|metadata| PathBuf::from(metadata.package_path));
    let config_root = crate::core::paths::rig_config(&spec.id)
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    Ok(Some(TraceRigContext {
        rig_spec: spec,
        rig_package_root: package_root,
        rig_config_root: config_root,
    }))
}

fn resolve_trace_execution_context(
    effective_id: &str,
    path_override: Option<String>,
    settings: Vec<(String, String)>,
    settings_json: Vec<(String, serde_json::Value)>,
    component_override: Option<Component>,
) -> homeboy::core::Result<execution_context::ExecutionContext> {
    match execution_context::resolve_with_component(
        &ResolveOptions::with_capability_and_json(
            effective_id,
            path_override.clone(),
            ExtensionCapability::Trace,
            settings,
            settings_json,
        ),
        component_override.clone(),
    ) {
        Ok(ctx) => Ok(ctx),
        Err(error) if extension_trace::trace_is_unclaimed(&error) => {
            execution_context::resolve_with_component(
                &ResolveOptions::source_only(effective_id, path_override),
                component_override,
            )
        }
        Err(error) => Err(error),
    }
}

fn trace_overlays_for_args(
    args: &TraceArgs,
    rig_context: Option<&TraceRigContext>,
    component_id: &str,
    component_path: &str,
) -> homeboy::core::Result<Vec<TraceOverlayRequest>> {
    let mut overlays = Vec::new();
    if !args.variants.is_empty() {
        let context = rig_context.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "--variant",
                "trace variants require --rig so Homeboy can read rig/workload metadata",
                None,
                None,
            )
        })?;
        let scenario = required_trace_scenario(args)?;
        let variants = trace_variants_for_args(context, component_id, &scenario);
        let available = variants.keys().cloned().collect::<Vec<_>>();
        for name in &args.variants {
            let variant = variants.get(name).ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "--variant",
                    format!(
                        "unknown trace variant '{}' for component '{}' and scenario '{}'",
                        name, component_id, scenario
                    ),
                    Some(format!(
                        "available variants: {}",
                        if available.is_empty() {
                            "none".to_string()
                        } else {
                            available.join(", ")
                        }
                    )),
                    None,
                )
            })?;
            overlays.extend(trace_variant_overlay_requests(
                context,
                name,
                variant,
                component_id,
            )?);
        }
    }
    overlays.extend(
        args.overlays
            .iter()
            .cloned()
            .map(|overlay_path| TraceOverlayRequest {
                variant: None,
                component_id: Some(component_id.to_string()),
                component_path: component_path.to_string(),
                overlay_path,
            }),
    );
    Ok(overlays)
}

pub(super) fn validate_trace_variants_for_args(args: &TraceArgs) -> homeboy::core::Result<()> {
    if args.variants.is_empty() {
        return Ok(());
    }
    let rig_context = load_rig_context(args.rig.as_deref())?;
    let effective_id = resolve_component_id(
        &args.comp,
        rig_context.as_ref().map(|context| &context.rig_spec),
    )?;
    let component_path = args
        .comp
        .path
        .clone()
        .or_else(|| {
            rig_context
                .as_ref()
                .and_then(|context| rig_component_path(&context.rig_spec, &effective_id))
        })
        .unwrap_or_default();
    trace_overlays_for_args(args, rig_context.as_ref(), &effective_id, &component_path)?;
    Ok(())
}

fn trace_variant_overlay_requests(
    context: &TraceRigContext,
    variant_name: &str,
    variant: &rig::TraceVariantSpec,
    default_component_id: &str,
) -> homeboy::core::Result<Vec<TraceOverlayRequest>> {
    let mut requests = Vec::new();
    if let Some(overlay) = variant.overlay.as_deref() {
        let component_id = variant.component.as_deref().unwrap_or(default_component_id);
        requests.push(trace_overlay_request_for_component(
            context,
            variant_name,
            component_id,
            overlay,
        )?);
    }
    for overlay in &variant.overlays {
        requests.push(trace_overlay_request_for_component(
            context,
            variant_name,
            &overlay.component,
            &overlay.overlay,
        )?);
    }
    Ok(requests)
}

fn trace_overlay_request_for_component(
    context: &TraceRigContext,
    variant_name: &str,
    component_id: &str,
    overlay: &str,
) -> homeboy::core::Result<TraceOverlayRequest> {
    let component_path = rig_component_path(&context.rig_spec, component_id).ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "--variant",
            format!(
                "trace variant '{}' overlay references unknown component '{}'",
                variant_name, component_id
            ),
            None,
            None,
        )
    })?;
    Ok(TraceOverlayRequest {
        variant: Some(variant_name.to_string()),
        component_id: Some(component_id.to_string()),
        component_path,
        overlay_path: resolve_trace_variant_overlay(context, overlay),
    })
}

fn trace_variants_for_args<'a>(
    context: &'a TraceRigContext,
    component_id: &str,
    scenario: &str,
) -> BTreeMap<String, &'a rig::TraceVariantSpec> {
    let mut variants = BTreeMap::new();
    for (name, variant) in &context.rig_spec.trace_variants {
        if trace_variant_matches_component(variant, component_id) {
            variants.insert(name.clone(), variant);
        }
    }
    for workload in context
        .rig_spec
        .trace_workloads
        .values()
        .flat_map(|workloads| workloads.iter())
    {
        if trace_workload_scenario_id(workload.path()) != scenario {
            continue;
        }
        for (name, variant) in workload.trace_variants() {
            if trace_variant_matches_component(variant, component_id) {
                variants.insert(name.clone(), variant);
            }
        }
    }
    variants
}

fn trace_variant_matches_component(variant: &rig::TraceVariantSpec, component_id: &str) -> bool {
    if !variant.overlays.is_empty() {
        return variant
            .overlays
            .iter()
            .any(|overlay| overlay.component == component_id);
    }
    variant
        .component
        .as_deref()
        .is_none_or(|id| id == component_id)
}

fn resolve_trace_variant_overlay(context: &TraceRigContext, overlay: &str) -> String {
    let expanded = rig::expand::expand_vars(&context.rig_spec, overlay);
    let expanded = if let Some(root) = context.rig_package_root.as_ref() {
        expanded.replace("${package.root}", &root.to_string_lossy())
    } else {
        expanded
    };
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        return path.to_string_lossy().to_string();
    }
    context
        .rig_package_root
        .as_ref()
        .or(context.rig_config_root.as_ref())
        .map(|root| root.join(path).to_string_lossy().to_string())
        .unwrap_or(expanded)
}

fn run_rig_workload_preflight(
    spec: &RigSpec,
    extension_id: Option<&str>,
    kind: rig::RigWorkloadKind,
) -> homeboy::core::Result<()> {
    let groups =
        extension_id.and_then(|id| rig::check_groups_for_extension_workloads(spec, kind, id));
    let check = match groups {
        Some(groups) => rig::run_check_groups(spec, &groups)?,
        None => rig::run_check(spec)?,
    };
    if !check.success {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--rig",
            format!(
                "rig '{}' check failed; fix the rig before running trace",
                spec.id
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn resolve_component_id(
    comp: &PositionalComponentArgs,
    rig_spec: Option<&RigSpec>,
) -> homeboy::core::Result<String> {
    if let Some(id) = comp.id() {
        return Ok(id.to_string());
    }
    if let Some(spec) = rig_spec {
        if spec.components.len() == 1 {
            return Ok(spec.components.keys().next().unwrap().clone());
        }
        return Err(homeboy::core::Error::validation_invalid_argument(
            "component",
            format!(
                "rig '{}' has multiple components; pass the component id to trace",
                spec.id
            ),
            None,
            None,
        ));
    }
    comp.resolve_id()
}

fn rig_component_path(spec: &RigSpec, component_id: &str) -> Option<String> {
    let component = spec.components.get(component_id)?;
    Some(homeboy::core::rig::expand::expand_vars(
        spec,
        &component.path,
    ))
}

fn rig_component_for_trace(spec: &RigSpec, component_id: &str) -> Option<Component> {
    let component = spec.components.get(component_id)?;
    let mut extensions = component.extensions.clone().unwrap_or_default();
    for extension_id in rig::extension_ids_for_workloads(spec, rig::RigWorkloadKind::Trace) {
        extensions
            .entry(extension_id)
            .or_insert_with(ScopedExtensionConfig::default);
    }
    Some(Component {
        id: component_id.to_string(),
        local_path: rig_component_path(spec, component_id)
            .unwrap_or_else(|| component.path.clone()),
        remote_url: component.remote_url.clone(),
        triage_remote_url: component.triage_remote_url.clone(),
        extensions: if extensions.is_empty() {
            None
        } else {
            Some(extensions)
        },
        ..Default::default()
    })
}

struct ActiveTraceObservation {
    store: ObservationStore,
    run_id: String,
    component_id: String,
    rig_id: Option<String>,
    scenario_id: String,
}

fn persist_trace_workflow_result(
    observation: &ActiveTraceObservation,
    run_dir: &RunDir,
    workflow: &extension_trace::TraceRunWorkflowResult,
    rig_state: Option<&rig::RigStateSnapshot>,
) {
    let run_status = trace_run_status(workflow);
    let baseline_status = baseline_status(workflow);
    let results = workflow.results.as_ref();
    let trace_scenario_id = results
        .map(|results| results.scenario_id.clone())
        .unwrap_or_else(|| observation.scenario_id.clone());
    let _ = observation.store.record_trace_run(
        NewTraceRunRecord::builder(
            &observation.run_id,
            &observation.component_id,
            trace_scenario_id,
            run_status.as_str(),
        )
        .trace_rig_id(observation.rig_id.as_deref())
        .baseline_status(baseline_status.as_deref())
        .metadata(serde_json::json!({
            "status": &workflow.status,
            "exit_code": workflow.exit_code,
            "summary": results.and_then(|results| results.summary.clone()),
            "failure": &workflow.failure,
            "overlays": &workflow.overlays,
            "baseline_comparison": &workflow.baseline_comparison,
            "baseline_status": baseline_status,
            "hints": &workflow.hints,
            "rig_state": rig_state,
            "assertion_count": results.map(|results| results.assertions.len()).unwrap_or(0),
            "artifact_count": results.map(|results| results.artifacts.len()).unwrap_or(0),
            "span_count": results.map(|results| results.span_results.len()).unwrap_or(0),
        }))
        .build(),
    );

    if let Some(results) = results {
        for span in &results.span_results {
            let _ = observation.store.record_trace_span(
                NewTraceSpanRecord::builder(
                    &observation.run_id,
                    &span.id,
                    format!("{:?}", span.status).to_ascii_lowercase(),
                )
                .duration_ms(span.duration_ms.map(|value| value as f64))
                .from_event(Some(&span.from))
                .to_event(Some(&span.to))
                .metadata(serde_json::json!({
                    "from_t_ms": span.from_t_ms,
                    "to_t_ms": span.to_t_ms,
                    "missing": span.missing,
                    "message": &span.message,
                }))
                .build(),
            );
        }
    }

    let artifact_observation =
        record_trace_artifacts(&observation.store, &observation.run_id, run_dir, results);
    let finish_status =
        if run_status == RunStatus::Pass && artifact_observation.has_declared_artifact_failures() {
            RunStatus::Fail
        } else {
            run_status
        };
    let _ = observation.store.finish_run(
        &observation.run_id,
        finish_status,
        Some(trace_run_finish_metadata(
            workflow,
            Some(&artifact_observation),
        )),
    );
}

fn persist_trace_workflow_error(
    observation: &ActiveTraceObservation,
    run_dir: &RunDir,
    error: &homeboy::core::Error,
) {
    let error_metadata = serde_json::json!({
        "error": {
            "code": error.code.as_str(),
            "message": &error.message,
            "details": &error.details,
        }
    });
    let _ = observation.store.record_trace_run(
        NewTraceRunRecord::builder(
            &observation.run_id,
            &observation.component_id,
            &observation.scenario_id,
            RunStatus::Error.as_str(),
        )
        .trace_rig_id(observation.rig_id.as_deref())
        .metadata(error_metadata.clone())
        .build(),
    );
    let artifact_observation =
        record_trace_artifacts(&observation.store, &observation.run_id, run_dir, None);
    let _ = observation.store.finish_run(
        &observation.run_id,
        RunStatus::Error,
        Some(merge_trace_artifact_metadata(
            error_metadata,
            &artifact_observation,
        )),
    );
}

fn trace_run_status(workflow: &extension_trace::TraceRunWorkflowResult) -> RunStatus {
    if workflow.failure.is_some() || workflow.status == "error" {
        RunStatus::Error
    } else if workflow.exit_code == 0 && workflow.status == "pass" {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    }
}

fn baseline_status(workflow: &extension_trace::TraceRunWorkflowResult) -> Option<String> {
    workflow.baseline_comparison.as_ref().map(|comparison| {
        if comparison.regression {
            "regression"
        } else if comparison.has_improvements {
            "improvement"
        } else {
            "pass"
        }
        .to_string()
    })
}

fn trace_run_finish_metadata(
    workflow: &extension_trace::TraceRunWorkflowResult,
    artifact_observation: Option<&observations::TraceArtifactObservationResult>,
) -> serde_json::Value {
    let metadata = serde_json::json!({
        "status": &workflow.status,
        "exit_code": workflow.exit_code,
        "failure": &workflow.failure,
        "overlays": &workflow.overlays,
        "baseline_comparison": &workflow.baseline_comparison,
        "hints": &workflow.hints,
        "results": &workflow.results,
    });
    if let Some(artifact_observation) = artifact_observation {
        merge_trace_artifact_metadata(metadata, artifact_observation)
    } else {
        metadata
    }
}

fn merge_trace_artifact_metadata(
    mut metadata: serde_json::Value,
    artifact_observation: &observations::TraceArtifactObservationResult,
) -> serde_json::Value {
    if !artifact_observation.has_declared_artifact_failures() {
        return metadata;
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "artifact_validation".to_string(),
            serde_json::json!({
                "status": "fail",
                "missing_declared_artifacts": artifact_observation.missing_declared_artifacts,
                "invalid_declared_artifacts": artifact_observation.invalid_declared_artifacts,
            }),
        );
    }
    metadata
}

#[cfg(test)]
mod aggregate_test_support;
#[cfg(test)]
mod compare_tests;
#[cfg(test)]
mod compare_variant_tests;
#[cfg(test)]
mod experiment_tests;
#[cfg(test)]
mod generic_tests;
#[cfg(test)]
mod guardrail_tests;
#[cfg(test)]
mod output_tests;
#[cfg(test)]
mod tests;

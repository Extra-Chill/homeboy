use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use homeboy::core::component::{Component, ScopedExtensionConfig};
use homeboy::core::engine::baseline::BaselineFlags;
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::invocation::InvocationRequirements;
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::{
    TraceAttachment, TraceCanonicalPolicy, TraceCheckoutProvenance, TraceCommandOutput,
    TraceListWorkflowArgs, TraceOverlayRequest, TraceRunWorkflowArgs, TraceRunnerInputs,
    TraceSpanDefinition,
};
use homeboy::core::extension::ExtensionCapability;
use homeboy::core::observation::{NewRunRecord, ObservationStore};
use homeboy::core::rig::{self, RigSpec};

use super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

mod aggregate;
#[cfg(test)]
mod aggregate_tests;
mod bundle;
mod compare_bundle;
mod compare_targets;
mod compare_variant;
mod experiment;
mod guardrails;
mod matrix;
#[cfg(test)]
mod matrix_tests;
mod metadata;
mod observations;
mod output;
mod overlay_locks;
mod phase_args;
mod preview_args;
mod probes;
#[cfg(test)]
mod profile_tests;
pub(super) mod repeat;
#[cfg(test)]
mod rig_tests;
mod schedule;
#[cfg(test)]
mod test_fixture;
mod workload;

use compare_bundle::run_compare_bundle;
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
use overlay_locks::run_overlay_locks;
use phase_args::{cli_span_definitions_for_args, span_definitions_for_args};
use preview_args::trace_public_preview_for_args;

#[cfg(test)]
use output::render_aggregate_markdown;
use output::{
    attach_span_metadata, classification_summaries, parse_metric_guardrail, render_matrix_markdown,
    render_scenario_matrix_markdown, render_trace_aggregate_evidence_markdown,
    render_trace_compare_evidence_markdown, render_trace_run_evidence_markdown, run_compare,
};
use probes::trace_probes_for_args;
use repeat::run_repeat;
pub(super) use schedule::{
    plan_trace_run_order, TraceRunPlanEntry, TraceSchedule, TraceVariantMatrixMode,
};
use workload::trace_workload_scenario_id;

mod observation_lifecycle;
mod secret_env;

#[cfg(test)]
pub(super) use observation_lifecycle::finish_lab_dispatch_observation;
pub(super) use observation_lifecycle::start_lab_dispatch_observation;
use observation_lifecycle::{
    persist_trace_workflow_error, persist_trace_workflow_result, ActiveTraceObservation,
};
use secret_env::hydrate_trace_secret_env;
pub(crate) use secret_env::{
    apply_resolved_trace_secret_env, resolve_trace_secret_env_once,
    trace_secret_env_project_id_for_args, ResolvedTraceSecretEnv,
};

#[cfg(test)]
use matrix::{expand_variant_matrix, TraceVariantStackItem};
#[derive(Args, Clone)]
pub struct TraceArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,
    /// Target component for command-shaped trace modes like `compare-variant` and `compare-bundle`.
    #[arg(long = "component", value_name = "COMPONENT_ID")]
    pub component_arg: Option<String>,
    /// Scenario ID to run, or `list` to discover available scenarios.
    pub scenario: Option<String>,
    /// Scenario ID or comma-separated scenario list for command-shaped trace modes like `compare-variant` and `compare-bundle`.
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
    /// Secret environment variable name to hydrate for the trace runner. Repeatable.
    #[arg(long = "secret-env", value_name = "NAME")]
    pub secret_env: Vec<String>,
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

    /// Compare scalar metrics with `METRIC[.min|.median|.max]:POLICY[:VALUE]`. Repeatable.
    #[arg(long = "metric-guardrail", value_name = "SPEC", value_parser = parse_metric_guardrail)]
    pub metric_guardrails: Vec<output::TraceMetricGuardrailSpec>,

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

    /// Directory where trace matrix and compare bundle modes write aggregate, compare, cell, and summary artifacts.
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Run visual screenshot comparisons for trace compare browser artifacts.
    #[arg(long)]
    pub visual_compare: bool,

    /// Directory where visual compare artifacts should be written.
    #[arg(long, value_name = "DIR")]
    pub visual_artifacts_dir: Option<PathBuf>,

    /// Executable implementing the generic Homeboy visual compare provider contract.
    #[arg(long, value_name = "COMMAND")]
    pub visual_compare_provider: Option<String>,

    /// Extra argument forwarded to the visual compare provider before the input JSON path.
    #[arg(long = "visual-provider-arg", value_name = "ARG")]
    pub visual_provider_args: Vec<String>,

    /// Visual mismatch threshold forwarded to the visual compare provider.
    #[arg(long, value_name = "RATIO")]
    pub visual_threshold: Option<f64>,

    /// Leave overlay changes in place after the trace run.
    #[arg(long)]
    pub keep_overlay: bool,

    /// Require canonical evidence. This is the default; retained for explicit command logs.
    #[arg(long, alias = "proof")]
    pub canonical: bool,
    /// Allow intentionally local/development evidence. The output is marked non-canonical.
    #[arg(long, alias = "allow-local-evidence")]
    pub allow_local_toolchain: bool,

    /// Clean only stale trace overlay locks.
    #[arg(long)]
    pub stale: bool,

    /// Remove stale trace overlay locks even when touched files are dirty.
    #[arg(long)]
    pub force: bool,

    #[arg(skip)]
    pub checkout_provenance: Option<TraceCheckoutProvenance>,
}

impl TraceArgs {
    pub fn is_compare_target_run(&self) -> bool {
        self.comp.component.as_deref() == Some("compare")
            && (self.baseline_target.is_some() || self.candidate.is_some())
    }
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

    if args.comp.component.as_deref() == Some("compare-bundle") {
        let (output, exit_code) = run_compare_bundle(args)?;
        return Ok(((output, None), exit_code));
    }

    reject_target_compare_flags_without_compare(&args)?;

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

fn reject_target_compare_flags_without_compare(args: &TraceArgs) -> homeboy::core::Result<()> {
    if args.baseline_target.is_none() && args.candidate.is_none() {
        return Ok(());
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "--baseline-target/--candidate",
        "target compare flags are only supported by `homeboy trace compare`; use `homeboy trace compare <component> <scenario> --baseline-target <target> --candidate <target>`",
        None,
        None,
    ))
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
    let _lease = rig_context
        .as_ref()
        .map(|context| rig::lease::acquire_active_run_lease(&context.rig_spec, "trace"))
        .transpose()?
        .flatten();
    let span_definitions = span_definitions_for_args(
        &args,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
        args.phase_preset.is_none() && args.phases.is_empty() && args.spans.is_empty(),
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
    let experiment_settings = trace_experiment_settings(experiment_plan.as_ref())?;
    let mut experiment_env = trace_experiment_env(experiment_plan.as_ref())?;
    experiment_env.extend(args.matrix_env.clone());
    let trace_secret_env_status = hydrate_trace_secret_env(
        &args.secret_env,
        Some(&ctx.component_id),
        &mut experiment_env,
    )?;
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
                        "secret_env": trace_secret_env_status.clone(),
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
            .map(|run| {
                std::env::set_var(homeboy::core::observation::ACTIVE_RUN_ID_ENV, &run.id);
                ActiveTraceObservation {
                    store,
                    run_id: run.id,
                    component_id: ctx.component_id.clone(),
                    rig_id: rig_id.clone(),
                    scenario_id: scenario_id.clone(),
                }
            })
    });
    let (extra_workloads, trace_dependencies, runner_capabilities, invocation_requirements) =
        trace_workload_inputs(rig_context.as_ref(), ctx.extension_id.as_deref());
    warn_rig_owned_trace_workload_expansion(rig_context.as_ref(), ctx.extension_id.as_deref());
    let trace_probes =
        trace_probes_for_args(&args, rig_context.as_ref(), ctx.extension_id.as_deref())?;
    let public_preview =
        trace_public_preview_for_args(&args, rig_context.as_ref(), ctx.extension_id.as_deref())?;
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
                invocation_requirements,
                public_preview,
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
            canonical_policy: TraceCanonicalPolicy::from_flags(
                args.canonical,
                args.allow_local_toolchain,
            ),
            checkout_provenance: args.checkout_provenance,
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
    let (extra_workloads, trace_dependencies, runner_capabilities, invocation_requirements) =
        trace_workload_inputs(rig_context.as_ref(), ctx.extension_id.as_deref());
    warn_rig_owned_trace_workload_expansion(rig_context.as_ref(), ctx.extension_id.as_deref());
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
                invocation_requirements,
                public_preview: None,
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
) -> (
    Vec<PathBuf>,
    Vec<rig::TraceDependencySpec>,
    Vec<String>,
    InvocationRequirements,
) {
    let Some((context, id)) = rig_context.zip(extension_id) else {
        return (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            InvocationRequirements::default(),
        );
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
        rig::invocation_requirements_for_extension_workloads(
            &context.rig_spec,
            rig::RigWorkloadKind::Trace,
            id,
        ),
    )
}

fn warn_rig_owned_trace_workload_expansion(
    rig_context: Option<&TraceRigContext>,
    extension_id: Option<&str>,
) {
    for warning in rig_owned_trace_workload_expansion_warnings(rig_context, extension_id) {
        eprintln!("{warning}");
    }
}

fn rig_owned_trace_workload_expansion_warnings(
    rig_context: Option<&TraceRigContext>,
    extension_id: Option<&str>,
) -> Vec<String> {
    let Some((context, id)) = rig_context.zip(extension_id) else {
        return Vec::new();
    };
    let Some((root_label, root)) = context
        .rig_package_root
        .as_ref()
        .map(|root| ("rig package root", root))
        .or_else(|| {
            context
                .rig_config_root
                .as_ref()
                .map(|root| ("rig config root", root))
        })
    else {
        return Vec::new();
    };

    rig::workload_path_expansions_for_extension(
        &context.rig_spec,
        rig::RigWorkloadKind::Trace,
        context.rig_package_root.as_deref(),
        id,
    )
    .into_iter()
    .filter(|expansion| !path_is_under(&expansion.expanded_path, root))
    .map(|expansion| {
        let freshness = if expansion.expanded_path.is_file() {
            " If this is stale, refresh/reinstall the rig package or sync edits to the executed path before rerunning."
        } else {
            " Refresh/reinstall the rig package or check the expanded path before rerunning."
        };
        format!(
            "Warning: rig `{}` owns trace workload `{}`, but it expands outside the {root_label}. Rig source root: `{}`. Executed extra workload path: `{}`.{freshness}",
            context.rig_spec.id,
            expansion.declared_path,
            root.display(),
            expansion.expanded_path.display(),
        )
    })
    .collect()
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    let normalized_path = path.components().collect::<PathBuf>();
    let normalized_root = root.components().collect::<PathBuf>();
    normalized_path == normalized_root || normalized_path.starts_with(normalized_root)
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

/// Re-exported from core so existing CLI call sites keep using the
/// `trace::PersistedRunRetrieval` path while the type is owned by
/// `core::lab_routing`.
pub(super) use homeboy::core::lab_routing::PersistedRunRetrieval;

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
mod run_phase_tests;
#[cfg(test)]
mod tests;

use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceSpanDefinition;

use super::workload::trace_workload_scenario_id;
use super::{trace_scenario, TraceArgs, TraceRigContext};

const DEFAULT_TRACE_PHASE_PRESET: &str = "default";

pub(super) fn cli_span_definitions_for_args(
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

pub(super) fn span_definitions_for_args(
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

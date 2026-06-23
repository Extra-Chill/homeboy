//! Trace scenario discovery (list-only) workflow.

use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension::{resolve_execution_context, ExtensionCapability, RunnerOutput};

use super::super::canonicality::TraceCanonicalPolicy;
use super::super::parsing::{parse_trace_list_str, TraceList};
use super::runner::{build_trace_runner, trace_is_unclaimed};
use super::types::{TraceListWorkflowArgs, TraceRunWorkflowArgs};

pub fn run_trace_list_workflow(
    component: &Component,
    args: TraceListWorkflowArgs,
    run_dir: &RunDir,
) -> Result<TraceList> {
    if component.has_script(ExtensionCapability::Trace) {
        let source_path = crate::core::extension::component_script::source_path(
            component,
            args.path_override.as_deref(),
        );
        let output = crate::core::extension::component_script::run_component_scripts_with_run_dir(
            component,
            ExtensionCapability::Trace,
            &source_path,
            run_dir,
            true,
            &[("HOMEBOY_TRACE_LIST_ONLY".to_string(), "1".to_string())],
            &[],
        )?;
        return trace_list_from_output(run_dir, TraceListOutput::from(output));
    }

    let execution_context = match resolve_execution_context(component, ExtensionCapability::Trace) {
        Ok(execution_context) => Some(execution_context),
        Err(error) if trace_is_unclaimed(&error) => None,
        Err(error) => return Err(error),
    };
    let runner_args = TraceRunWorkflowArgs {
        component_label: args.component_label.clone(),
        component_id: args.component_id,
        path_override: args.path_override,
        settings: args.settings,
        runner_inputs: args.runner_inputs,
        scenario_id: String::new(),
        json_summary: false,
        rig_id: args.rig_id,
        overlays: Vec::new(),
        keep_overlay: false,
        span_definitions: Vec::new(),
        baseline_flags: BaselineFlags {
            baseline: false,
            ignore_baseline: true,
            ratchet: false,
        },
        regression_threshold_percent: super::super::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        regression_min_delta_ms: super::super::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        canonical_policy: TraceCanonicalPolicy::Development,
        checkout_provenance: None,
    };
    let output = build_trace_runner(
        execution_context.as_ref(),
        component,
        &runner_args,
        run_dir,
        true,
    )?;
    trace_list_from_output(run_dir, TraceListOutput::from(output))
}

struct TraceListOutput {
    exit_code: i32,
    success: bool,
    stdout: String,
    stderr: String,
}

impl From<crate::core::extension::component_script::ComponentScriptOutput> for TraceListOutput {
    fn from(output: crate::core::extension::component_script::ComponentScriptOutput) -> Self {
        Self {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

impl From<RunnerOutput> for TraceListOutput {
    fn from(output: RunnerOutput) -> Self {
        Self {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

fn trace_list_from_output(run_dir: &RunDir, output: TraceListOutput) -> Result<TraceList> {
    if output.success {
        return parse_trace_list_output(run_dir, &output.stdout);
    }

    Err(trace_list_error(
        output.exit_code,
        &output.stdout,
        &output.stderr,
    ))
}

fn trace_list_error(exit_code: i32, stdout: &str, stderr: &str) -> Error {
    Error::validation_invalid_argument(
        "trace_list",
        format!("trace scenario discovery failed with exit code {exit_code}"),
        Some(format!("stdout:\n{stdout}\n\nstderr:\n{stderr}")),
        None,
    )
}

fn parse_trace_list_output(run_dir: &RunDir, stdout: &str) -> Result<TraceList> {
    let results_path = run_dir.step_file(run_dir::files::TRACE_RESULTS);
    if results_path.exists() {
        let content = std::fs::read_to_string(&results_path).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to read trace list file {}: {}",
                    results_path.display(),
                    e
                ),
                Some("trace.list.read".to_string()),
            )
        })?;
        return parse_trace_list_str(&content);
    }

    parse_trace_list_str(stdout)
}

//! Trace runner construction and failure/baseline helpers.

use std::path::{Path, PathBuf};

use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, ErrorCode, Result};
use crate::core::extension::{
    build_scenario_runner, stderr_tail, ExtensionExecutionContext, RunnerOutput,
    ScenarioRunnerOptions,
};
use crate::core::paths;

use super::super::attach::TraceAttachment;
use super::super::generic_runner::run_generic_trace_runner;
use super::super::parsing::TraceResults;
use super::super::preflight::preflight_trace_runner_capabilities;
use super::super::probes::TraceProbeConfig;
use super::types::{TraceRunFailure, TraceRunWorkflowArgs};

pub(crate) fn build_trace_runner(
    execution_context: Option<&ExtensionExecutionContext>,
    component: &Component,
    args: &TraceRunWorkflowArgs,
    run_dir: &RunDir,
    list_only: bool,
) -> Result<RunnerOutput> {
    let artifact_dir = run_dir.path().join("artifacts");
    std::fs::create_dir_all(&artifact_dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to create trace artifact dir {}: {}",
                artifact_dir.display(),
                e
            ),
            Some("trace.artifacts.create".to_string()),
        )
    })?;

    let Some(execution_context) = execution_context else {
        preflight_trace_runner_capabilities(None, &args.runner_inputs.runner_capabilities)?;
        return run_generic_trace_runner(component, args, run_dir, &artifact_dir, list_only);
    };

    preflight_trace_runner_capabilities(
        Some(execution_context),
        &args.runner_inputs.runner_capabilities,
    )?;

    let mut runner = build_scenario_runner(ScenarioRunnerOptions {
        execution_context,
        component,
        path_override: args.path_override.clone(),
        settings: &args.settings,
        settings_json: &args.runner_inputs.json_settings,
        run_dir,
        results_env: Some((
            "HOMEBOY_TRACE_RESULTS_FILE",
            run_dir.step_file(run_dir::files::TRACE_RESULTS),
        )),
        scenario_env: Some(("HOMEBOY_TRACE_SCENARIO", &args.scenario_id)),
        artifact_env: Some(("HOMEBOY_TRACE_ARTIFACT_DIR", &artifact_dir)),
        list_only_env: Some(("HOMEBOY_TRACE_LIST_ONLY", list_only)),
        extra_workloads_env: Some((
            "HOMEBOY_TRACE_EXTRA_WORKLOADS",
            &args.runner_inputs.workload_paths,
            "trace_workloads",
        )),
        env_provider_extensions: &[],
        invocation_requirements: args.runner_inputs.invocation_requirements.clone(),
    })?;

    if let Some(rig_id) = &args.rig_id {
        runner = runner.env("HOMEBOY_TRACE_RIG_ID", rig_id);
    }
    if let Some(path) = &args.path_override {
        runner = runner.env("HOMEBOY_TRACE_COMPONENT_PATH", path);
    }
    if !args.runner_inputs.attachments.is_empty() {
        let attachments_json =
            serde_json::to_string(&args.runner_inputs.attachments).map_err(|e| {
                Error::internal_json(
                    format!("Failed to serialize trace attachments: {e}"),
                    Some("trace.attach.serialize".to_string()),
                )
            })?;
        runner = runner.env("HOMEBOY_TRACE_ATTACHMENTS", &attachments_json);
    }
    for (key, value) in &args.runner_inputs.env {
        runner = runner.env(key, value);
    }

    runner.run()
}

pub fn trace_is_unclaimed(error: &Error) -> bool {
    error.code == ErrorCode::ExtensionUnsupported
        || (error.code == ErrorCode::ValidationInvalidArgument
            && error
                .message
                .contains("has no linked extensions that provide trace support"))
}

pub(super) fn trace_probes_with_fswatch_attachments(
    probes: &[TraceProbeConfig],
    attachments: &[TraceAttachment],
) -> Vec<TraceProbeConfig> {
    let mut merged = probes.to_vec();
    for attachment in attachments {
        if attachment.kind != "fswatch" {
            continue;
        }
        let already_watched = merged.iter().any(|probe| match probe {
            TraceProbeConfig::FileWatch { path, .. } => path == &attachment.target,
            _ => false,
        });
        if !already_watched {
            merged.push(TraceProbeConfig::FileWatch {
                path: attachment.target.clone(),
                interval_ms: None,
            });
        }
    }
    merged
}

/// Resolve the directory that holds the trace baseline `homeboy.json`.
///
/// Non-rig traces keep the historical component-local behavior — the baseline
/// is co-located with the project's `homeboy.json` in the component checkout.
/// Rig-owned traces store baselines in the rig state directory so that
/// `homeboy trace --rig <id>` against an unrelated component checkout (e.g.
/// `example-org/studio`) never creates or mutates a `homeboy.json` inside that
/// repo. See Extra-Chill/homeboy#2329.
pub(super) fn resolve_trace_baseline_root(
    component_path: &str,
    rig_id: Option<&str>,
) -> Result<PathBuf> {
    match rig_id {
        Some(id) => {
            let root = paths::rig_baseline_root(id)?;
            std::fs::create_dir_all(&root).map_err(|e| {
                Error::internal_io(
                    format!(
                        "Failed to create rig baseline root {}: {}",
                        root.display(),
                        e
                    ),
                    Some("trace.baseline.rig_root.create".to_string()),
                )
            })?;
            Ok(root)
        }
        None => Ok(PathBuf::from(component_path)),
    }
}

pub(super) fn failure_from_output(
    args: &TraceRunWorkflowArgs,
    output: &RunnerOutput,
    artifact_dir: Option<&Path>,
    results: Option<&TraceResults>,
) -> TraceRunFailure {
    let child = output.child_resource.as_ref().map(|summary| &summary.child);
    let last_event = results.and_then(last_observed_homeboy_event);
    TraceRunFailure {
        component_id: args.component_id.clone(),
        path_override: args.path_override.clone(),
        scenario_id: args.scenario_id.clone(),
        exit_code: output.exit_code,
        stderr_excerpt: stderr_tail(&output.stderr),
        current_phase: last_event.clone(),
        child_pid: child.map(|child| child.root_pid),
        child_command: child.map(|child| child.command_label.clone()),
        recipe_path: recipe_path_from_args(args),
        artifact_root: artifact_dir.map(|path| path.to_string_lossy().to_string()),
        last_observed_homeboy_event: last_event,
        cleanup_succeeded: output.child_resource.as_ref().map(|_| true),
    }
}

pub(super) fn recipe_path_from_args(args: &TraceRunWorkflowArgs) -> Option<String> {
    // Prefer an explicit recipe-named JSON setting; fall back to the first
    // workload path. Kept as a straight-line scan rather than an iterator
    // combinator chain so it does not parallel unrelated artifact-path lookups
    // in the report subsystem (#5364) — the two share no domain types and a
    // generic helper would couple unrelated subsystems for no real saving.
    for (key, value) in &args.runner_inputs.json_settings {
        if !key.to_ascii_lowercase().contains("recipe") {
            continue;
        }
        if let Some(recipe) = value.as_str() {
            return Some(recipe.to_string());
        }
    }

    let first_workload = args.runner_inputs.workload_paths.first()?;
    Some(first_workload.to_string_lossy().to_string())
}

fn last_observed_homeboy_event(results: &TraceResults) -> Option<String> {
    results
        .timeline
        .iter()
        .max_by_key(|event| event.t_ms)
        .map(|event| format!("{}.{}", event.source, event.event))
}

//! Trace workflows: invoke extension runners, parse JSON, preserve artifacts.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, ErrorCode, Result};
use crate::core::extension::trace::baseline::TraceBaselineComparison;
use crate::core::extension::RunnerOutput;
use crate::core::extension::{
    build_scenario_runner, path_list_env_value, resolve_execution_context, stderr_tail,
    ExtensionCapability, ExtensionExecutionContext, ScenarioRunnerOptions,
};
use crate::core::paths;
use crate::core::rig::RigStateSnapshot;

use super::attach::{append_attach_observations, observe_trace_attachments, TraceAttachment};
use super::overlay::{
    acquire_trace_overlay_locks, apply_trace_overlays, cleanup_after_overlay_error,
    cleanup_trace_overlays, TraceOverlayRequest,
};

use super::parsing::{
    parse_trace_list_str, parse_trace_results_file, TraceAssertion, TraceAssertionStatus,
    TraceList, TraceResults, TraceSpanDefinition, TraceStatus,
};
use super::probes::{ActiveTraceProbes, TraceProbeConfig};

#[derive(Debug, Clone)]
pub struct TraceRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub runner_inputs: TraceRunnerInputs,
    pub scenario_id: String,
    pub json_summary: bool,
    pub rig_id: Option<String>,
    pub overlays: Vec<TraceOverlayRequest>,
    pub keep_overlay: bool,
    pub span_definitions: Vec<TraceSpanDefinition>,
    pub baseline_flags: BaselineFlags,
    pub regression_threshold_percent: f64,
    pub regression_min_delta_ms: u64,
}

#[derive(Debug, Clone)]
pub struct TraceListWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub runner_inputs: TraceRunnerInputs,
    pub rig_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TraceRunnerInputs {
    pub json_settings: Vec<(String, serde_json::Value)>,
    pub env: Vec<(String, String)>,
    pub workload_paths: Vec<PathBuf>,
    pub probes: Vec<TraceProbeConfig>,
    pub attachments: Vec<TraceAttachment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<TraceResults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<TraceRunFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TraceOverlay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<TraceBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    pub path: String,
    pub component_path: String,
    pub touched_files: Vec<String>,
    pub kept: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceRunFailure {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_override: Option<String>,
    pub scenario_id: String,
    pub exit_code: i32,
    pub stderr_excerpt: String,
}

pub fn run_trace_workflow(
    component: &Component,
    args: TraceRunWorkflowArgs,
    run_dir: &RunDir,
    rig_state: Option<RigStateSnapshot>,
) -> Result<TraceRunWorkflowResult> {
    if component.has_script(ExtensionCapability::Trace) {
        return run_trace_workflow_with_component_script(component, args, run_dir, rig_state);
    }

    let execution_context = match resolve_execution_context(component, ExtensionCapability::Trace) {
        Ok(execution_context) => Some(execution_context),
        Err(error) if trace_is_unclaimed(&error) => None,
        Err(error) => return Err(error),
    };
    run_trace_workflow_with_context(
        execution_context.as_ref(),
        component,
        args,
        run_dir,
        rig_state,
    )
}

fn run_trace_workflow_with_component_script(
    component: &Component,
    args: TraceRunWorkflowArgs,
    run_dir: &RunDir,
    rig_state: Option<RigStateSnapshot>,
) -> Result<TraceRunWorkflowResult> {
    let component_path = args
        .path_override
        .as_deref()
        .unwrap_or(component.local_path.as_str());
    let source_path = Path::new(component_path);
    let _overlay_locks = if args.overlays.is_empty() {
        None
    } else {
        Some(acquire_trace_overlay_locks(&args.overlays, run_dir)?)
    };
    let applied_overlays = apply_trace_overlays(&args.overlays, args.keep_overlay)?;
    let script_output =
        crate::core::extension::component_script::run_component_scripts_with_run_dir(
            component,
            ExtensionCapability::Trace,
            source_path,
            run_dir,
            true,
            &[
                (
                    "HOMEBOY_TRACE_SCENARIO".to_string(),
                    args.scenario_id.clone(),
                ),
                ("HOMEBOY_TRACE_LIST_ONLY".to_string(), "0".to_string()),
            ],
            &[],
        );
    if !args.keep_overlay {
        cleanup_trace_overlays(&applied_overlays)?
    }
    let script_output = script_output?;
    let results_path = run_dir.step_file(run_dir::files::TRACE_RESULTS);
    let mut results = if results_path.exists() {
        let mut parsed = parse_trace_results_file(&results_path)?;
        if parsed.rig.is_none() {
            parsed.rig = rig_state;
        }
        Some(parsed)
    } else {
        None
    };
    if let Some(parsed) = results.as_mut() {
        super::spans::apply_span_definitions(parsed, &args.span_definitions);
        super::assertions::apply_temporal_assertions(parsed);
        persist_trace_results(&results_path, parsed)?;
    }
    let status = results
        .as_ref()
        .map(|r| r.status.as_str().to_string())
        .unwrap_or_else(|| {
            if script_output.success {
                "pass"
            } else {
                "error"
            }
            .to_string()
        });
    let exit_code = if script_output.success {
        if status == "pass" {
            0
        } else {
            1
        }
    } else {
        script_output.exit_code
    };
    let failure = (!script_output.success).then(|| TraceRunFailure {
        component_id: args.component_id.clone(),
        path_override: args.path_override.clone(),
        scenario_id: args.scenario_id.clone(),
        exit_code: script_output.exit_code,
        stderr_excerpt: stderr_tail(&script_output.stderr),
    });

    Ok(TraceRunWorkflowResult {
        status,
        component: args.component_label,
        exit_code,
        results,
        failure,
        overlays: applied_overlays
            .into_iter()
            .map(|overlay| TraceOverlay {
                variant: overlay.variant,
                component_id: overlay.component_id,
                path: overlay.patch_path.to_string_lossy().to_string(),
                component_path: overlay.component_path.to_string_lossy().to_string(),
                touched_files: overlay.touched_files,
                kept: overlay.keep,
            })
            .collect(),
        baseline_comparison: None,
        hints: Some(vec![
            "Component scripts use the extension runner env contract without extension resolution."
                .to_string(),
        ]),
    })
}

fn run_trace_workflow_with_context(
    execution_context: Option<&ExtensionExecutionContext>,
    component: &Component,
    args: TraceRunWorkflowArgs,
    run_dir: &RunDir,
    rig_state: Option<RigStateSnapshot>,
) -> Result<TraceRunWorkflowResult> {
    let component_path = args
        .path_override
        .as_deref()
        .unwrap_or(component.local_path.as_str());
    let _overlay_locks = if args.overlays.is_empty() {
        None
    } else {
        Some(acquire_trace_overlay_locks(&args.overlays, run_dir)?)
    };
    let applied_overlays = apply_trace_overlays(&args.overlays, args.keep_overlay)?;
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
    let probe_configs = trace_probes_with_fswatch_attachments(
        &args.runner_inputs.probes,
        &args.runner_inputs.attachments,
    );
    let active_probes =
        ActiveTraceProbes::start_with_artifact_dir(&probe_configs, Some(artifact_dir.clone()))?;
    let started_at = Instant::now();
    let mut attach_observations =
        observe_trace_attachments(&args.runner_inputs.attachments, "before", started_at);
    let runner_output =
        match build_trace_runner(execution_context, component, &args, run_dir, false) {
            Ok(output) => output,
            Err(error) => {
                return cleanup_after_overlay_error(&applied_overlays, args.keep_overlay, error)
            }
        };
    let probe_events = active_probes.stop();
    attach_observations.extend(observe_trace_attachments(
        &args.runner_inputs.attachments,
        "after",
        started_at,
    ));
    if !args.keep_overlay {
        cleanup_trace_overlays(&applied_overlays)?
    }
    let results_path = run_dir.step_file(run_dir::files::TRACE_RESULTS);
    let mut results = if results_path.exists() {
        let mut parsed = parse_trace_results_file(&results_path)?;
        if parsed.rig.is_none() {
            parsed.rig = rig_state;
        }
        Some(parsed)
    } else {
        None
    };
    let failure = (!runner_output.success).then(|| failure_from_output(&args, &runner_output));
    if let Some(parsed) = results.as_mut() {
        parsed.timeline.extend(probe_events);
        parsed.timeline.sort_by_key(|event| event.t_ms);
        append_attach_observations(parsed, run_dir, &attach_observations)?;
        super::spans::apply_span_definitions(parsed, &args.span_definitions);
        super::assertions::apply_temporal_assertions(parsed);
        validate_declared_trace_artifacts(parsed, run_dir, &artifact_dir);
        persist_trace_results(&results_path, parsed)?;
    }

    let status = results
        .as_ref()
        .map(|r| r.status.as_str().to_string())
        .unwrap_or_else(|| {
            if runner_output.success {
                "pass"
            } else {
                "error"
            }
            .to_string()
        });
    let exit_code = if runner_output.success {
        if status == "pass" {
            0
        } else {
            1
        }
    } else {
        runner_output.exit_code
    };
    let rig_id = args.rig_id.as_deref();
    let baseline_root = resolve_trace_baseline_root(component_path, rig_id)?;
    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;
    let mut hints = Vec::new();
    let has_baseline_items = results
        .as_ref()
        .is_some_and(|parsed| !parsed.span_results.is_empty() || !parsed.assertions.is_empty());

    if args.baseline_flags.baseline && status == "pass" && has_baseline_items {
        if let Some(ref parsed) = results {
            let _ =
                super::baseline::save_baseline(&baseline_root, &args.component_id, parsed, rig_id)?;
        }
    }
    if has_baseline_items && !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline {
        if let Some(ref parsed) = results {
            if let Some(existing) = super::baseline::load_baseline(&baseline_root, rig_id) {
                let comparison = super::baseline::compare(
                    parsed,
                    &existing,
                    args.regression_threshold_percent,
                    args.regression_min_delta_ms,
                );
                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if comparison.has_improvements && args.baseline_flags.ratchet {
                    let _ = super::baseline::save_baseline(
                        &baseline_root,
                        &args.component_id,
                        parsed,
                        rig_id,
                    );
                }
                baseline_comparison = Some(comparison);
            }
        }
    }

    let trace_invocation = match rig_id {
        Some(id) => format!(
            "homeboy trace {} {} --rig {}",
            args.component_id, args.scenario_id, id
        ),
        None => format!("homeboy trace {} {}", args.component_id, args.scenario_id),
    };
    if has_baseline_items && !args.baseline_flags.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save trace baseline: {} --baseline",
            trace_invocation
        ));
    }
    if baseline_comparison.is_some() && !args.baseline_flags.ratchet {
        hints.push(format!(
            "Auto-update trace baseline on improvement: {} --ratchet",
            trace_invocation
        ));
    }
    if let Some(ref cmp) = baseline_comparison {
        if cmp.regression {
            hints.push(format!(
                "Trace span regression threshold: {}% and {}ms. Raise them with --regression-threshold=<PCT> or --regression-min-delta-ms=<MS> if expected.",
                cmp.threshold_percent, cmp.min_delta_ms
            ));
        }
    }

    let exit_code = baseline_exit_override.unwrap_or(exit_code);

    Ok(TraceRunWorkflowResult {
        status,
        component: args.component_label,
        exit_code,
        results,
        failure,
        overlays: applied_overlays
            .into_iter()
            .map(|overlay| TraceOverlay {
                variant: overlay.variant,
                component_id: overlay.component_id,
                path: overlay.patch_path.to_string_lossy().to_string(),
                component_path: overlay.component_path.to_string_lossy().to_string(),
                touched_files: overlay.touched_files,
                kept: overlay.keep,
            })
            .collect(),
        baseline_comparison,
        hints: if hints.is_empty() { None } else { Some(hints) },
    })
}

fn validate_declared_trace_artifacts(
    results: &mut TraceResults,
    run_dir: &RunDir,
    artifact_dir: &Path,
) {
    let missing = results
        .artifacts
        .iter()
        .filter(|artifact| {
            resolve_declared_trace_artifact_path(&artifact.path, run_dir, artifact_dir).is_none()
        })
        .map(|artifact| artifact.path.clone())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return;
    }

    results.status = TraceStatus::Error;
    results.failure = Some(format!(
        "missing declared trace artifact{}: {}",
        if missing.len() == 1 { "" } else { "s" },
        missing.join(", ")
    ));
    for path in missing {
        results.assertions.push(TraceAssertion {
            id: format!("trace_artifact_exists:{}", path),
            status: TraceAssertionStatus::Error,
            message: Some(format!("Declared trace artifact is missing: {path}")),
            details: Some(serde_json::json!({ "path": path })),
        });
    }
}

pub fn resolve_declared_trace_artifact_path(
    path: &str,
    run_dir: &RunDir,
    artifact_dir: &Path,
) -> Option<PathBuf> {
    let relative = Path::new(path);
    if relative.is_absolute() {
        return relative.exists().then(|| relative.to_path_buf());
    }
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }

    [run_dir.path().join(relative), artifact_dir.join(relative)]
        .into_iter()
        .find(|candidate| candidate.exists())
}

fn persist_trace_results(path: &Path, results: &TraceResults) -> Result<()> {
    let content = serde_json::to_string_pretty(results).map_err(|e| {
        Error::internal_json(
            format!("Failed to serialize trace results JSON: {}", e),
            Some("trace.results.serialize".to_string()),
        )
    })?;
    std::fs::write(path, content).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write trace results file {}: {}",
                path.display(),
                e
            ),
            Some("trace.results.write".to_string()),
        )
    })
}

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
        regression_threshold_percent: super::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        regression_min_delta_ms: super::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
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
        return run_generic_trace_runner(component, args, run_dir, &artifact_dir, list_only);
    };

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
        invocation_requirements: crate::core::engine::invocation::InvocationRequirements::default(),
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

fn trace_probes_with_fswatch_attachments(
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

fn run_generic_trace_runner(
    component: &Component,
    args: &TraceRunWorkflowArgs,
    run_dir: &RunDir,
    artifact_dir: &Path,
    list_only: bool,
) -> Result<RunnerOutput> {
    let component_path = args
        .path_override
        .as_deref()
        .unwrap_or(component.local_path.as_str());
    let workloads =
        discover_generic_trace_workloads(Path::new(component_path), &args.runner_inputs)?;

    if list_only {
        let scenarios = workloads
            .iter()
            .map(|path| {
                serde_json::json!({
                    "id": trace_workload_scenario_id(path),
                    "source": path.to_string_lossy()
                })
            })
            .collect::<Vec<_>>();
        let stdout = serde_json::json!({
            "component_id": args.component_id,
            "scenarios": scenarios
        })
        .to_string();
        return Ok(RunnerOutput {
            exit_code: 0,
            success: true,
            stdout,
            stderr: String::new(),
        });
    }

    let Some(workload) = workloads
        .iter()
        .find(|path| trace_workload_scenario_id(path) == args.scenario_id)
    else {
        return Ok(RunnerOutput {
            exit_code: 3,
            success: false,
            stdout: String::new(),
            stderr: format!("unknown trace scenario {}", args.scenario_id),
        });
    };

    let mut command = generic_trace_workload_command(workload);
    command.current_dir(component_path);
    command.envs(generic_trace_env(
        component,
        args,
        run_dir,
        artifact_dir,
        &workloads,
        list_only,
    )?);
    let output = command.output().map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to run generic trace workload {}: {}",
                workload.display(),
                e
            ),
            Some("trace.generic.run".to_string()),
        )
    })?;

    Ok(RunnerOutput {
        exit_code: output.status.code().unwrap_or(1),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn discover_generic_trace_workloads(
    component_path: &Path,
    runner_inputs: &TraceRunnerInputs,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for dir in [
        component_path.join("traces"),
        component_path.join("scripts/trace"),
    ] {
        if !dir.is_dir() {
            continue;
        }
        let entries = std::fs::read_dir(&dir).map_err(|e| {
            Error::internal_io(
                format!("Failed to read trace workload dir {}: {}", dir.display(), e),
                Some("trace.generic.discover".to_string()),
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                Error::internal_io(
                    format!(
                        "Failed to read trace workload entry in {}: {}",
                        dir.display(),
                        e
                    ),
                    Some("trace.generic.discover".to_string()),
                )
            })?;
            let path = entry.path();
            if is_generic_trace_workload(&path) {
                paths.push(path);
            }
        }
    }

    paths.extend(runner_inputs.workload_paths.iter().cloned());
    if let Some(extra) = std::env::var_os("HOMEBOY_TRACE_EXTRA_WORKLOADS") {
        paths.extend(std::env::split_paths(&extra));
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn is_generic_trace_workload(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    (name.ends_with(".trace.mjs") || name.ends_with(".trace.sh") || name.ends_with(".trace.py"))
        || matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("mjs" | "sh" | "py")
        ) && path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some("trace")
}

fn trace_workload_scenario_id(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    for suffix in [".trace.mjs", ".trace.sh", ".trace.py", ".mjs", ".sh", ".py"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    name.to_string()
}

fn generic_trace_workload_command(workload: &Path) -> Command {
    match workload.extension().and_then(|ext| ext.to_str()) {
        Some("mjs") => {
            let mut command = Command::new("node");
            command.arg(workload);
            command
        }
        Some("py") => {
            let mut command = Command::new("python3");
            command.arg(workload);
            command
        }
        Some("sh") => {
            let mut command = Command::new("sh");
            command.arg(workload);
            command
        }
        _ => Command::new(workload),
    }
}

fn generic_trace_env(
    component: &Component,
    args: &TraceRunWorkflowArgs,
    run_dir: &RunDir,
    artifact_dir: &Path,
    workloads: &[PathBuf],
    list_only: bool,
) -> Result<Vec<(String, String)>> {
    let component_path = args
        .path_override
        .as_deref()
        .unwrap_or(component.local_path.as_str());
    let mut env = run_dir.legacy_env_vars();
    env.extend([
        (
            "HOMEBOY_EXTENSION_ID".to_string(),
            "generic-shell".to_string(),
        ),
        (
            "HOMEBOY_COMPONENT_ID".to_string(),
            args.component_id.clone(),
        ),
        (
            "HOMEBOY_COMPONENT_PATH".to_string(),
            component_path.to_string(),
        ),
        (
            "HOMEBOY_TRACE_RESULTS_FILE".to_string(),
            run_dir
                .step_file(run_dir::files::TRACE_RESULTS)
                .to_string_lossy()
                .to_string(),
        ),
        (
            "HOMEBOY_TRACE_SCENARIO".to_string(),
            args.scenario_id.clone(),
        ),
        (
            "HOMEBOY_TRACE_ARTIFACT_DIR".to_string(),
            artifact_dir.to_string_lossy().to_string(),
        ),
        (
            "HOMEBOY_TRACE_LIST_ONLY".to_string(),
            if list_only { "1" } else { "0" }.to_string(),
        ),
        (
            "HOMEBOY_TRACE_EXTRA_WORKLOADS".to_string(),
            extra_workloads_env_value(workloads)?,
        ),
    ]);
    if let Some(rig_id) = &args.rig_id {
        env.push(("HOMEBOY_TRACE_RIG_ID".to_string(), rig_id.clone()));
    }
    if !args.runner_inputs.attachments.is_empty() {
        env.push((
            "HOMEBOY_TRACE_ATTACHMENTS".to_string(),
            serde_json::to_string(&args.runner_inputs.attachments).map_err(|e| {
                Error::internal_json(
                    format!("Failed to serialize trace attachments: {e}"),
                    Some("trace.attach.serialize".to_string()),
                )
            })?,
        ));
    }
    for (key, value) in &args.runner_inputs.env {
        env.push((key.clone(), value.clone()));
    }
    Ok(env)
}

fn extra_workloads_env_value(paths: &[PathBuf]) -> Result<String> {
    path_list_env_value("trace_workloads", paths)
}

/// Resolve the directory that holds the trace baseline `homeboy.json`.
///
/// Non-rig traces keep the historical component-local behavior — the baseline
/// is co-located with the project's `homeboy.json` in the component checkout.
/// Rig-owned traces store baselines in the rig state directory so that
/// `homeboy trace --rig <id>` against an unrelated component checkout (e.g.
/// `Automattic/studio`) never creates or mutates a `homeboy.json` inside that
/// repo. See Extra-Chill/homeboy#2329.
fn resolve_trace_baseline_root(component_path: &str, rig_id: Option<&str>) -> Result<PathBuf> {
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

fn failure_from_output(args: &TraceRunWorkflowArgs, output: &RunnerOutput) -> TraceRunFailure {
    TraceRunFailure {
        component_id: args.component_id.clone(),
        path_override: args.path_override.clone(),
        scenario_id: args.scenario_id.clone(),
        exit_code: output.exit_code,
        stderr_excerpt: stderr_tail(&output.stderr),
    }
}

#[cfg(test)]
mod run_tests {
    include!("run_tests.inc");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_trace_runner() {
        let temp = tempfile::tempdir().unwrap();
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let output = build_trace_runner(
            None,
            &component,
            &test_run_args(temp.path()),
            &run_dir,
            false,
        )
        .unwrap();
        assert!(!output.success);
        assert_eq!(output.exit_code, 3);
        run_dir.cleanup();
    }

    #[test]
    fn test_run_trace_list_workflow() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().unwrap();
            let component = test_component(temp.path());
            let run_dir = RunDir::create().unwrap();
            let list = run_trace_list_workflow(
                &component,
                TraceListWorkflowArgs {
                    component_label: "example".to_string(),
                    component_id: "example".to_string(),
                    path_override: Some(temp.path().to_string_lossy().to_string()),
                    settings: Vec::new(),
                    runner_inputs: TraceRunnerInputs::default(),
                    rig_id: None,
                },
                &run_dir,
            )
            .unwrap();
            assert!(list.scenarios.is_empty());
            run_dir.cleanup();
        });
    }

    #[test]
    fn test_run_trace_workflow() {
        let temp = tempfile::tempdir().unwrap();
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let result =
            run_trace_workflow(&component, test_run_args(temp.path()), &run_dir, None).unwrap();
        assert_eq!(result.status, "error");
        assert_eq!(result.exit_code, 3);
        run_dir.cleanup();
    }

    #[test]
    fn test_trace_is_unclaimed() {
        let unsupported = Error::new(
            ErrorCode::ExtensionUnsupported,
            "No extension provider configured for component 'example'",
            serde_json::json!({}),
        );
        assert!(trace_is_unclaimed(&unsupported));
    }

    #[test]
    fn resolve_trace_baseline_root_without_rig_returns_component_path() {
        let temp = tempfile::tempdir().unwrap();
        let component_path = temp.path().to_string_lossy().to_string();
        let root = resolve_trace_baseline_root(&component_path, None).unwrap();
        assert_eq!(root, PathBuf::from(&component_path));
        // Crucially, no homeboy.json gets created in the component checkout
        // just by resolving — that only happens when a baseline is saved.
        assert!(!temp.path().join("homeboy.json").exists());
    }

    #[test]
    fn resolve_trace_baseline_root_with_rig_uses_rig_state_dir_and_skips_component_path() {
        let temp = tempfile::tempdir().unwrap();
        let component_path = temp.path().to_string_lossy().to_string();
        let rig_id = format!("__hb-trace-baseline-test-{}", std::process::id());

        let root = resolve_trace_baseline_root(&component_path, Some(&rig_id)).unwrap();

        assert!(
            root.ends_with(format!("{}.state/baselines", rig_id)),
            "rig baseline root should live under <id>.state/baselines, got {}",
            root.display()
        );
        assert!(
            root.exists(),
            "rig baseline root should be created on resolve"
        );
        assert!(
            !root.starts_with(temp.path()),
            "rig baseline root must not live inside the component checkout"
        );
        assert!(
            !temp.path().join("homeboy.json").exists(),
            "resolving a rig baseline root must not touch component homeboy.json"
        );

        // Cleanup: best-effort remove the rig state dir we created.
        if let Some(state_dir) = root.parent() {
            let _ = std::fs::remove_dir_all(state_dir);
        }
    }

    #[test]
    fn rig_save_baseline_does_not_write_component_homeboy_json() {
        use crate::core::extension::trace::baseline;
        use crate::core::extension::trace::parsing::{
            TraceResults, TraceSpanResult, TraceSpanStatus, TraceStatus,
        };

        let temp = tempfile::tempdir().unwrap();
        let component_path = temp.path().to_string_lossy().to_string();
        let rig_id = format!("__hb-trace-save-test-{}", std::process::id());

        let baseline_root = resolve_trace_baseline_root(&component_path, Some(&rig_id)).unwrap();

        let results = TraceResults {
            component_id: "studio".to_string(),
            scenario_id: "create-site".to_string(),
            status: TraceStatus::Pass,
            summary: None,
            failure: None,
            rig: None,
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: vec![TraceSpanResult {
                id: "submit_to_cli".to_string(),
                from: "ui.submit".to_string(),
                to: "cli.start".to_string(),
                status: TraceSpanStatus::Ok,
                duration_ms: Some(120),
                from_t_ms: Some(0),
                to_t_ms: Some(120),
                missing: Vec::new(),
                message: None,
            }],
            assertions: Vec::new(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::new(),
        };

        let written = baseline::save_baseline(&baseline_root, "studio", &results, Some(&rig_id))
            .expect("rig baseline saves into rig state dir");

        assert!(
            written.starts_with(&baseline_root),
            "rig baseline must be written under the rig baseline root, got {}",
            written.display()
        );
        assert!(
            !temp.path().join("homeboy.json").exists(),
            "rig baseline save must not write homeboy.json into the component checkout"
        );

        let loaded = baseline::load_baseline(&baseline_root, Some(&rig_id))
            .expect("rig baseline loads from rig state dir");
        assert_eq!(loaded.metadata.spans[0].id, "submit_to_cli");

        if let Some(state_dir) = baseline_root.parent() {
            let _ = std::fs::remove_dir_all(state_dir);
        }
    }

    #[test]
    fn fswatch_attachments_add_file_watch_probes_without_duplicates() {
        let attachment = TraceAttachment::parse("fswatch:/tmp/auth.json").unwrap();
        let existing_probe = TraceProbeConfig::FileWatch {
            path: "/tmp/auth.json".to_string(),
            interval_ms: Some(50),
        };

        let merged =
            trace_probes_with_fswatch_attachments(&[existing_probe.clone()], &[attachment.clone()]);
        assert_eq!(merged, vec![existing_probe]);

        let merged = trace_probes_with_fswatch_attachments(&[], &[attachment]);
        assert_eq!(
            merged,
            vec![TraceProbeConfig::FileWatch {
                path: "/tmp/auth.json".to_string(),
                interval_ms: None,
            }]
        );
    }

    fn test_component(path: &std::path::Path) -> Component {
        Component {
            id: "example".to_string(),
            local_path: path.to_string_lossy().to_string(),
            ..Default::default()
        }
    }

    fn test_run_args(path: &std::path::Path) -> TraceRunWorkflowArgs {
        TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some(path.to_string_lossy().to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs::default(),
            scenario_id: "missing".to_string(),
            json_summary: false,
            rig_id: None,
            overlays: Vec::new(),
            keep_overlay: false,
            span_definitions: Vec::new(),
            baseline_flags: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold_percent:
                super::super::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
            regression_min_delta_ms: super::super::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        }
    }
}

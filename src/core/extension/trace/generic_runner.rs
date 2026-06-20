use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension::{path_list_env_value, RunnerOutput};

use super::run::{TraceRunWorkflowArgs, TraceRunnerInputs};

pub(super) fn run_generic_trace_runner(
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
            child_resource: None,
            extension_phase_timings: Vec::new(),
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
            child_resource: None,
            extension_phase_timings: Vec::new(),
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
        child_resource: None,
        extension_phase_timings: super::super::runner::read_extension_phase_timings(
            run_dir.path(),
        )?,
    })
}

pub(super) fn discover_generic_trace_workloads(
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

pub(super) fn trace_workload_scenario_id(path: &Path) -> String {
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

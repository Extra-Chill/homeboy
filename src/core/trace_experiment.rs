//! Trace-experiment orchestration core service.
//!
//! Command modules stay thin adapters: they build the trace experiment plan
//! from rig metadata and CLI arguments, then delegate the actual orchestration
//! — setup/teardown command execution and artifact collection — to this core
//! service. Keeping process execution and filesystem mutation here means the
//! command layer never accumulates orchestration weight.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::core::engine::run_dir::RunDir;
use crate::core::extension::trace as extension_trace;
use crate::core::rig;

/// Context required to resolve rig variables when orchestrating a trace
/// experiment. Owns only the data the orchestration needs, decoupled from the
/// command-layer rig context type.
pub struct TraceExperimentContext<'a> {
    pub rig_spec: &'a rig::RigSpec,
    pub package_root: Option<&'a Path>,
}

impl TraceExperimentContext<'_> {
    fn resolve(&self, value: &str) -> String {
        let expanded = rig::expand::expand_vars(self.rig_spec, value);
        match self.package_root {
            Some(root) => expanded.replace("${package.root}", &root.to_string_lossy()),
            None => expanded,
        }
    }
}

/// Resolve the experiment's declared settings, expanding rig variables in any
/// string values.
pub fn resolve_settings(
    context: &TraceExperimentContext,
    experiment: &rig::TraceExperimentSpec,
) -> Vec<(String, serde_json::Value)> {
    experiment
        .settings
        .iter()
        .map(|(key, value)| {
            let resolved = match value {
                serde_json::Value::String(value) => {
                    serde_json::Value::String(context.resolve(value))
                }
                other => other.clone(),
            };
            (key.clone(), resolved)
        })
        .collect()
}

/// Resolve the experiment's declared environment, expanding rig variables.
pub fn resolve_env(
    context: &TraceExperimentContext,
    experiment: &rig::TraceExperimentSpec,
) -> Vec<(String, String)> {
    experiment
        .env
        .iter()
        .map(|(key, value)| (key.clone(), context.resolve(value)))
        .collect()
}

/// Run a phase (setup/teardown) of a trace experiment by executing each
/// configured command with the experiment environment applied.
pub fn run_phase(
    context: &TraceExperimentContext,
    experiment_name: &str,
    phase: &str,
    commands: &[rig::TraceExperimentCommandSpec],
    experiment_env: &BTreeMap<String, String>,
    run_dir: &RunDir,
) -> crate::core::Result<()> {
    for command_spec in commands {
        let command_text = context.resolve(&command_spec.command);
        let mut command = Command::new(experiment_shell());
        command.arg("-c").arg(&command_text);
        command.env("HOMEBOY_TRACE_EXPERIMENT", experiment_name);
        command.env("HOMEBOY_TRACE_EXPERIMENT_PHASE", phase);
        command.env("HOMEBOY_RUN_DIR", run_dir.path());
        command.env(
            "HOMEBOY_TRACE_ARTIFACT_DIR",
            run_dir.path().join("artifacts"),
        );
        for (key, value) in experiment_env {
            command.env(key, context.resolve(value));
        }
        for (key, value) in &command_spec.env {
            command.env(key, context.resolve(value));
        }
        if let Some(cwd) = &command_spec.cwd {
            command.current_dir(PathBuf::from(context.resolve(cwd)));
        }
        let status = command.status().map_err(|err| {
            crate::core::Error::validation_invalid_argument(
                "--experiment",
                format!(
                    "trace experiment '{}' {} command failed to spawn: {}",
                    experiment_name, phase, err
                ),
                Some(command_text.clone()),
                None,
            )
        })?;
        if !status.success() {
            return Err(crate::core::Error::validation_invalid_argument(
                "--experiment",
                format!(
                    "trace experiment '{}' {} command exited {}",
                    experiment_name,
                    phase,
                    status.code().unwrap_or(-1)
                ),
                Some(command_text),
                None,
            ));
        }
    }
    Ok(())
}

/// Collect the experiment's declared artifacts into the run directory and
/// record them on the workflow result.
pub fn collect_artifacts(
    context: &TraceExperimentContext,
    experiment_name: &str,
    experiment: &rig::TraceExperimentSpec,
    run_dir: &RunDir,
    workflow: &mut extension_trace::TraceRunWorkflowResult,
) -> crate::core::Result<()> {
    let Some(results) = workflow.results.as_mut() else {
        return Ok(());
    };
    for (index, artifact) in experiment.artifacts.iter().enumerate() {
        let (label, source) = match artifact {
            rig::TraceExperimentArtifactSpec::Path(path) => (
                Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("experiment artifact")
                    .to_string(),
                path.as_str(),
            ),
            rig::TraceExperimentArtifactSpec::Detailed { label, path } => {
                (label.clone(), path.as_str())
            }
        };
        let source_path = PathBuf::from(context.resolve(source));
        if !source_path.is_file() {
            return Err(crate::core::Error::validation_invalid_argument(
                "--experiment",
                format!(
                    "trace experiment '{}' artifact '{}' does not exist or is not a file",
                    experiment_name,
                    source_path.display()
                ),
                None,
                None,
            ));
        }
        let file_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact");
        let relative = PathBuf::from("artifacts")
            .join("experiments")
            .join(experiment_name)
            .join(format!("{:02}-{}", index + 1, file_name));
        let destination = run_dir.path().join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                crate::core::Error::internal_io(
                    format!(
                        "Failed to create trace experiment artifact dir {}: {}",
                        parent.display(),
                        err
                    ),
                    Some("trace.experiment.artifact.mkdir".to_string()),
                )
            })?;
        }
        std::fs::copy(&source_path, &destination).map_err(|err| {
            crate::core::Error::internal_io(
                format!(
                    "Failed to collect trace experiment artifact {} to {}: {}",
                    source_path.display(),
                    destination.display(),
                    err
                ),
                Some("trace.experiment.artifact.copy".to_string()),
            )
        })?;
        results.artifacts.push(extension_trace::TraceArtifact {
            label,
            path: relative.to_string_lossy().to_string(),
            kind: None,
        });
    }
    Ok(())
}

/// Create a trace-experiment bundle directory (and any parents), mirroring
/// `mkdir -p`. Experiment bundles accumulate several directories (the bundle
/// root and an overlays subdir), so the create-dir orchestration lives here,
/// keeping the command a thin adapter that only computes which directories it
/// needs.
pub fn prepare_experiment_bundle_dir(dir: &Path) -> crate::core::Result<()> {
    std::fs::create_dir_all(dir).map_err(|err| {
        crate::core::Error::internal_io(
            format!(
                "Failed to create trace experiment bundle {}: {}",
                dir.display(),
                err
            ),
            Some("trace.experiment.mkdir".to_string()),
        )
    })
}

/// Create the overlay subdirectory for a trace-experiment bundle, mirroring
/// `mkdir -p`.
pub fn prepare_experiment_overlay_dir(dir: &Path) -> crate::core::Result<()> {
    std::fs::create_dir_all(dir).map_err(|err| {
        crate::core::Error::internal_io(
            format!(
                "Failed to create overlay bundle dir {}: {}",
                dir.display(),
                err
            ),
            Some("trace.experiment.overlay.mkdir".to_string()),
        )
    })
}

/// Write a pre-rendered text artifact for a trace-experiment bundle. The command
/// layer renders the content; this owns the filesystem write so persistence
/// orchestration never accumulates in the command.
pub fn write_experiment_file(path: &Path, content: &str, context: &str) -> crate::core::Result<()> {
    std::fs::write(path, content).map_err(|err| {
        crate::core::Error::internal_io(
            format!("Failed to write {}: {}", path.display(), err),
            Some(context.to_string()),
        )
    })
}

/// Serialize `value` to pretty JSON (with a trailing newline) and write it to
/// `path` as a trace-experiment bundle artifact.
pub fn write_experiment_json_file<T: Serialize>(
    path: &Path,
    value: &T,
    context: &str,
) -> crate::core::Result<()> {
    let content = serde_json::to_string_pretty(value).map_err(|err| {
        crate::core::Error::internal_json(err.to_string(), Some(context.to_string()))
    })?;
    write_experiment_file(path, &(content + "\n"), context)
}

/// Read the raw bytes of a trace-experiment overlay source file for bundling.
pub fn read_experiment_overlay(source: &Path) -> crate::core::Result<Vec<u8>> {
    std::fs::read(source).map_err(|err| {
        crate::core::Error::internal_io(
            format!(
                "Failed to read trace overlay {} for bundling: {}",
                source.display(),
                err
            ),
            Some("trace.experiment.overlay.read".to_string()),
        )
    })
}

/// Write bundled trace-experiment overlay bytes to `target`.
pub fn write_experiment_overlay(target: &Path, bytes: &[u8]) -> crate::core::Result<()> {
    std::fs::write(target, bytes).map_err(|err| {
        crate::core::Error::internal_io(
            format!(
                "Failed to write bundled trace overlay {}: {}",
                target.display(),
                err
            ),
            Some("trace.experiment.overlay.write".to_string()),
        )
    })
}

/// Read a trace-experiment overlay source file for checksumming.
pub fn read_experiment_overlay_for_checksum(path: &Path) -> crate::core::Result<Vec<u8>> {
    std::fs::read(path).map_err(|err| {
        crate::core::Error::internal_io(
            format!("Failed to read {} for checksum: {}", path.display(), err),
            Some("trace.experiment.overlay.sha256".to_string()),
        )
    })
}

#[cfg(unix)]
fn experiment_shell() -> &'static str {
    "/bin/sh"
}

#[cfg(not(unix))]
fn experiment_shell() -> &'static str {
    "sh"
}

//! Retained Lab Cook output discovery and adoption.
//!
//! This command deliberately accepts only an owning agent-task ID and a path
//! relative to its immutable retained workspace receipt. It never exposes a
//! caller-supplied runner root or creates a diagnostic runner job to inspect it.

use std::path::{Component, Path};

use serde::Serialize;
use serde_json::{json, Value};

use homeboy::agents::agent_task_lifecycle::resolve_retained_workspace;
use homeboy::core::Error;

use super::super::CmdResult;
use super::args::{RetainedArtifactsArgs, RetainedArtifactsCommand};

const RETAINED_ARTIFACTS_SCHEMA: &str = "homeboy/retained-agent-task-artifacts/v1";

#[derive(Serialize)]
struct RetainedWorkspaceOutput {
    schema: &'static str,
    command: &'static str,
    run_id: String,
    runner_id: String,
    runner_job_id: String,
    workspace_status: &'static str,
    attachment_root: String,
    attachment_path_policy: &'static str,
    commands: RetainedArtifactCommands,
}

#[derive(Serialize)]
struct RetainedArtifactCommands {
    attach: String,
    artifacts: String,
}

pub(super) fn run(args: RetainedArtifactsArgs) -> CmdResult<Value> {
    match args.command {
        RetainedArtifactsCommand::Discover { run_id } => discover(&run_id),
        RetainedArtifactsCommand::Attach { run_id, path, name } => attach(&run_id, &path, &name),
    }
}

fn discover(run_id: &str) -> CmdResult<Value> {
    let workspace = resolve_retained_workspace(run_id)?;
    let output = RetainedWorkspaceOutput {
        schema: RETAINED_ARTIFACTS_SCHEMA,
        command: "agent-task.retained-artifacts.discover",
        run_id: workspace.run_id,
        runner_id: workspace.runner_id,
        runner_job_id: workspace.runner_job_id,
        workspace_status: "retained",
        attachment_root: ".".to_string(),
        attachment_path_policy: "repository-relative; absolute and parent paths are rejected",
        commands: RetainedArtifactCommands {
            attach: format!(
                "homeboy agent-task retained-artifacts attach {run_id} --path <relative-path> --name <artifact-name>"
            ),
            artifacts: format!("homeboy runs artifacts {run_id}"),
        },
    };
    Ok((serde_json::to_value(output).unwrap_or(Value::Null), 0))
}

fn attach(run_id: &str, relative_path: &str, name: &str) -> CmdResult<Value> {
    validate_relative_path(relative_path)?;
    let workspace = resolve_retained_workspace(run_id)?;
    let source_path = format!(
        "{}/{}",
        workspace.remote_workspace.trim_end_matches('/'),
        relative_path
    );
    let artifact = crate::commands::runs::attach_runner_artifact(
        workspace.run_id.clone(),
        workspace.runner_id.clone(),
        source_path,
        name.to_string(),
    )?;
    Ok((
        json!({
            "schema": RETAINED_ARTIFACTS_SCHEMA,
            "command": "agent-task.retained-artifacts.attach",
            "run_id": workspace.run_id,
            "runner_id": workspace.runner_id,
            "runner_job_id": workspace.runner_job_id,
            "workspace_status": "retained",
            "relative_path": relative_path,
            "artifact": artifact,
        }),
        0,
    ))
}

fn validate_relative_path(path: &str) -> homeboy::core::Result<()> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(Error::validation_invalid_argument(
            "path",
            "retained artifact path must be a non-empty repository-relative path without parent components",
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_artifact_paths_are_bounded_to_the_workspace() {
        assert!(validate_relative_path("artifacts/result.json").is_ok());
        assert!(validate_relative_path("/tmp/result.json").is_err());
        assert!(validate_relative_path("artifacts/../secret.txt").is_err());
        assert!(validate_relative_path("").is_err());
    }
}

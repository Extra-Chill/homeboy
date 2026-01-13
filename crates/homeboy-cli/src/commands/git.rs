use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::process::Command;

use homeboy::config::ConfigManager;
use homeboy::json::read_json_spec_to_string;

use crate::commands::version;

use super::CmdResult;

#[derive(Args)]
pub struct GitArgs {
    #[command(subcommand)]
    command: GitCommand,
}

#[derive(Subcommand)]
enum GitCommand {
    /// Show git status for a component
    Status {
        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,
    },
    /// Stage all changes and commit
    Commit {
        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,

        /// Commit message (non-JSON mode)
        message: Option<String>,
    },
    /// Push local commits to remote
    Push {
        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,

        /// Push tags as well
        #[arg(long)]
        tags: bool,
    },
    /// Pull remote changes
    Pull {
        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,
    },
    /// Create a git tag
    Tag {
        /// Component ID
        component_id: String,
        /// Tag name (e.g., v0.1.2)
        ///
        /// If omitted, tag defaults to v<component version>.
        tag_name: Option<String>,
        /// Tag message (creates annotated tag)
        #[arg(short, long)]
        message: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitOutput {
    component_id: String,
    path: String,
    action: String,
    success: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

// Bulk input structs

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BulkCommitInput {
    components: Vec<CommitSpec>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitSpec {
    id: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BulkIdsInput {
    component_ids: Vec<String>,
    #[serde(default)]
    tags: bool,
}

// Bulk output structs

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkOutput {
    action: String,
    results: Vec<GitOutput>,
    summary: BulkSummary,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BulkSummary {
    total: usize,
    succeeded: usize,
    failed: usize,
}

// Tagged union for output type

#[derive(Serialize)]
#[serde(untagged)]
pub enum GitCommandOutput {
    Single(GitOutput),
    Bulk(BulkOutput),
}

pub fn run(args: GitArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<GitCommandOutput> {
    match args.command {
        GitCommand::Status { json, component_id } => {
            if let Some(spec) = json {
                let raw = read_json_spec_to_string(&spec)?;
                let input: BulkIdsInput = serde_json::from_str(&raw)
                    .map_err(|e| homeboy::Error::validation_invalid_json(e, Some("parse bulk status input".to_string())))?;
                return bulk_status(input);
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --json for bulk)",
                    None,
                    None,
                )
            })?;
            let (output, code) = status(&id)?;
            Ok((GitCommandOutput::Single(output), code))
        }
        GitCommand::Commit {
            json,
            component_id,
            message,
        } => {
            if let Some(spec) = json {
                let raw = read_json_spec_to_string(&spec)?;
                let input: BulkCommitInput = serde_json::from_str(&raw)
                    .map_err(|e| homeboy::Error::validation_invalid_json(e, Some("parse bulk commit input".to_string())))?;
                return bulk_commit(input);
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --json for bulk)",
                    None,
                    None,
                )
            })?;
            let msg = message.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "message",
                    "Missing message (or use --json for bulk)",
                    None,
                    None,
                )
            })?;
            let (output, code) = commit(&id, &msg)?;
            Ok((GitCommandOutput::Single(output), code))
        }
        GitCommand::Push {
            json,
            component_id,
            tags,
        } => {
            if let Some(spec) = json {
                let raw = read_json_spec_to_string(&spec)?;
                let input: BulkIdsInput = serde_json::from_str(&raw)
                    .map_err(|e| homeboy::Error::validation_invalid_json(e, Some("parse bulk push input".to_string())))?;
                return bulk_push(input);
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --json for bulk)",
                    None,
                    None,
                )
            })?;
            let (output, code) = push(&id, tags)?;
            Ok((GitCommandOutput::Single(output), code))
        }
        GitCommand::Pull { json, component_id } => {
            if let Some(spec) = json {
                let raw = read_json_spec_to_string(&spec)?;
                let input: BulkIdsInput = serde_json::from_str(&raw)
                    .map_err(|e| homeboy::Error::validation_invalid_json(e, Some("parse bulk pull input".to_string())))?;
                return bulk_pull(input);
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --json for bulk)",
                    None,
                    None,
                )
            })?;
            let (output, code) = pull(&id)?;
            Ok((GitCommandOutput::Single(output), code))
        }
        GitCommand::Tag {
            component_id,
            tag_name,
            message,
        } => {
            let derived_tag_name = match tag_name {
                Some(name) => name,
                None => {
                    let (out, _) = version::show_version_output(&component_id)?;
                    format!("v{}", out.version)
                }
            };

            let (output, code) = tag(&component_id, &derived_tag_name, message.as_deref())?;
            Ok((GitCommandOutput::Single(output), code))
        }
    }
}

fn get_component_path(component_id: &str) -> homeboy::Result<String> {
    let component = ConfigManager::load_component(component_id)?;
    Ok(component.local_path)
}

fn execute_git(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").args(args).current_dir(path).output()
}

fn to_exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn status(component_id: &str) -> CmdResult<GitOutput> {
    let path = get_component_path(component_id)?;

    let output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| homeboy::Error::other(e.to_string()))?;

    Ok((
        GitOutput {
            component_id: component_id.to_string(),
            path,
            action: "status".to_string(),
            success: output.status.success(),
            exit_code: to_exit_code(output.status),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        to_exit_code(output.status),
    ))
}

fn commit(component_id: &str, message: &str) -> CmdResult<GitOutput> {
    let path = get_component_path(component_id)?;

    let status_output = execute_git(&path, &["status", "--porcelain=v1"])
        .map_err(|e| homeboy::Error::other(e.to_string()))?;

    let status_stdout = String::from_utf8_lossy(&status_output.stdout).to_string();

    if status_stdout.trim().is_empty() {
        return Ok((
            GitOutput {
                component_id: component_id.to_string(),
                path,
                action: "commit".to_string(),
                success: true,
                exit_code: 0,
                stdout: "Nothing to commit, working tree clean".to_string(),
                stderr: String::new(),
            },
            0,
        ));
    }

    let add_output =
        execute_git(&path, &["add", "."]).map_err(|e| homeboy::Error::other(e.to_string()))?;

    if !add_output.status.success() {
        let exit_code = to_exit_code(add_output.status);
        return Ok((
            GitOutput {
                component_id: component_id.to_string(),
                path,
                action: "commit".to_string(),
                success: false,
                exit_code,
                stdout: String::from_utf8_lossy(&add_output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&add_output.stderr).to_string(),
            },
            exit_code,
        ));
    }

    let commit_output = execute_git(&path, &["commit", "-m", message])
        .map_err(|e| homeboy::Error::other(e.to_string()))?;

    let exit_code = to_exit_code(commit_output.status);

    Ok((
        GitOutput {
            component_id: component_id.to_string(),
            path,
            action: "commit".to_string(),
            success: commit_output.status.success(),
            exit_code,
            stdout: String::from_utf8_lossy(&commit_output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&commit_output.stderr).to_string(),
        },
        exit_code,
    ))
}

fn push(component_id: &str, tags: bool) -> CmdResult<GitOutput> {
    let path = get_component_path(component_id)?;

    let push_args: Vec<&str> = if tags {
        vec!["push", "--tags"]
    } else {
        vec!["push"]
    };

    let output =
        execute_git(&path, &push_args).map_err(|e| homeboy::Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok((
        GitOutput {
            component_id: component_id.to_string(),
            path,
            action: "push".to_string(),
            success: output.status.success(),
            exit_code,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        exit_code,
    ))
}

fn pull(component_id: &str) -> CmdResult<GitOutput> {
    let path = get_component_path(component_id)?;

    let output =
        execute_git(&path, &["pull"]).map_err(|e| homeboy::Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok((
        GitOutput {
            component_id: component_id.to_string(),
            path,
            action: "pull".to_string(),
            success: output.status.success(),
            exit_code,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        exit_code,
    ))
}

fn tag(component_id: &str, tag_name: &str, message: Option<&str>) -> CmdResult<GitOutput> {
    let path = get_component_path(component_id)?;

    let tag_args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", tag_name, "-m", msg],
        None => vec!["tag", tag_name],
    };

    let output =
        execute_git(&path, &tag_args).map_err(|e| homeboy::Error::other(e.to_string()))?;
    let exit_code = to_exit_code(output.status);

    Ok((
        GitOutput {
            component_id: component_id.to_string(),
            path,
            action: "tag".to_string(),
            success: output.status.success(),
            exit_code,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        exit_code,
    ))
}

// Bulk handlers

fn bulk_status(input: BulkIdsInput) -> CmdResult<GitCommandOutput> {
    let mut results = Vec::new();

    for id in &input.component_ids {
        match status(id) {
            Ok((output, _)) => results.push(output),
            Err(e) => results.push(GitOutput {
                component_id: id.clone(),
                path: String::new(),
                action: "status".to_string(),
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.len() - succeeded;
    let exit_code = if failed > 0 { 1 } else { 0 };

    Ok((
        GitCommandOutput::Bulk(BulkOutput {
            action: "status".to_string(),
            results,
            summary: BulkSummary {
                total: input.component_ids.len(),
                succeeded,
                failed,
            },
        }),
        exit_code,
    ))
}

fn bulk_commit(input: BulkCommitInput) -> CmdResult<GitCommandOutput> {
    let mut results = Vec::new();

    for spec in &input.components {
        match commit(&spec.id, &spec.message) {
            Ok((output, _)) => results.push(output),
            Err(e) => results.push(GitOutput {
                component_id: spec.id.clone(),
                path: String::new(),
                action: "commit".to_string(),
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.len() - succeeded;
    let exit_code = if failed > 0 { 1 } else { 0 };

    Ok((
        GitCommandOutput::Bulk(BulkOutput {
            action: "commit".to_string(),
            results,
            summary: BulkSummary {
                total: input.components.len(),
                succeeded,
                failed,
            },
        }),
        exit_code,
    ))
}

fn bulk_push(input: BulkIdsInput) -> CmdResult<GitCommandOutput> {
    let mut results = Vec::new();
    let push_tags = input.tags;

    for id in &input.component_ids {
        match push(id, push_tags) {
            Ok((output, _)) => results.push(output),
            Err(e) => results.push(GitOutput {
                component_id: id.clone(),
                path: String::new(),
                action: "push".to_string(),
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.len() - succeeded;
    let exit_code = if failed > 0 { 1 } else { 0 };

    Ok((
        GitCommandOutput::Bulk(BulkOutput {
            action: "push".to_string(),
            results,
            summary: BulkSummary {
                total: input.component_ids.len(),
                succeeded,
                failed,
            },
        }),
        exit_code,
    ))
}

fn bulk_pull(input: BulkIdsInput) -> CmdResult<GitCommandOutput> {
    let mut results = Vec::new();

    for id in &input.component_ids {
        match pull(id) {
            Ok((output, _)) => results.push(output),
            Err(e) => results.push(GitOutput {
                component_id: id.clone(),
                path: String::new(),
                action: "pull".to_string(),
                success: false,
                exit_code: 1,
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.len() - succeeded;
    let exit_code = if failed > 0 { 1 } else { 0 };

    Ok((
        GitCommandOutput::Bulk(BulkOutput {
            action: "pull".to_string(),
            results,
            summary: BulkSummary {
                total: input.component_ids.len(),
                succeeded,
                failed,
            },
        }),
        exit_code,
    ))
}

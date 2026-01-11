use clap::{Args, Subcommand};
use serde::Serialize;
use std::process::Command;

use homeboy_core::config::ConfigManager;

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
        /// Component ID
        component_id: String,
    },
    /// Stage all changes and commit
    Commit {
        /// Component ID
        component_id: String,
        /// Commit message
        message: String,
    },
    /// Push local commits to remote
    Push {
        /// Component ID
        component_id: String,
        /// Push tags as well
        #[arg(long)]
        tags: bool,
    },
    /// Pull remote changes
    Pull {
        /// Component ID
        component_id: String,
    },
    /// Create a git tag
    Tag {
        /// Component ID
        component_id: String,
        /// Tag name (e.g., v0.1.2)
        tag_name: String,
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

pub fn run(args: GitArgs) -> CmdResult<GitOutput> {
    match args.command {
        GitCommand::Status { component_id } => status(&component_id),
        GitCommand::Commit {
            component_id,
            message,
        } => commit(&component_id, &message),
        GitCommand::Push { component_id, tags } => push(&component_id, tags),
        GitCommand::Pull { component_id } => pull(&component_id),
        GitCommand::Tag {
            component_id,
            tag_name,
            message,
        } => tag(&component_id, &tag_name, message.as_deref()),
    }
}

fn get_component_path(component_id: &str) -> homeboy_core::Result<String> {
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
        .map_err(|e| homeboy_core::Error::Other(e.to_string()))?;

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
        .map_err(|e| homeboy_core::Error::Other(e.to_string()))?;

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
        execute_git(&path, &["add", "."]).map_err(|e| homeboy_core::Error::Other(e.to_string()))?;

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
        .map_err(|e| homeboy_core::Error::Other(e.to_string()))?;

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
        execute_git(&path, &push_args).map_err(|e| homeboy_core::Error::Other(e.to_string()))?;
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
        execute_git(&path, &["pull"]).map_err(|e| homeboy_core::Error::Other(e.to_string()))?;
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
        execute_git(&path, &tag_args).map_err(|e| homeboy_core::Error::Other(e.to_string()))?;
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

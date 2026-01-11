use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy_core::base_path;
use homeboy_core::config::ConfigManager;
use homeboy_core::ssh::SshClient;

use crate::commands::CmdResult;

#[derive(Args)]
pub struct LogsArgs {
    #[command(subcommand)]
    command: LogsCommand,
}

#[derive(Subcommand)]
enum LogsCommand {
    /// List pinned log files
    List {
        /// Project ID
        project_id: String,
    },
    /// Show log file content
    Show {
        /// Project ID
        project_id: String,
        /// Log file path
        path: String,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "100")]
        lines: u32,
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
    },
    /// Clear log file contents
    Clear {
        /// Project ID
        project_id: String,
        /// Log file path
        path: String,
    },
}

pub fn run(args: LogsArgs) -> CmdResult<LogsOutput> {
    match args.command {
        LogsCommand::List { project_id } => list(&project_id),
        LogsCommand::Show {
            project_id,
            path,
            lines,
            follow,
        } => show(&project_id, &path, lines, follow),
        LogsCommand::Clear { project_id, path } => clear(&project_id, &path),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsOutput {
    pub command: String,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<LogEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<LogContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleared_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub path: String,
    pub label: Option<String>,
    pub tail_lines: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogContent {
    pub path: String,
    pub lines: u32,
    pub content: String,
}

fn list(project_id: &str) -> CmdResult<LogsOutput> {
    let project = ConfigManager::load_project(project_id)?;

    let entries = project
        .remote_logs
        .pinned_logs
        .iter()
        .map(|log| LogEntry {
            path: log.path.clone(),
            label: log.label.clone(),
            tail_lines: log.tail_lines,
        })
        .collect();

    Ok((
        LogsOutput {
            command: "logs.list".to_string(),
            project_id: project_id.to_string(),
            entries: Some(entries),
            log: None,
            cleared_path: None,
        },
        0,
    ))
}

fn show(project_id: &str, path: &str, lines: u32, follow: bool) -> CmdResult<LogsOutput> {
    let project = ConfigManager::load_project(project_id)?;

    let server_id = project.server_id.as_deref().ok_or_else(|| {
        homeboy_core::Error::Other(format!(
            "Server not configured for project '{}'",
            project_id
        ))
    })?;

    let server = ConfigManager::load_server(server_id)?;

    let full_path = base_path::join_remote_path(project.base_path.as_deref(), path)?;

    let client = SshClient::from_server(&server, server_id)
        .map_err(|e| homeboy_core::Error::Other(format!("SSH error: {}", e)))?;

    if follow {
        let tail_cmd = format!("tail -f '{}'", full_path);
        let code = client.execute_interactive(Some(&tail_cmd));

        Ok((
            LogsOutput {
                command: "logs.follow".to_string(),
                project_id: project_id.to_string(),
                entries: None,
                log: None,
                cleared_path: None,
            },
            code,
        ))
    } else {
        let command = format!("tail -n {} '{}'", lines, full_path);
        let output = client.execute(&command);

        if !output.success {
            return Err(homeboy_core::Error::Other(output.stderr));
        }

        Ok((
            LogsOutput {
                command: "logs.show".to_string(),
                project_id: project_id.to_string(),
                entries: None,
                log: Some(LogContent {
                    path: full_path,
                    lines,
                    content: output.stdout,
                }),
                cleared_path: None,
            },
            0,
        ))
    }
}

fn clear(project_id: &str, path: &str) -> CmdResult<LogsOutput> {
    let project = ConfigManager::load_project(project_id)?;

    let server_id = project.server_id.as_deref().ok_or_else(|| {
        homeboy_core::Error::Other(format!(
            "Server not configured for project '{}'",
            project_id
        ))
    })?;

    let server = ConfigManager::load_server(server_id)?;

    let client = SshClient::from_server(&server, server_id)
        .map_err(|e| homeboy_core::Error::Other(format!("SSH error: {}", e)))?;

    let full_path = base_path::join_remote_path(project.base_path.as_deref(), path)?;

    let command = format!(": > '{}'", full_path);
    let output = client.execute(&command);

    if !output.success {
        return Err(homeboy_core::Error::Other(output.stderr));
    }

    Ok((
        LogsOutput {
            command: "logs.clear".to_string(),
            project_id: project_id.to_string(),
            entries: None,
            log: None,
            cleared_path: Some(full_path),
        },
        0,
    ))
}

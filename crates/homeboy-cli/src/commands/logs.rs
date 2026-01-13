use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::base_path;
use homeboy::config::ConfigManager;
use homeboy::context::resolve_project_ssh;
use homeboy::shell;

use crate::commands::CmdResult;

#[derive(Args)]
pub struct LogsArgs {
    #[command(subcommand)]
    command: LogsCommand,
}

#[derive(Subcommand)]
pub enum LogsCommand {
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

pub fn is_interactive(args: &LogsArgs) -> bool {
    matches!(&args.command, LogsCommand::Show { follow: true, .. })
}

pub fn run(args: LogsArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<LogsOutput> {
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
    let project = ConfigManager::load_project_record(project_id)?;

    let entries = project
        .config
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
    let ctx = resolve_project_ssh(project_id)?;

    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;

    if follow {
        let tail_cmd = format!("tail -f {}", shell::quote_path(&full_path));
        let code = ctx.client.execute_interactive(Some(&tail_cmd));

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
        let command = format!("tail -n {} {}", lines, shell::quote_path(&full_path));
        let output = ctx.client.execute(&command);

        if !output.success {
            return Err(homeboy::Error::other(output.stderr));
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
    let ctx = resolve_project_ssh(project_id)?;

    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;

    let command = format!(": > {}", shell::quote_path(&full_path));
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(homeboy::Error::other(output.stderr));
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

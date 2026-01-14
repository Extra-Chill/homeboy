use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::logs::{self, LogContent, LogEntry, LogSearchResult};

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
    /// Search log file for pattern
    Search {
        /// Project ID
        project_id: String,
        /// Log file path
        path: String,
        /// Search pattern
        pattern: String,
        /// Case insensitive search
        #[arg(short = 'i', long)]
        ignore_case: bool,
        /// Limit to last N lines before searching
        #[arg(short = 'n', long)]
        lines: Option<u32>,
        /// Lines of context around matches
        #[arg(short = 'C', long)]
        context: Option<u32>,
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
        LogsCommand::Search {
            project_id,
            path,
            pattern,
            ignore_case,
            lines,
            context,
        } => search(&project_id, &path, &pattern, ignore_case, lines, context),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_result: Option<LogSearchResult>,
}

fn list(project_id: &str) -> CmdResult<LogsOutput> {
    let entries = logs::list(project_id)?;

    Ok((
        LogsOutput {
            command: "logs.list".to_string(),
            project_id: project_id.to_string(),
            entries: Some(entries),
            log: None,
            cleared_path: None,
            search_result: None,
        },
        0,
    ))
}

fn show(project_id: &str, path: &str, lines: u32, follow: bool) -> CmdResult<LogsOutput> {
    if follow {
        let code = logs::follow(project_id, path)?;

        Ok((
            LogsOutput {
                command: "logs.follow".to_string(),
                project_id: project_id.to_string(),
                entries: None,
                log: None,
                cleared_path: None,
                search_result: None,
            },
            code,
        ))
    } else {
        let content = logs::show(project_id, path, lines)?;

        Ok((
            LogsOutput {
                command: "logs.show".to_string(),
                project_id: project_id.to_string(),
                entries: None,
                log: Some(content),
                cleared_path: None,
                search_result: None,
            },
            0,
        ))
    }
}

fn clear(project_id: &str, path: &str) -> CmdResult<LogsOutput> {
    let cleared_path = logs::clear(project_id, path)?;

    Ok((
        LogsOutput {
            command: "logs.clear".to_string(),
            project_id: project_id.to_string(),
            entries: None,
            log: None,
            cleared_path: Some(cleared_path),
            search_result: None,
        },
        0,
    ))
}

fn search(
    project_id: &str,
    path: &str,
    pattern: &str,
    ignore_case: bool,
    lines: Option<u32>,
    context: Option<u32>,
) -> CmdResult<LogsOutput> {
    let result = logs::search(project_id, path, pattern, ignore_case, lines, context)?;

    Ok((
        LogsOutput {
            command: "logs.search".to_string(),
            project_id: project_id.to_string(),
            entries: None,
            log: None,
            cleared_path: None,
            search_result: Some(result),
        },
        0,
    ))
}

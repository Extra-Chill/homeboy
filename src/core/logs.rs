//! Log file operations.
//!
//! Provides viewing, following, and clearing of log files.
//! Routes to local or SSH execution based on project configuration.

use crate::base_path;
use crate::context::require_project_base_path;
use crate::error::{Error, Result};
use crate::executor::{execute_for_project, execute_for_project_interactive};
use crate::project;
use crate::shell;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]

pub struct LogEntry {
    pub path: String,
    pub label: Option<String>,
    pub tail_lines: u32,
}

#[derive(Debug, Serialize)]

pub struct LogContent {
    pub path: String,
    pub lines: u32,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct LogSearchMatch {
    pub line_number: u32,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct LogSearchResult {
    pub path: String,
    pub pattern: String,
    pub matches: Vec<LogSearchMatch>,
    pub match_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PinnedLogContent {
    pub path: String,
    pub label: Option<String>,
    pub lines: u32,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PinnedLogsContent {
    pub logs: Vec<PinnedLogContent>,
    pub total_logs: usize,
}

/// Lists pinned log files for a project.
pub fn list(project_id: &str) -> Result<Vec<LogEntry>> {
    let project = project::load(project_id)?;

    Ok(project
        .remote_logs
        .pinned_logs
        .iter()
        .map(|log| LogEntry {
            path: log.path.clone(),
            label: log.label.clone(),
            tail_lines: log.tail_lines,
        })
        .collect())
}

/// Shows all pinned logs for a project.
pub fn show_pinned(project_id: &str, lines: u32) -> Result<PinnedLogsContent> {
    let project = project::load(project_id)?;

    if project.remote_logs.pinned_logs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "pinned_logs",
            "No pinned logs configured for this project",
            None,
            Some(vec![
                format!(
                    "Pin a log: homeboy project set {} --pin-log /path/to/app.log",
                    project_id
                ),
                format!("List pinned logs: homeboy logs list {}", project_id),
            ]),
        ));
    }

    let base_path = require_project_base_path(project_id, &project)?;

    let mut logs = Vec::new();
    for pinned_log in &project.remote_logs.pinned_logs {
        let log_lines = if lines > 0 { lines } else { pinned_log.tail_lines };
        let full_path = base_path::join_remote_path(Some(&base_path), &pinned_log.path)?;

        let command = format!("tail -n {} {}", log_lines, shell::quote_path(&full_path));
        let output = execute_for_project(&project, &command)?;

        logs.push(PinnedLogContent {
            path: full_path,
            label: pinned_log.label.clone(),
            lines: log_lines,
            content: output.stdout,
        });
    }

    let total_logs = logs.len();
    Ok(PinnedLogsContent { logs, total_logs })
}

/// Shows the last N lines of a log file.
pub fn show(project_id: &str, path: &str, lines: u32) -> Result<LogContent> {
    let project = project::load(project_id)?;
    let base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&base_path), path)?;

    let command = format!("tail -n {} {}", lines, shell::quote_path(&full_path));
    let output = execute_for_project(&project, &command)?;

    Ok(LogContent {
        path: full_path,
        lines,
        content: output.stdout,
    })
}

/// Follows a log file (tail -f). Returns exit code from interactive session.
///
/// Note: This requires an interactive terminal. The caller is responsible
/// for ensuring terminal availability before calling.
pub fn follow(project_id: &str, path: &str) -> Result<i32> {
    let project = project::load(project_id)?;
    let base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&base_path), path)?;

    let tail_cmd = format!("tail -f {}", shell::quote_path(&full_path));
    execute_for_project_interactive(&project, &tail_cmd)
}

/// Clears the contents of a log file. Returns the full path that was cleared.
pub fn clear(project_id: &str, path: &str) -> Result<String> {
    let project = project::load(project_id)?;
    let base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&base_path), path)?;

    let command = format!(": > {}", shell::quote_path(&full_path));
    execute_for_project(&project, &command)?;

    Ok(full_path)
}

/// Searches a log file for a pattern.
pub fn search(
    project_id: &str,
    path: &str,
    pattern: &str,
    case_insensitive: bool,
    lines: Option<u32>,
    context: Option<u32>,
) -> Result<LogSearchResult> {
    let project = project::load(project_id)?;
    let base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&base_path), path)?;

    let mut grep_flags = String::from("-n");
    if case_insensitive {
        grep_flags.push('i');
    }
    if let Some(ctx_lines) = context {
        grep_flags.push_str(&format!(" -C {}", ctx_lines));
    }

    let command = if let Some(n) = lines {
        format!(
            "tail -n {} {} | grep {} {}",
            n,
            shell::quote_path(&full_path),
            grep_flags,
            shell::quote_path(pattern)
        )
    } else {
        format!(
            "grep {} {} {}",
            grep_flags,
            shell::quote_path(pattern),
            shell::quote_path(&full_path)
        )
    };

    let output = execute_for_project(&project, &command)?;

    // grep returns exit code 1 when no matches found, which is not an error
    let matches = parse_grep_output(&output.stdout);
    let match_count = matches.len();

    Ok(LogSearchResult {
        path: full_path,
        pattern: pattern.to_string(),
        matches,
        match_count,
    })
}

/// Parse grep -n output into structured matches.
fn parse_grep_output(output: &str) -> Vec<LogSearchMatch> {
    let mut matches = Vec::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        // grep -n format: "line_number:content" or "line_number-content" (for context lines)
        if let Some(colon_pos) = line.find(':') {
            if let Ok(line_num) = line[..colon_pos].parse::<u32>() {
                matches.push(LogSearchMatch {
                    line_number: line_num,
                    content: line[colon_pos + 1..].to_string(),
                });
            }
        } else if let Some(dash_pos) = line.find('-') {
            // Context lines use dash separator
            if let Ok(line_num) = line[..dash_pos].parse::<u32>() {
                matches.push(LogSearchMatch {
                    line_number: line_num,
                    content: line[dash_pos + 1..].to_string(),
                });
            }
        }
    }

    matches
}

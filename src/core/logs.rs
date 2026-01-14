//! Remote log file operations.
//!
//! Provides viewing, following, and clearing of remote log files
//! without exposing SSH, shell quoting, or path utilities.

use crate::base_path;
use crate::context::resolve_project_ssh;
use crate::error::{Result, TargetDetails};
use crate::project;
use crate::shell;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub path: String,
    pub label: Option<String>,
    pub tail_lines: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogContent {
    pub path: String,
    pub lines: u32,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSearchMatch {
    pub line_number: u32,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSearchResult {
    pub path: String,
    pub pattern: String,
    pub matches: Vec<LogSearchMatch>,
    pub match_count: usize,
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

/// Shows the last N lines of a log file.
pub fn show(project_id: &str, path: &str, lines: u32) -> Result<LogContent> {
    let ctx = resolve_project_ssh(project_id)?;
    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;

    let command = format!("tail -n {} {}", lines, shell::quote_path(&full_path));
    let target = TargetDetails {
        project_id: Some(project_id.to_string()),
        server_id: Some(ctx.server_id.clone()),
        host: Some(ctx.client.host.clone()),
    };
    let output = ctx
        .client
        .execute(&command)
        .into_remote_result(&command, target)?;

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
    let ctx = resolve_project_ssh(project_id)?;
    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;

    let tail_cmd = format!("tail -f {}", shell::quote_path(&full_path));
    let code = ctx.client.execute_interactive(Some(&tail_cmd));

    Ok(code)
}

/// Clears the contents of a log file. Returns the full path that was cleared.
pub fn clear(project_id: &str, path: &str) -> Result<String> {
    let ctx = resolve_project_ssh(project_id)?;
    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;

    let command = format!(": > {}", shell::quote_path(&full_path));
    let target = TargetDetails {
        project_id: Some(project_id.to_string()),
        server_id: Some(ctx.server_id.clone()),
        host: Some(ctx.client.host.clone()),
    };
    let _output = ctx
        .client
        .execute(&command)
        .into_remote_result(&command, target)?;

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
    let ctx = resolve_project_ssh(project_id)?;
    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;

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

    let output = ctx.client.execute(&command);

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

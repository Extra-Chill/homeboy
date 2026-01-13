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

/// Lists pinned log files for a project.
pub fn list(project_id: &str) -> Result<Vec<LogEntry>> {
    let project = project::load_record(project_id)?;

    Ok(project
        .config
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
    let output = ctx.client.execute(&command).into_remote_result(&command, target)?;

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
    let _output = ctx.client.execute(&command).into_remote_result(&command, target)?;

    Ok(full_path)
}

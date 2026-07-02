//! Project log file operations.
//!
//! Provides viewing, following, and clearing of project log files.
//! Routes to local or SSH execution based on project configuration.
//! Pass `local: true` to bypass SSH and execute commands directly on the
//! current machine (useful when homeboy runs on the target server itself).

use crate::core::context::require_project_base_path;
use crate::core::engine::executor::{execute_for_project, execute_for_project_interactive};
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::paths as base_path;
use crate::core::project::{self, Project};
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
    pub evidence: LogEvidenceMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEvidenceMetadata {
    pub evidence_type: String,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_lines: Option<u32>,
    pub captured_lines: usize,
    pub byte_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_scope_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_insensitive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_count: Option<usize>,
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
    pub evidence: LogEvidenceMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct PinnedLogContent {
    pub path: String,
    pub label: Option<String>,
    pub lines: u32,
    pub content: String,
    pub evidence: LogEvidenceMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct PinnedLogsContent {
    pub logs: Vec<PinnedLogContent>,
    pub total_logs: usize,
}

fn load_project(project_id: &str, local: bool) -> Result<Project> {
    let mut project = project::load(project_id)?;
    if local {
        project.server_id = None;
    }
    Ok(project)
}

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

pub fn show_pinned(project_id: &str, lines: u32, local: bool) -> Result<PinnedLogsContent> {
    let project = load_project(project_id, local)?;

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
        let log_lines = if lines > 0 {
            lines
        } else {
            pinned_log.tail_lines
        };
        let full_path = base_path::join_remote_path(Some(&base_path), &pinned_log.path)?;

        let command = format!("tail -n {} {}", log_lines, shell::quote_path(&full_path));
        let output = execute_for_project(&project, &command)?;
        let evidence = LogEvidenceMetadata::tail(
            &full_path,
            pinned_log.label.clone(),
            log_lines,
            &output.stdout,
        );

        logs.push(PinnedLogContent {
            path: full_path,
            label: pinned_log.label.clone(),
            lines: log_lines,
            evidence,
            content: output.stdout,
        });
    }

    let total_logs = logs.len();
    Ok(PinnedLogsContent { logs, total_logs })
}

pub fn show(project_id: &str, path: &str, lines: u32, local: bool) -> Result<LogContent> {
    let project = load_project(project_id, local)?;
    let base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&base_path), path)?;

    let command = format!("tail -n {} {}", lines, shell::quote_path(&full_path));
    let output = execute_for_project(&project, &command)?;
    let evidence = LogEvidenceMetadata::tail(&full_path, None, lines, &output.stdout);

    Ok(LogContent {
        path: full_path,
        lines,
        evidence,
        content: output.stdout,
    })
}

pub fn follow(project_id: &str, path: &str, local: bool) -> Result<i32> {
    let project = load_project(project_id, local)?;
    let base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&base_path), path)?;

    let tail_cmd = format!("tail -f {}", shell::quote_path(&full_path));
    execute_for_project_interactive(&project, &tail_cmd)
}

pub fn clear(project_id: &str, path: &str, local: bool) -> Result<String> {
    let project = load_project(project_id, local)?;
    let base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&base_path), path)?;

    let command = format!(": > {}", shell::quote_path(&full_path));
    execute_for_project(&project, &command)?;

    Ok(full_path)
}

pub fn search(
    project_id: &str,
    path: &str,
    pattern: &str,
    case_insensitive: bool,
    lines: Option<u32>,
    context: Option<u32>,
    local: bool,
) -> Result<LogSearchResult> {
    let project = load_project(project_id, local)?;
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
    let matches = parse_grep_output(&output.stdout);
    let match_count = matches.len();
    let evidence = LogEvidenceMetadata::search(
        &full_path,
        pattern,
        lines,
        context,
        case_insensitive,
        &output.stdout,
        match_count,
    );

    Ok(LogSearchResult {
        path: full_path,
        pattern: pattern.to_string(),
        matches,
        match_count,
        evidence,
    })
}

impl LogEvidenceMetadata {
    fn tail(path: &str, label: Option<String>, requested_lines: u32, content: &str) -> Self {
        Self {
            evidence_type: "log_tail".to_string(),
            source_path: path.to_string(),
            label,
            requested_lines: Some(requested_lines),
            captured_lines: content.lines().count(),
            byte_count: content.len(),
            pattern: None,
            search_scope_lines: None,
            context_lines: None,
            case_insensitive: None,
            match_count: None,
        }
    }

    fn search(
        path: &str,
        pattern: &str,
        search_scope_lines: Option<u32>,
        context_lines: Option<u32>,
        case_insensitive: bool,
        content: &str,
        match_count: usize,
    ) -> Self {
        Self {
            evidence_type: "log_search".to_string(),
            source_path: path.to_string(),
            label: None,
            requested_lines: None,
            captured_lines: content.lines().count(),
            byte_count: content.len(),
            pattern: Some(pattern.to_string()),
            search_scope_lines,
            context_lines,
            case_insensitive: Some(case_insensitive),
            match_count: Some(match_count),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_evidence_records_source_and_capture_size() {
        let evidence = LogEvidenceMetadata::tail(
            "/var/log/app.log",
            Some("app".to_string()),
            50,
            "first\nsecond\n",
        );

        assert_eq!(evidence.evidence_type, "log_tail");
        assert_eq!(evidence.source_path, "/var/log/app.log");
        assert_eq!(evidence.label.as_deref(), Some("app"));
        assert_eq!(evidence.requested_lines, Some(50));
        assert_eq!(evidence.captured_lines, 2);
        assert_eq!(evidence.byte_count, "first\nsecond\n".len());
    }

    #[test]
    fn search_evidence_records_query_scope_and_match_count() {
        let evidence = LogEvidenceMetadata::search(
            "/var/log/app.log",
            "fatal",
            Some(500),
            Some(2),
            true,
            "12:Fatal error\n13-context\n",
            1,
        );

        assert_eq!(evidence.evidence_type, "log_search");
        assert_eq!(evidence.source_path, "/var/log/app.log");
        assert_eq!(evidence.pattern.as_deref(), Some("fatal"));
        assert_eq!(evidence.search_scope_lines, Some(500));
        assert_eq!(evidence.context_lines, Some(2));
        assert_eq!(evidence.case_insensitive, Some(true));
        assert_eq!(evidence.match_count, Some(1));
        assert_eq!(evidence.captured_lines, 2);
    }
}

fn parse_grep_output(output: &str) -> Vec<LogSearchMatch> {
    let mut matches = Vec::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        if let Some(colon_pos) = line.find(':') {
            if let Ok(line_num) = line[..colon_pos].parse::<u32>() {
                matches.push(LogSearchMatch {
                    line_number: line_num,
                    content: line[colon_pos + 1..].to_string(),
                });
            }
        } else if let Some(dash_pos) = line.find('-') {
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

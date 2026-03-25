//! find — extracted from files.rs.

use crate::context::{require_project_base_path, resolve_project_ssh_with_base_path};
use crate::engine::executor::execute_for_project;
use crate::engine::text;
use crate::engine::{command, shell};
use crate::error::{Error, Result};
use crate::paths::{self as base_path, resolve_path_string};
use crate::project;
use serde::Serialize;
use std::io::{self, Read};
use std::path::Path;
use std::process::Command;
use super::GrepResult;
use super::GrepMatch;
use super::FindResult;
use super::ListResult;
use super::FileEntry;


/// Parse `ls -la` output into structured file entries.
pub(crate) fn parse_ls_output(output: &str, base_path: &str) -> Vec<FileEntry> {
    let mut entries: Vec<FileEntry> =
        text::lines_filtered(output, |line| !line.starts_with("total "))
            .filter_map(|line| parse_ls_line(line, base_path))
            .collect();

    entries.sort_by(|a, b| {
        if a.is_directory != b.is_directory {
            return b.is_directory.cmp(&a.is_directory);
        }
        text::cmp_case_insensitive(&a.name, &b.name)
    });

    entries
}

pub(crate) fn parse_ls_line(line: &str, base_path: &str) -> Option<FileEntry> {
    let parts = text::split_whitespace(line, 9)?;

    let permissions = parts[0];
    let name = parts[8..].join(" ");

    if name == "." || name == ".." {
        return None;
    }

    Some(FileEntry {
        name: name.clone(),
        path: resolve_path_string(base_path, &name),
        is_directory: permissions.starts_with('d'),
        size: parts[4].parse().ok(),
        permissions: permissions[1..].to_string(),
    })
}

/// List directory contents.
pub fn list(project_id: &str, path: &str) -> Result<ListResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;
    let command = format!("ls -la {}", shell::quote_path(&full_path));
    let output = execute_for_project(&project, &command)?;
    command::require_success(output.success, &output.stderr, "LIST")?;

    let entries = parse_ls_output(&output.stdout, &full_path);

    Ok(ListResult {
        base_path: Some(project_base_path),
        path: full_path,
        entries,
    })
}

/// Parse find output into list of matching paths.
pub(crate) fn parse_find_output(output: &str) -> Vec<String> {
    text::lines(output).map(|s| s.to_string()).collect()
}

/// Parse grep output into structured matches.
pub(crate) fn parse_grep_output(output: &str) -> Vec<GrepMatch> {
    let mut matches = Vec::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        // grep -n format: "filename:line_number:content"
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() >= 3 {
            if let Ok(line_num) = parts[1].parse::<u32>() {
                matches.push(GrepMatch {
                    file: parts[0].to_string(),
                    line: line_num,
                    content: parts[2].to_string(),
                });
            }
        }
    }

    matches
}

/// Find files matching pattern.
pub fn find(
    project_id: &str,
    path: &str,
    name_pattern: Option<&str>,
    file_type: Option<&str>,
    max_depth: Option<u32>,
) -> Result<FindResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let mut cmd = format!("find {}", shell::quote_path(&full_path));

    if let Some(depth) = max_depth {
        cmd.push_str(&format!(" -maxdepth {}", depth));
    }

    if let Some(t) = file_type {
        match t {
            "f" | "d" | "l" => cmd.push_str(&format!(" -type {}", t)),
            _ => {
                return Err(Error::validation_invalid_argument(
                    "file_type",
                    "Invalid file type. Use 'f', 'd', or 'l'.",
                    Some(t.to_string()),
                    Some(vec!["f".to_string(), "d".to_string(), "l".to_string()]),
                ))
            }
        }
    }

    if let Some(name) = name_pattern {
        cmd.push_str(&format!(" -name {}", shell::quote_path(name)));
    }

    // Sort output for consistent results
    cmd.push_str(" 2>/dev/null | sort");

    let output = execute_for_project(&project, &cmd)?;

    // find returns exit code 0 even with no matches
    let matches = parse_find_output(&output.stdout);

    Ok(FindResult {
        base_path: Some(project_base_path),
        path: full_path,
        pattern: name_pattern.map(|s| s.to_string()),
        matches,
    })
}

/// Search file contents using grep.
pub fn grep(
    project_id: &str,
    path: &str,
    pattern: &str,
    name_filter: Option<&str>,
    max_depth: Option<u32>,
    case_insensitive: bool,
) -> Result<GrepResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    if pattern.trim().is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "pattern".to_string()
        ]));
    }

    // Check if path is a file or directory
    let is_dir_cmd = format!(
        "test -d {} && echo dir || echo file",
        shell::quote_path(&full_path)
    );
    let check_output = execute_for_project(&project, &is_dir_cmd)?;
    let is_directory = check_output.stdout.trim() == "dir";

    // Build grep command based on path type and options
    let cmd = if is_directory && (max_depth.is_some() || name_filter.is_some()) {
        // Use find + xargs for portable depth limiting and name filtering
        let case_flag = if case_insensitive { "-i" } else { "" };
        let mut find_cmd = format!("find {}", shell::quote_path(&full_path));

        if let Some(depth) = max_depth {
            find_cmd.push_str(&format!(" -maxdepth {}", depth));
        }

        find_cmd.push_str(" -type f");

        if let Some(name) = name_filter {
            find_cmd.push_str(&format!(" -name {}", shell::quote_path(name)));
        }

        format!(
            "{} -print0 2>/dev/null | xargs -0 grep -n {} {} 2>/dev/null",
            find_cmd,
            case_flag,
            shell::quote_path(pattern)
        )
    } else if is_directory {
        // Simple recursive grep for directories without depth/name filters
        let flags = if case_insensitive { "-rni" } else { "-rn" };
        format!(
            "grep {} {} {} 2>/dev/null",
            flags,
            shell::quote_path(pattern),
            shell::quote_path(&full_path)
        )
    } else {
        // Single file grep (no -r flag)
        let flags = if case_insensitive { "-ni" } else { "-n" };
    };

    let output = execute_for_project(&project, &cmd)?;

    // grep returns exit code 1 when no matches found, which is not an error
    let matches = parse_grep_output(&output.stdout);

    Ok(GrepResult {
        base_path: Some(project_base_path),
        path: full_path,
        pattern: pattern.to_string(),
        matches,
    })
}

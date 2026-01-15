//! File operations.
//!
//! Provides file browsing, reading, writing, and searching.
//! Routes to local or SSH execution based on project configuration.

use serde::Serialize;
use std::io::{self, Read};

use crate::context::require_project_base_path;
use crate::error::{Error, Result};
use crate::executor::execute_for_project;
use crate::project;
use crate::{base_path, shell, token};

#[derive(Debug, Clone, Serialize)]

pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: Option<i64>,
    pub permissions: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct ListResult {
    pub base_path: Option<String>,
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Debug, Clone, Serialize)]

pub struct ReadResult {
    pub base_path: Option<String>,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct WriteResult {
    pub base_path: Option<String>,
    pub path: String,
    pub bytes_written: usize,
}

#[derive(Debug, Clone, Serialize)]

pub struct DeleteResult {
    pub base_path: Option<String>,
    pub path: String,
    pub recursive: bool,
}

#[derive(Debug, Clone, Serialize)]

pub struct RenameResult {
    pub base_path: Option<String>,
    pub old_path: String,
    pub new_path: String,
}

/// Parse `ls -la` output into structured file entries.
pub fn parse_ls_output(output: &str, base_path: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();

    for line in output.lines() {
        if line.is_empty() || line.starts_with("total ") {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }

        let permissions = parts[0];
        let name = parts[8..].join(" ");

        if name == "." || name == ".." {
            continue;
        }

        let is_directory = permissions.starts_with('d');
        let size = parts[4].parse::<i64>().ok();

        let full_path = if base_path.ends_with('/') {
            format!("{}{}", base_path, name)
        } else {
            format!("{}/{}", base_path, name)
        };

        entries.push(FileEntry {
            name,
            path: full_path,
            is_directory,
            size,
            permissions: permissions[1..].to_string(),
        });
    }

    entries.sort_by(|a, b| {
        if a.is_directory != b.is_directory {
            return b.is_directory.cmp(&a.is_directory);
        }
        token::cmp_case_insensitive(&a.name, &b.name)
    });

    entries
}

/// Read content from stdin, stripping trailing newline.
pub fn read_stdin() -> Result<String> {
    let mut content = String::new();
    io::stdin()
        .read_to_string(&mut content)
        .map_err(|e| Error::other(format!("Failed to read stdin: {}", e)))?;

    if content.ends_with('\n') {
        content.pop();
    }

    Ok(content)
}

/// List directory contents.
pub fn list(project_id: &str, path: &str) -> Result<ListResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;
    let command = format!("ls -la {}", shell::quote_path(&full_path));
    let output = execute_for_project(&project, &command)?;

    if !output.success {
        return Err(Error::other(format!("LIST_FAILED: {}", output.stderr)));
    }

    let entries = parse_ls_output(&output.stdout, &full_path);

    Ok(ListResult {
        base_path: Some(project_base_path),
        path: full_path,
        entries,
    })
}

/// Read file content.
pub fn read(project_id: &str, path: &str) -> Result<ReadResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;
    let command = format!("cat {}", shell::quote_path(&full_path));
    let output = execute_for_project(&project, &command)?;

    if !output.success {
        return Err(Error::other(format!("READ_FAILED: {}", output.stderr)));
    }

    Ok(ReadResult {
        base_path: Some(project_base_path),
        path: full_path,
        content: output.stdout,
    })
}

/// Write content to file.
pub fn write(project_id: &str, path: &str, content: &str) -> Result<WriteResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;
    let command = format!(
        "cat > {} << 'HOMEBOYEOF'\n{}\nHOMEBOYEOF",
        shell::quote_path(&full_path),
        content
    );
    let output = execute_for_project(&project, &command)?;

    if !output.success {
        return Err(Error::other(format!("WRITE_FAILED: {}", output.stderr)));
    }

    Ok(WriteResult {
        base_path: Some(project_base_path),
        path: full_path,
        bytes_written: content.len(),
    })
}

/// Delete file or directory.
pub fn delete(project_id: &str, path: &str, recursive: bool) -> Result<DeleteResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;
    let flags = if recursive { "-rf" } else { "-f" };
    let command = format!("rm {} {}", flags, shell::quote_path(&full_path));
    let output = execute_for_project(&project, &command)?;

    if !output.success {
        return Err(Error::other(format!("DELETE_FAILED: {}", output.stderr)));
    }

    Ok(DeleteResult {
        base_path: Some(project_base_path),
        path: full_path,
        recursive,
    })
}

/// Rename or move file.
pub fn rename(project_id: &str, old_path: &str, new_path: &str) -> Result<RenameResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_old = base_path::join_remote_path(Some(&project_base_path), old_path)?;
    let full_new = base_path::join_remote_path(Some(&project_base_path), new_path)?;
    let command = format!(
        "mv {} {}",
        shell::quote_path(&full_old),
        shell::quote_path(&full_new)
    );
    let output = execute_for_project(&project, &command)?;

    if !output.success {
        return Err(Error::other(format!("RENAME_FAILED: {}", output.stderr)));
    }

    Ok(RenameResult {
        base_path: Some(project_base_path),
        old_path: full_old,
        new_path: full_new,
    })
}

#[derive(Debug, Clone, Serialize)]

pub struct FindResult {
    pub base_path: Option<String>,
    pub path: String,
    pub pattern: Option<String>,
    pub matches: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]

pub struct GrepMatch {
    pub file: String,
    pub line: u32,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct GrepResult {
    pub base_path: Option<String>,
    pub path: String,
    pub pattern: String,
    pub matches: Vec<GrepMatch>,
}

/// Parse find output into list of matching paths.
fn parse_find_output(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

/// Parse grep output into structured matches.
fn parse_grep_output(output: &str) -> Vec<GrepMatch> {
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
                return Err(Error::other(
                    "Invalid file type. Use 'f', 'd', or 'l'.".to_string(),
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
        return Err(Error::other("Search pattern required".to_string()));
    }

    let flags = if case_insensitive { "-rni" } else { "-rn" };

    let mut cmd = format!(
        "grep {} {} {}",
        flags,
        shell::quote_path(pattern),
        shell::quote_path(&full_path)
    );

    if let Some(name) = name_filter {
        cmd.push_str(&format!(" --include={}", shell::quote_path(name)));
    }

    if let Some(depth) = max_depth {
        cmd.push_str(&format!(" --max-depth={}", depth));
    }

    // Suppress error messages for unreadable files
    cmd.push_str(" 2>/dev/null");

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

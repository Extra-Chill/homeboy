//! File operations.
//!
//! Provides file browsing, reading, writing, and searching.
//! Routes to local or SSH execution based on project configuration.

use serde::Serialize;
use std::io::{self, Read};

use crate::context::{require_project_base_path, resolve_project_ssh_with_base_path};
use crate::defaults;
use crate::error::{Error, Result};
use crate::engine::executor::execute_for_project;
use crate::project;
use crate::utils::{command, parser, shell, token};
use crate::utils::base_path;

use std::path::Path;
use std::process::Command;

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
    let mut entries: Vec<FileEntry> = parser::lines_filtered(output, |line| {
        !line.starts_with("total ")
    })
    .filter_map(|line| parse_ls_line(line, base_path))
    .collect();

    entries.sort_by(|a, b| {
        if a.is_directory != b.is_directory {
            return b.is_directory.cmp(&a.is_directory);
        }
        token::cmp_case_insensitive(&a.name, &b.name)
    });

    entries
}

fn parse_ls_line(line: &str, base_path: &str) -> Option<FileEntry> {
    let parts = parser::split_whitespace(line, 9)?;

    let permissions = parts[0];
    let name = parts[8..].join(" ");

    if name == "." || name == ".." {
        return None;
    }

    Some(FileEntry {
        name: name.clone(),
        path: parser::resolve_path_string(base_path, &name),
        is_directory: permissions.starts_with('d'),
        size: parts[4].parse().ok(),
        permissions: permissions[1..].to_string(),
    })
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
    command::require_success(output.success, &output.stderr, "LIST")?;

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
    command::require_success(output.success, &output.stderr, "READ")?;

    Ok(ReadResult {
        base_path: Some(project_base_path),
        path: full_path,
        content: output.stdout,
    })
}

/// Generate a unique heredoc delimiter that doesn't appear in content.
fn generate_unique_delimiter(content: &str) -> String {
    let mut delimiter = "HOMEBOYEOF".to_string();
    let mut counter = 0;
    while content.contains(&delimiter) {
        counter += 1;
        delimiter = format!("HOMEBOYEOF_{}", counter);
    }
    delimiter
}

/// Write content to file.
pub fn write(project_id: &str, path: &str, content: &str) -> Result<WriteResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;
    let delimiter = generate_unique_delimiter(content);
    let command = format!(
        "cat > {} << '{}'\n{}\n{}",
        shell::quote_path(&full_path),
        delimiter,
        content,
        delimiter
    );
    let output = execute_for_project(&project, &command)?;
    command::require_success(output.success, &output.stderr, "WRITE")?;

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
    command::require_success(output.success, &output.stderr, "DELETE")?;

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
    command::require_success(output.success, &output.stderr, "RENAME")?;

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

#[derive(Debug, Clone, Serialize)]

pub struct EditResult {
    pub base_path: Option<String>,
    pub path: String,
    pub original_lines: Vec<String>,
    pub modified_lines: Vec<String>,
    pub changes_made: Vec<LineChange>,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]

pub struct LineChange {
    pub line_number: usize,
    pub original: String,
    pub modified: String,
    pub operation: String,
}

/// Parse find output into list of matching paths.
fn parse_find_output(output: &str) -> Vec<String> {
    parser::lines(output).map(|s| s.to_string()).collect()
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
        format!(
            "grep {} {} {} 2>/dev/null",
            flags,
            shell::quote_path(pattern),
            shell::quote_path(&full_path)
        )
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

pub fn edit_replace_line(
    project_id: &str,
    path: &str,
    line_num: usize,
    content: &str,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    if line_num == 0 || line_num > original_lines.len() {
        return Err(Error::validation_invalid_argument(
            "line_num",
            format!("Line number {} is out of range (file has {} lines)", line_num, original_lines.len()),
            None,
            None,
        ));
    }

    let mut modified_lines = original_lines.clone();
    let line_index = line_num - 1;
    let original_content = modified_lines[line_index].clone();
    modified_lines[line_index] = content.to_string();

    let modified_content = modified_lines.join("\n");
    write(project_id, path, &modified_content)?;

    let changes = vec![LineChange {
        line_number: line_num,
        original: original_content,
        modified: content.to_string(),
        operation: "replace".to_string(),
    }];

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub fn edit_insert_after_line(
    project_id: &str,
    path: &str,
    line_num: usize,
    content: &str,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    if line_num == 0 || line_num > original_lines.len() {
        return Err(Error::validation_invalid_argument(
            "line_num",
            format!("Line number {} is out of range (file has {} lines)", line_num, original_lines.len()),
            None,
            None,
        ));
    }

    let mut modified_lines = original_lines.clone();
    modified_lines.insert(line_num, content.to_string());

    let modified_content = modified_lines.join("\n");
    write(project_id, path, &modified_content)?;

    let changes = vec![LineChange {
        line_number: line_num + 1,
        original: String::new(),
        modified: content.to_string(),
        operation: "insert".to_string(),
    }];

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub fn edit_insert_before_line(
    project_id: &str,
    path: &str,
    line_num: usize,
    content: &str,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    if line_num == 0 || line_num > original_lines.len() {
        return Err(Error::validation_invalid_argument(
            "line_num",
            format!("Line number {} is out of range (file has {} lines)", line_num, original_lines.len()),
            None,
            None,
        ));
    }

    let mut modified_lines = original_lines.clone();
    modified_lines.insert(line_num - 1, content.to_string());

    let modified_content = modified_lines.join("\n");
    write(project_id, path, &modified_content)?;

    let changes = vec![LineChange {
        line_number: line_num,
        original: String::new(),
        modified: content.to_string(),
        operation: "insert".to_string(),
    }];

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub fn edit_delete_line(project_id: &str, path: &str, line_num: usize) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    if line_num == 0 || line_num > original_lines.len() {
        return Err(Error::validation_invalid_argument(
            "line_num",
            format!("Line number {} is out of range (file has {} lines)", line_num, original_lines.len()),
            None,
            None,
        ));
    }

    let mut modified_lines = original_lines.clone();
    let removed_content = modified_lines.remove(line_num - 1);

    let modified_content = modified_lines.join("\n");
    write(project_id, path, &modified_content)?;

    let changes = vec![LineChange {
        line_number: line_num,
        original: removed_content,
        modified: String::new(),
        operation: "delete".to_string(),
    }];

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub fn edit_delete_lines(
    project_id: &str,
    path: &str,
    start_line: usize,
    end_line: usize,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    if start_line == 0 || start_line > original_lines.len() || end_line == 0
        || end_line > original_lines.len() || start_line > end_line
    {
        return Err(Error::validation_invalid_argument(
            "line_range",
            format!("Invalid line range {}-{} (file has {} lines)", start_line, end_line, original_lines.len()),
            None,
            None,
        ));
    }

    let mut modified_lines = original_lines.clone();
    let start_index = start_line - 1;
    let end_index = end_line;
    let removed_lines: Vec<String> = modified_lines.drain(start_index..end_index).collect();

    let modified_content = modified_lines.join("\n");
    write(project_id, path, &modified_content)?;

    let changes: Vec<LineChange> = removed_lines
        .iter()
        .enumerate()
        .map(|(i, line)| LineChange {
            line_number: start_line + i,
            original: line.clone(),
            modified: String::new(),
            operation: "delete".to_string(),
        })
        .collect();

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub fn edit_replace_pattern(
    project_id: &str,
    path: &str,
    pattern: &str,
    replacement: &str,
    all: bool,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    let modified_content = if all {
        read_result.content.replace(pattern, replacement)
    } else {
        read_result.content.replacen(pattern, replacement, 1)
    };

    write(project_id, path, &modified_content)?;

    let modified_lines: Vec<String> = modified_content
        .lines()
        .map(String::from)
        .collect();

    let changes: Vec<LineChange> = original_lines
        .iter()
        .enumerate()
        .zip(modified_lines.iter())
        .filter_map(|((i, orig), modified)| {
            if orig != modified {
                Some(LineChange {
                    line_number: i + 1,
                    original: orig.clone(),
                    modified: modified.clone(),
                    operation: if all { "replace_all" } else { "replace" }.to_string(),
                })
            } else {
                None
            }
        })
        .collect();

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub fn edit_delete_pattern(project_id: &str, path: &str, pattern: &str) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    let modified_lines: Vec<String> = original_lines
        .iter()
        .filter(|line| !line.contains(pattern))
        .map(|s| s.to_string())
        .collect();

    let modified_content = modified_lines.join("\n");
    write(project_id, path, &modified_content)?;

    let changes: Vec<LineChange> = original_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(pattern))
        .map(|(i, line)| LineChange {
            line_number: i + 1,
            original: line.clone(),
            modified: String::new(),
            operation: "delete".to_string(),
        })
        .collect();

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub fn edit_append(project_id: &str, path: &str, content: &str) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    let command = format!(
        "printf '%s\\n' {} >> {}",
        shell::quote_arg(content),
        shell::quote_path(&full_path)
    );

    let output = execute_for_project(&project, &command)?;
    command::require_success(output.success, &output.stderr, "EDIT")?;

    let mut modified_lines = original_lines.clone();
    modified_lines.push(content.to_string());

    let changes = vec![LineChange {
        line_number: modified_lines.len(),
        original: String::new(),
        modified: content.to_string(),
        operation: "append".to_string(),
    }];

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub fn edit_prepend(project_id: &str, path: &str, content: &str) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = base_path::join_remote_path(Some(&project_base_path), path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result
        .content
        .lines()
        .map(String::from)
        .collect();

    let command = format!(
        "tmp=$(mktemp) && printf '%s\\n' {} | cat - {} > \"$tmp\" && mv \"$tmp\" {}",
        shell::quote_arg(content),
        shell::quote_path(&full_path),
        shell::quote_path(&full_path)
    );

    let output = execute_for_project(&project, &command)?;
    command::require_success(output.success, &output.stderr, "EDIT")?;

    let mut modified_lines = original_lines.clone();
    modified_lines.insert(0, content.to_string());

    let changes = vec![LineChange {
        line_number: 1,
        original: String::new(),
        modified: content.to_string(),
        operation: "prepend".to_string(),
    }];

    Ok(EditResult {
        base_path: Some(project_base_path),
        path: full_path,
        original_lines,
        modified_lines,
        changes_made: changes,
        success: true,
        error: None,
    })
}

pub struct DownloadResult {
    pub remote_path: String,
    pub local_path: String,
    pub recursive: bool,
    pub success: bool,
    pub exit_code: i32,
    pub error: Option<String>,
}

/// Download a file or directory from remote server via SCP.
pub fn download(
    project_id: &str,
    remote_path: &str,
    local_path: &str,
    recursive: bool,
) -> Result<DownloadResult> {
    let (ctx, project_base_path) = resolve_project_ssh_with_base_path(project_id)?;
    let full_remote_path = base_path::join_remote_path(Some(&project_base_path), remote_path)?;

    // Create local parent directories if needed
    let local = Path::new(local_path);
    if let Some(parent) = local.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::other(format!("Failed to create local directory: {}", e))
            })?;
        }
    }

    let deploy_defaults = defaults::load_defaults().deploy;
    let mut scp_args: Vec<String> = deploy_defaults.scp_flags.clone();

    if recursive {
        scp_args.push("-r".to_string());
    }

    if let Some(identity_file) = &ctx.client.identity_file {
        scp_args.extend(["-i".to_string(), identity_file.clone()]);
    }

    if ctx.client.port != deploy_defaults.default_ssh_port {
        scp_args.extend(["-P".to_string(), ctx.client.port.to_string()]);
    }

    // Remote source (reverse of upload)
    scp_args.push(format!(
        "{}@{}:{}",
        ctx.client.user,
        ctx.client.host,
        shell::quote_path(&full_remote_path)
    ));
    scp_args.push(local_path.to_string());

    let label = if recursive { "directory" } else { "file" };
    eprintln!(
        "[download] Downloading {}: {}@{}:{} -> {}",
        label, ctx.client.user, ctx.client.host, full_remote_path, local_path
    );

    let output = Command::new("scp").args(&scp_args).output();
    match output {
        Ok(output) if output.status.success() => Ok(DownloadResult {
            remote_path: full_remote_path,
            local_path: local_path.to_string(),
            recursive,
            success: true,
            exit_code: 0,
            error: None,
        }),
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(1);
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok(DownloadResult {
                remote_path: full_remote_path,
                local_path: local_path.to_string(),
                recursive,
                success: false,
                exit_code,
                error: Some(stderr),
            })
        }
        Err(err) => Ok(DownloadResult {
            remote_path: full_remote_path,
            local_path: local_path.to_string(),
            recursive,
            success: false,
            exit_code: 1,
            error: Some(err.to_string()),
        }),
    }
}

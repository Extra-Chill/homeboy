//! edit — extracted from files.rs.

use std::io::{self, Read};
use crate::context::{require_project_base_path, resolve_project_ssh_with_base_path};
use crate::engine::executor::execute_for_project;
use crate::engine::{command, shell};
use crate::error::{Error, Result};
use crate::paths::{self as base_path, resolve_path_string};
use crate::project;
use serde::Serialize;
use std::path::Path;
use std::process::Command;
use super::WriteResult;
use super::LineChange;
use super::EditResult;
use super::ReadResult;
use super::DeleteResult;


/// Read content from stdin, stripping trailing newline.
pub fn read_stdin() -> Result<String> {
    let mut content = String::new();
    io::stdin().read_to_string(&mut content).map_err(|e| {
        Error::internal_io(
            format!("Failed to read stdin: {}", e),
            Some("read stdin".to_string()),
        )
    })?;

    if content.ends_with('\n') {
        content.pop();
    }

    Ok(content)
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
pub(crate) fn generate_unique_delimiter(content: &str) -> String {
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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    if line_num == 0 || line_num > original_lines.len() {
        return Err(Error::validation_invalid_argument(
            "line_num",
            format!(
                "Line number {} is out of range (file has {} lines)",
                line_num,
                original_lines.len()
            ),
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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    if line_num == 0 || line_num > original_lines.len() {
        return Err(Error::validation_invalid_argument(
            "line_num",
            format!(
                "Line number {} is out of range (file has {} lines)",
                line_num,
                original_lines.len()
            ),
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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    if line_num == 0 || line_num > original_lines.len() {
        return Err(Error::validation_invalid_argument(
            "line_num",
            format!(
                "Line number {} is out of range (file has {} lines)",
                line_num,
                original_lines.len()
            ),
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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    if line_num == 0 || line_num > original_lines.len() {
        return Err(Error::validation_invalid_argument(
            "line_num",
            format!(
                "Line number {} is out of range (file has {} lines)",
                line_num,
                original_lines.len()
            ),
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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    if start_line == 0
        || start_line > original_lines.len()
        || end_line == 0
        || end_line > original_lines.len()
        || start_line > end_line
    {
        return Err(Error::validation_invalid_argument(
            "line_range",
            format!(
                "Invalid line range {}-{} (file has {} lines)",
                start_line,
                end_line,
                original_lines.len()
            ),
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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    let modified_content = if all {
        read_result.content.replace(pattern, replacement)
    } else {
        read_result.content.replacen(pattern, replacement, 1)
    };

    write(project_id, path, &modified_content)?;

    let modified_lines: Vec<String> = modified_content.lines().map(String::from).collect();

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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

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
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

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

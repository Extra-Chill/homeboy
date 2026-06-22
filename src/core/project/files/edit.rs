use serde::Serialize;

use crate::core::context::require_project_base_path;
use crate::core::engine::executor::execute_for_project;
use crate::core::engine::{command, shell};
use crate::core::error::{Error, Result};
use crate::core::project;

use super::{read, write};

#[derive(Debug, Clone, Copy, Default)]
pub struct EditOptions {
    pub dry_run: bool,
    pub force: bool,
}

fn write_modified_content(
    project_id: &str,
    path: &str,
    content: &str,
    options: EditOptions,
) -> Result<()> {
    if options.dry_run {
        return Ok(());
    }

    write(project_id, path, content).map(|_| ())
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

pub fn edit_replace_line(
    project_id: &str,
    path: &str,
    line_num: usize,
    content: &str,
) -> Result<EditResult> {
    edit_replace_line_with_options(project_id, path, line_num, content, EditOptions::default())
}

pub fn edit_replace_line_with_options(
    project_id: &str,
    path: &str,
    line_num: usize,
    content: &str,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

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
    write_modified_content(project_id, path, &modified_content, options)?;

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
    edit_insert_after_line_with_options(project_id, path, line_num, content, EditOptions::default())
}

pub fn edit_insert_after_line_with_options(
    project_id: &str,
    path: &str,
    line_num: usize,
    content: &str,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

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
    write_modified_content(project_id, path, &modified_content, options)?;

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
    edit_insert_before_line_with_options(
        project_id,
        path,
        line_num,
        content,
        EditOptions::default(),
    )
}

pub fn edit_insert_before_line_with_options(
    project_id: &str,
    path: &str,
    line_num: usize,
    content: &str,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

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
    write_modified_content(project_id, path, &modified_content, options)?;

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
    edit_delete_line_with_options(project_id, path, line_num, EditOptions::default())
}

pub fn edit_delete_line_with_options(
    project_id: &str,
    path: &str,
    line_num: usize,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

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
    write_modified_content(project_id, path, &modified_content, options)?;

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
    edit_delete_lines_with_options(
        project_id,
        path,
        start_line,
        end_line,
        EditOptions::default(),
    )
}

pub fn edit_delete_lines_with_options(
    project_id: &str,
    path: &str,
    start_line: usize,
    end_line: usize,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

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
    write_modified_content(project_id, path, &modified_content, options)?;

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
    edit_replace_pattern_with_options(
        project_id,
        path,
        pattern,
        replacement,
        all,
        EditOptions::default(),
    )
}

pub fn edit_replace_pattern_with_options(
    project_id: &str,
    path: &str,
    pattern: &str,
    replacement: &str,
    all: bool,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    let match_count = read_result.content.matches(pattern).count();
    if !all && match_count > 1 && !options.force {
        return Err(Error::validation_invalid_argument(
            "replace_pattern",
            format!(
                "Pattern matches {} times; use --replace-all-pattern or --force to replace only the first match",
                match_count
            ),
            None,
            None,
        ));
    }

    let modified_content = if all {
        read_result.content.replace(pattern, replacement)
    } else {
        read_result.content.replacen(pattern, replacement, 1)
    };

    write_modified_content(project_id, path, &modified_content, options)?;

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
    edit_delete_pattern_with_options(project_id, path, pattern, EditOptions::default())
}

pub fn edit_delete_pattern_with_options(
    project_id: &str,
    path: &str,
    pattern: &str,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    let modified_lines: Vec<String> = original_lines
        .iter()
        .filter(|line| !line.contains(pattern))
        .map(|s| s.to_string())
        .collect();

    let modified_content = modified_lines.join("\n");
    write_modified_content(project_id, path, &modified_content, options)?;

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
    edit_append_with_options(project_id, path, content, EditOptions::default())
}

pub fn edit_append_with_options(
    project_id: &str,
    path: &str,
    content: &str,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    let command = format!(
        "printf '%s\\n' {} >> {}",
        shell::quote_arg(content),
        shell::quote_path(&full_path)
    );

    if !options.dry_run {
        let output = execute_for_project(&project, &command)?;
        command::require_success(output.success, &output.stderr, "EDIT")?;
    }

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
    edit_prepend_with_options(project_id, path, content, EditOptions::default())
}

pub fn edit_prepend_with_options(
    project_id: &str,
    path: &str,
    content: &str,
    options: EditOptions,
) -> Result<EditResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path =
        crate::core::project::resolve_project_remote_path(&project, &project_base_path, path)?;

    let read_result = read(project_id, path)?;
    let original_lines: Vec<String> = read_result.content.lines().map(String::from).collect();

    let command = format!(
        "tmp=$(mktemp) && printf '%s\\n' {} | cat - {} > \"$tmp\" && mv \"$tmp\" {}",
        shell::quote_arg(content),
        shell::quote_path(&full_path),
        shell::quote_path(&full_path)
    );

    if !options.dry_run {
        let output = execute_for_project(&project, &command)?;
        command::require_success(output.success, &output.stderr, "EDIT")?;
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_replace_line() {
        let edit_fn: fn(&str, &str, usize, &str) -> Result<EditResult> = edit_replace_line;

        let _ = edit_fn;
    }

    #[test]
    fn test_edit_insert_after_line() {
        let edit_fn: fn(&str, &str, usize, &str) -> Result<EditResult> = edit_insert_after_line;

        let _ = edit_fn;
    }

    #[test]
    fn test_edit_insert_before_line() {
        let edit_fn: fn(&str, &str, usize, &str) -> Result<EditResult> = edit_insert_before_line;

        let _ = edit_fn;
    }

    #[test]
    fn test_edit_delete_line() {
        let edit_fn: fn(&str, &str, usize) -> Result<EditResult> = edit_delete_line;

        let _ = edit_fn;
    }

    #[test]
    fn test_edit_delete_lines() {
        let edit_fn: fn(&str, &str, usize, usize) -> Result<EditResult> = edit_delete_lines;

        let _ = edit_fn;
    }

    #[test]
    fn test_edit_replace_pattern() {
        let edit_fn: fn(&str, &str, &str, &str, bool) -> Result<EditResult> = edit_replace_pattern;

        let _ = edit_fn;
    }

    #[test]
    fn test_edit_delete_pattern() {
        let edit_fn: fn(&str, &str, &str) -> Result<EditResult> = edit_delete_pattern;

        let _ = edit_fn;
    }

    #[test]
    fn test_edit_append() {
        let edit_fn: fn(&str, &str, &str) -> Result<EditResult> = edit_append;

        let _ = edit_fn;
    }

    #[test]
    fn test_edit_prepend() {
        let edit_fn: fn(&str, &str, &str) -> Result<EditResult> = edit_prepend;

        let _ = edit_fn;
    }
}

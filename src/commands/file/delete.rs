//! delete — extracted from file.rs.

use homeboy::project::files::{self, FileEntry, GrepMatch, LineChange};
use super::super::CmdResult;
use clap::{Args, Subcommand};
use serde::Serialize;
use super::EditArgs;
use super::FileEditOutput;
use super::FileOutput;


pub(crate) fn delete(project_id: &str, path: &str, recursive: bool) -> CmdResult<FileOutput> {
    let result = files::delete(project_id, path, recursive)?;

    Ok((
        FileOutput {
            command: "file.delete".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: Some(result.path),
            old_path: None,
            new_path: None,
            recursive: Some(result.recursive),
            entries: None,
            content: None,
            bytes_written: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

pub(crate) fn edit(args: EditArgs) -> CmdResult<FileEditOutput> {
    let EditArgs {
        project_id,
        file_path,
        dry_run: _,
        force: _,
        line_ops,
        pattern_ops,
        file_mods,
    } = args;

    let result = if let Some(line_num) = line_ops.replace_line {
        let content = line_ops.replace_line_content.ok_or_else(|| {
            homeboy::Error::validation_invalid_argument(
                "content",
                "Content required for --replace-line",
                None,
                None,
            )
        })?;
        files::edit_replace_line(&project_id, &file_path, line_num, &content)?
    } else if let Some(line_num) = line_ops.insert_after {
        let content = line_ops.insert_after_content.ok_or_else(|| {
            homeboy::Error::validation_invalid_argument(
                "content",
                "Content required for --insert-after",
                None,
                None,
            )
        })?;
        files::edit_insert_after_line(&project_id, &file_path, line_num, &content)?
    } else if let Some(line_num) = line_ops.insert_before {
        let content = line_ops.insert_before_content.ok_or_else(|| {
            homeboy::Error::validation_invalid_argument(
                "content",
                "Content required for --insert-before",
                None,
                None,
            )
        })?;
        files::edit_insert_before_line(&project_id, &file_path, line_num, &content)?
    } else if let Some(line_num) = line_ops.delete_line {
        files::edit_delete_line(&project_id, &file_path, line_num)?
    } else if let Some(lines) = line_ops.delete_lines {
        if lines.len() != 2 {
            return Err(homeboy::Error::validation_invalid_argument(
                "delete_lines",
                "DELETE_LINES requires exactly 2 values: START END",
                None,
                None,
            ));
        }
        files::edit_delete_lines(&project_id, &file_path, lines[0], lines[1])?
    } else if let Some(pattern) = pattern_ops.replace_pattern {
        let replacement = pattern_ops.replace_pattern_content.ok_or_else(|| {
            homeboy::Error::validation_invalid_argument(
                "content",
                "Content required for --replace-pattern",
                None,
                None,
            )
        })?;
        files::edit_replace_pattern(&project_id, &file_path, &pattern, &replacement, false)?
    } else if let Some(pattern) = pattern_ops.replace_all_pattern {
        let replacement = pattern_ops.replace_all_content.ok_or_else(|| {
            homeboy::Error::validation_invalid_argument(
                "content",
                "Content required for --replace-all-pattern",
                None,
                None,
            )
        })?;
        files::edit_replace_pattern(&project_id, &file_path, &pattern, &replacement, true)?
    } else if let Some(pattern) = pattern_ops.delete_pattern {
        files::edit_delete_pattern(&project_id, &file_path, &pattern)?
    } else if let Some(content) = file_mods.append {
        files::edit_append(&project_id, &file_path, &content)?
    } else if let Some(content) = file_mods.prepend {
        files::edit_prepend(&project_id, &file_path, &content)?
    } else {
        return Err(homeboy::Error::validation_invalid_argument(
            "operation",
            "No edit operation specified. Use one of: --replace-line, --insert-after, --insert-before, --delete-line, --delete-lines, --replace-pattern, --replace-all-pattern, --delete-pattern, --append, --prepend",
            None,
            None,
        ));
    };

    let change_count = result.changes_made.len();

    Ok((
        FileEditOutput {
            command: "file.edit".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: result.path,
            changes_made: result.changes_made,
            change_count,
            success: result.success,
            error: result.error,
        },
        0,
    ))
}

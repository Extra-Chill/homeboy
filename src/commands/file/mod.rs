//! Inspect and modify remote project files.
//!
//! Split into focused submodules: CLI argument shapes (`args`), serializable
//! command outputs (`output`), and the command dispatch + per-operation
//! handlers below. Public types are re-exported so existing
//! `crate::commands::file::*` paths stay stable.

mod args;
mod output;

pub use args::FileArgs;
pub use output::{
    FileCommandOutput, FileDownloadOutput, FileEditOutput, FileFindOutput, FileGrepOutput,
    FileOutput,
};

use args::{EditArgs, FileCommand};

use homeboy::core::context::require_project_base_path;
use homeboy::core::engine::{command, executor, shell};
use homeboy::core::project::files;
use homeboy::core::server::transfer::{self, TransferConfig, TransferOutput};
use homeboy::core::{join_remote_path, project};

use super::CmdResult;

pub fn is_raw_read(args: &FileArgs) -> bool {
    matches!(&args.command, FileCommand::Read { raw: true, .. })
}

pub fn run(args: FileArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<FileCommandOutput> {
    match args.command {
        FileCommand::List { project_id, path } => {
            let (out, code) = list(&project_id, &path)?;
            Ok((FileCommandOutput::Standard(out), code))
        }
        FileCommand::Read {
            project_id,
            path,
            raw,
        } => {
            if raw {
                let result = files::read(&project_id, &path)?;
                Ok((FileCommandOutput::Raw(result.content), 0))
            } else {
                let (out, code) = read(&project_id, &path)?;
                Ok((FileCommandOutput::Standard(out), code))
            }
        }
        FileCommand::Write {
            project_id,
            path,
            apply,
        } => {
            let (out, code) = write(&project_id, &path, apply)?;
            Ok((FileCommandOutput::Standard(out), code))
        }
        FileCommand::Mkdir { project_id, path } => {
            let project = project::load(&project_id)?;
            let project_base_path = require_project_base_path(&project_id, &project)?;
            let full_path = join_remote_path(Some(&project_base_path), &path)?;
            let output = executor::execute_for_project(
                &project,
                &format!("mkdir {}", shell::quote_path(&full_path)),
            )?;
            command::require_success(output.success, &output.stderr, "MKDIR")?;

            Ok((
                FileCommandOutput::Standard(FileOutput {
                    command: "file.mkdir".to_string(),
                    project_id,
                    base_path: Some(project_base_path),
                    path: Some(full_path),
                    old_path: None,
                    new_path: None,
                    recursive: None,
                    entries: None,
                    content: None,
                    size: None,
                    bytes_written: None,
                    dry_run: false,
                    action_required: None,
                    stdout: None,
                    stderr: None,
                    exit_code: 0,
                    success: true,
                }),
                0,
            ))
        }
        FileCommand::Delete {
            project_id,
            path,
            recursive,
            apply,
        } => {
            let (out, code) = delete(&project_id, &path, recursive, apply)?;
            Ok((FileCommandOutput::Standard(out), code))
        }
        FileCommand::Rename {
            project_id,
            old_path,
            new_path,
        } => {
            let (out, code) = rename(&project_id, &old_path, &new_path)?;
            Ok((FileCommandOutput::Standard(out), code))
        }
        FileCommand::Find {
            project_id,
            path,
            name,
            file_type,
            max_depth,
        } => {
            let (out, code) = find(
                &project_id,
                &path,
                name.as_deref(),
                file_type.as_deref(),
                max_depth,
            )?;
            Ok((FileCommandOutput::Find(out), code))
        }
        FileCommand::Grep {
            project_id,
            path,
            pattern,
            name,
            max_depth,
            ignore_case,
        } => {
            let (out, code) = grep(
                &project_id,
                &path,
                &pattern,
                name.as_deref(),
                max_depth,
                ignore_case,
            )?;
            Ok((FileCommandOutput::Grep(out), code))
        }
        FileCommand::Download {
            project_id,
            path,
            local_path,
            recursive,
        } => {
            let result = files::download(&project_id, &path, &local_path, recursive)?;
            let code = result.exit_code;
            let out = FileDownloadOutput {
                command: "file.download".to_string(),
                project_id,
                remote_path: result.remote_path,
                local_path: result.local_path,
                recursive: result.recursive,
                success: result.success,
                exit_code: result.exit_code,
                error: result.error,
            };
            Ok((FileCommandOutput::Download(out), code))
        }
        FileCommand::Upload {
            server,
            local_path,
            remote_path,
            compress,
            dry_run,
        } => transfer_command(TransferConfig {
            source: local_path,
            destination: format!("{}:{}", server, remote_path),
            recursive: false,
            compress,
            dry_run,
            exclude: Vec::new(),
        }),
        FileCommand::Copy(args) => transfer_command(args.into_config()),
        FileCommand::Sync(args) => transfer_command(args.into_config()),
        FileCommand::Edit(args) => {
            let (out, code) = edit(args)?;
            Ok((FileCommandOutput::Edit(out), code))
        }
    }
}

fn run_transfer(config: TransferConfig) -> CmdResult<TransferOutput> {
    transfer::transfer(&config)
}

fn transfer_command(config: TransferConfig) -> CmdResult<FileCommandOutput> {
    let (out, code) = run_transfer(config)?;
    Ok((FileCommandOutput::Transfer(out), code))
}

fn list(project_id: &str, path: &str) -> CmdResult<FileOutput> {
    let result = files::list(project_id, path)?;

    Ok((
        FileOutput {
            command: "file.list".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: Some(result.path),
            old_path: None,
            new_path: None,
            recursive: None,
            entries: Some(result.entries),
            content: None,
            size: None,
            bytes_written: None,
            dry_run: false,
            action_required: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

fn read(project_id: &str, path: &str) -> CmdResult<FileOutput> {
    let result = files::read(project_id, path)?;

    Ok((
        FileOutput {
            command: "file.read".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: Some(result.path),
            old_path: None,
            new_path: None,
            recursive: None,
            entries: None,
            content: Some(result.content),
            size: result.size,
            bytes_written: None,
            dry_run: false,
            action_required: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

fn write(project_id: &str, path: &str, apply: bool) -> CmdResult<FileOutput> {
    let content = files::read_stdin()?;
    if !apply {
        let project = project::load(project_id)?;
        let project_base_path = require_project_base_path(project_id, &project)?;
        let full_path = join_remote_path(Some(&project_base_path), path)?;

        return Ok((
            FileOutput {
                command: "file.write".to_string(),
                project_id: project_id.to_string(),
                base_path: Some(project_base_path),
                path: Some(full_path),
                old_path: None,
                new_path: None,
                recursive: None,
                entries: None,
                content: None,
                size: None,
                bytes_written: Some(content.len()),
                dry_run: true,
                action_required: Some(
                    "Re-run with --apply to write stdin to the remote file.".to_string(),
                ),
                stdout: None,
                stderr: None,
                exit_code: 0,
                success: true,
            },
            0,
        ));
    }
    let result = files::write(project_id, path, &content)?;

    Ok((
        FileOutput {
            command: "file.write".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: Some(result.path),
            old_path: None,
            new_path: None,
            recursive: None,
            entries: None,
            content: None,
            size: None,
            bytes_written: Some(result.bytes_written),
            dry_run: false,
            action_required: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

fn delete(project_id: &str, path: &str, recursive: bool, apply: bool) -> CmdResult<FileOutput> {
    if !apply {
        let project = project::load(project_id)?;
        let project_base_path = require_project_base_path(project_id, &project)?;
        let full_path = join_remote_path(Some(&project_base_path), path)?;

        return Ok((
            FileOutput {
                command: "file.delete".to_string(),
                project_id: project_id.to_string(),
                base_path: Some(project_base_path),
                path: Some(full_path),
                old_path: None,
                new_path: None,
                recursive: Some(recursive),
                entries: None,
                content: None,
                size: None,
                bytes_written: None,
                dry_run: true,
                action_required: Some("Re-run with --apply to delete the remote path.".to_string()),
                stdout: None,
                stderr: None,
                exit_code: 0,
                success: true,
            },
            0,
        ));
    }
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
            size: None,
            bytes_written: None,
            dry_run: false,
            action_required: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

fn rename(project_id: &str, old_path: &str, new_path: &str) -> CmdResult<FileOutput> {
    let result = files::rename(project_id, old_path, new_path)?;

    Ok((
        FileOutput {
            command: "file.rename".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: None,
            old_path: Some(result.old_path),
            new_path: Some(result.new_path),
            recursive: None,
            entries: None,
            content: None,
            size: None,
            bytes_written: None,
            dry_run: false,
            action_required: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

fn find(
    project_id: &str,
    path: &str,
    name_pattern: Option<&str>,
    file_type: Option<&str>,
    max_depth: Option<u32>,
) -> CmdResult<FileFindOutput> {
    let result = files::find(project_id, path, name_pattern, file_type, max_depth)?;
    let match_count = result.matches.len();

    Ok((
        FileFindOutput {
            command: "file.find".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: result.path,
            pattern: result.pattern,
            matches: result.matches,
            match_count,
        },
        0,
    ))
}

fn grep(
    project_id: &str,
    path: &str,
    pattern: &str,
    name_filter: Option<&str>,
    max_depth: Option<u32>,
    case_insensitive: bool,
) -> CmdResult<FileGrepOutput> {
    let result = files::grep(
        project_id,
        path,
        pattern,
        name_filter,
        max_depth,
        case_insensitive,
    )?;
    let match_count = result.matches.len();

    Ok((
        FileGrepOutput {
            command: "file.grep".to_string(),
            project_id: project_id.to_string(),
            base_path: result.base_path,
            path: result.path,
            pattern: result.pattern,
            matches: result.matches,
            match_count,
        },
        0,
    ))
}

fn edit(args: EditArgs) -> CmdResult<FileEditOutput> {
    let EditArgs {
        project_id,
        file_path,
        dry_run,
        force,
        line_ops,
        pattern_ops,
        file_mods,
    } = args;
    let edit_options = files::EditOptions { dry_run, force };

    let result = if let Some(line_num) = line_ops.replace_line {
        let content = line_ops.replace_line_content.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "content",
                "Content required for --replace-line",
                None,
                None,
            )
        })?;
        files::edit_replace_line_with_options(
            &project_id,
            &file_path,
            line_num,
            &content,
            edit_options,
        )?
    } else if let Some(line_num) = line_ops.insert_after {
        let content = line_ops.insert_after_content.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "content",
                "Content required for --insert-after",
                None,
                None,
            )
        })?;
        files::edit_insert_after_line_with_options(
            &project_id,
            &file_path,
            line_num,
            &content,
            edit_options,
        )?
    } else if let Some(line_num) = line_ops.insert_before {
        let content = line_ops.insert_before_content.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "content",
                "Content required for --insert-before",
                None,
                None,
            )
        })?;
        files::edit_insert_before_line_with_options(
            &project_id,
            &file_path,
            line_num,
            &content,
            edit_options,
        )?
    } else if let Some(line_num) = line_ops.delete_line {
        files::edit_delete_line_with_options(&project_id, &file_path, line_num, edit_options)?
    } else if let Some(lines) = line_ops.delete_lines {
        if lines.len() != 2 {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "delete_lines",
                "DELETE_LINES requires exactly 2 values: START END",
                None,
                None,
            ));
        }
        files::edit_delete_lines_with_options(
            &project_id,
            &file_path,
            lines[0],
            lines[1],
            edit_options,
        )?
    } else if let Some(pattern) = pattern_ops.replace_pattern {
        let replacement = pattern_ops.replace_pattern_content.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "content",
                "Content required for --replace-pattern",
                None,
                None,
            )
        })?;
        files::edit_replace_pattern_with_options(
            &project_id,
            &file_path,
            &pattern,
            &replacement,
            false,
            edit_options,
        )?
    } else if let Some(pattern) = pattern_ops.replace_all_pattern {
        let replacement = pattern_ops.replace_all_content.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "content",
                "Content required for --replace-all-pattern",
                None,
                None,
            )
        })?;
        files::edit_replace_pattern_with_options(
            &project_id,
            &file_path,
            &pattern,
            &replacement,
            true,
            edit_options,
        )?
    } else if let Some(pattern) = pattern_ops.delete_pattern {
        files::edit_delete_pattern_with_options(&project_id, &file_path, &pattern, edit_options)?
    } else if let Some(content) = file_mods.append {
        files::edit_append_with_options(&project_id, &file_path, &content, edit_options)?
    } else if let Some(content) = file_mods.prepend {
        files::edit_prepend_with_options(&project_id, &file_path, &content, edit_options)?
    } else {
        return Err(homeboy::core::Error::validation_invalid_argument(
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
            dry_run,
            changes_made: result.changes_made,
            change_count,
            success: result.success,
            error: result.error,
        },
        0,
    ))
}

#[cfg(test)]
#[path = "../../../tests/commands/file_test.rs"]
mod file_test;

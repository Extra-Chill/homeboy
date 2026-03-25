//! helpers — extracted from file.rs.

use super::super::CmdResult;
use super::read;
use super::FileArgs;
use super::FileCommand;
use super::FileCommandOutput;
use super::FileDownloadOutput;
use super::FileFindOutput;
use super::FileGrepOutput;
use super::FileOutput;
use clap::{Args, Subcommand};
use homeboy::project::files::{self, FileEntry, GrepMatch, LineChange};
use serde::Serialize;

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
        FileCommand::Write { project_id, path } => {
            let (out, code) = write(&project_id, &path)?;
            Ok((FileCommandOutput::Standard(out), code))
        }
        FileCommand::Delete {
            project_id,
            path,
            recursive,
        } => {
            let (out, code) = delete(&project_id, &path, recursive)?;
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
        FileCommand::Edit(args) => {
            let (out, code) = edit(args)?;
            Ok((FileCommandOutput::Edit(out), code))
        }
    }
}

pub(crate) fn list(project_id: &str, path: &str) -> CmdResult<FileOutput> {
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
            bytes_written: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

pub(crate) fn write(project_id: &str, path: &str) -> CmdResult<FileOutput> {
    let content = files::read_stdin()?;
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
            bytes_written: Some(result.bytes_written),
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

pub(crate) fn rename(project_id: &str, old_path: &str, new_path: &str) -> CmdResult<FileOutput> {
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
            bytes_written: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

pub(crate) fn find(
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

pub(crate) fn grep(
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

use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::remote_files::{self, FileEntry, GrepMatch};

#[derive(Args)]
pub struct FileArgs {
    #[command(subcommand)]
    command: FileCommand,
}

#[derive(Subcommand)]
enum FileCommand {
    /// List directory contents
    List {
        /// Project ID
        project_id: String,
        /// Remote directory path
        path: String,
    },
    /// Read file content
    Read {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
    },
    /// Write content to file (from stdin)
    Write {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
    },
    /// Delete a file or directory
    Delete {
        /// Project ID
        project_id: String,
        /// Remote path to delete
        path: String,
        /// Delete directories recursively
        #[arg(short, long)]
        recursive: bool,
    },
    /// Rename or move a file
    Rename {
        /// Project ID
        project_id: String,
        /// Current path
        old_path: String,
        /// New path
        new_path: String,
    },
    /// Find files by name pattern
    Find {
        /// Project ID
        project_id: String,
        /// Directory path to search
        path: String,
        /// Filename pattern (glob, e.g., "*.php")
        #[arg(long)]
        name: Option<String>,
        /// File type: f (file), d (directory), l (symlink)
        #[arg(long, name = "type")]
        file_type: Option<String>,
        /// Maximum directory depth
        #[arg(long)]
        max_depth: Option<u32>,
    },
    /// Search file contents
    Grep {
        /// Project ID
        project_id: String,
        /// Directory path to search
        path: String,
        /// Search pattern
        pattern: String,
        /// Filter files by name pattern (e.g., "*.php")
        #[arg(long)]
        name: Option<String>,
        /// Maximum directory depth
        #[arg(long)]
        max_depth: Option<u32>,
        /// Case insensitive search
        #[arg(short = 'i', long)]
        ignore_case: bool,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOutput {
    command: String,
    project_id: String,
    base_path: Option<String>,
    path: Option<String>,
    old_path: Option<String>,
    new_path: Option<String>,
    recursive: Option<bool>,
    entries: Option<Vec<FileEntry>>,
    content: Option<String>,
    bytes_written: Option<usize>,
    stdout: Option<String>,
    stderr: Option<String>,
    exit_code: i32,
    success: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFindOutput {
    command: String,
    project_id: String,
    base_path: Option<String>,
    path: String,
    pattern: Option<String>,
    matches: Vec<String>,
    match_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileGrepOutput {
    command: String,
    project_id: String,
    base_path: Option<String>,
    path: String,
    pattern: String,
    matches: Vec<GrepMatch>,
    match_count: usize,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum FileCommandOutput {
    Standard(FileOutput),
    Find(FileFindOutput),
    Grep(FileGrepOutput),
}

pub fn run(
    args: FileArgs,
    _global: &crate::commands::GlobalArgs,
) -> homeboy::Result<(FileCommandOutput, i32)> {
    match args.command {
        FileCommand::List { project_id, path } => {
            let (out, code) = list(&project_id, &path)?;
            Ok((FileCommandOutput::Standard(out), code))
        }
        FileCommand::Read { project_id, path } => {
            let (out, code) = read(&project_id, &path)?;
            Ok((FileCommandOutput::Standard(out), code))
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
    }
}

fn list(project_id: &str, path: &str) -> homeboy::Result<(FileOutput, i32)> {
    let result = remote_files::list(project_id, path)?;

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

fn read(project_id: &str, path: &str) -> homeboy::Result<(FileOutput, i32)> {
    let result = remote_files::read(project_id, path)?;

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
            bytes_written: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

fn write(project_id: &str, path: &str) -> homeboy::Result<(FileOutput, i32)> {
    let content = remote_files::read_stdin()?;
    let result = remote_files::write(project_id, path, &content)?;

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

fn delete(project_id: &str, path: &str, recursive: bool) -> homeboy::Result<(FileOutput, i32)> {
    let result = remote_files::delete(project_id, path, recursive)?;

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

fn rename(project_id: &str, old_path: &str, new_path: &str) -> homeboy::Result<(FileOutput, i32)> {
    let result = remote_files::rename(project_id, old_path, new_path)?;

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

fn find(
    project_id: &str,
    path: &str,
    name_pattern: Option<&str>,
    file_type: Option<&str>,
    max_depth: Option<u32>,
) -> homeboy::Result<(FileFindOutput, i32)> {
    let result = remote_files::find(project_id, path, name_pattern, file_type, max_depth)?;
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
) -> homeboy::Result<(FileGrepOutput, i32)> {
    let result = remote_files::grep(
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

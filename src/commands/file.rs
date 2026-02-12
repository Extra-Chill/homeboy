use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::files::{self, FileEntry, GrepMatch, LineChange};

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
        /// Output raw content only (no JSON wrapper)
        #[arg(long)]
        raw: bool,
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
    /// Download a file or directory from remote server
    Download {
        /// Project ID
        project_id: String,
        /// Remote file path
        path: String,
        /// Local destination path (defaults to current directory)
        #[arg(default_value = ".")]
        local_path: String,
        /// Download directories recursively
        #[arg(short, long)]
        recursive: bool,
    },
    /// Edit file with line-based or pattern-based operations
    Edit(EditArgs),
}

#[derive(Args)]
struct EditArgs {
    /// Project ID
    project_id: String,
    /// Remote file path
    file_path: String,
    /// Show changes without applying
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Apply even if multiple pattern matches (warns by default)
    #[arg(short, long)]
    force: bool,
    #[command(flatten)]
    line_ops: LineOperations,
    #[command(flatten)]
    pattern_ops: PatternOperations,
    #[command(flatten)]
    file_mods: FileModifications,
}

#[derive(Args, Default)]
struct LineOperations {
    #[arg(long)]
    replace_line: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "replace_line")]
    replace_line_content: Option<String>,
    #[arg(long)]
    insert_after: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "insert_after")]
    insert_after_content: Option<String>,
    #[arg(long)]
    insert_before: Option<usize>,
    #[arg(long, value_name = "CONTENT", requires = "insert_before")]
    insert_before_content: Option<String>,
    #[arg(long)]
    delete_line: Option<usize>,
    #[arg(long, value_names = ["START", "END"])]
    delete_lines: Option<Vec<usize>>,
}

#[derive(Args, Default)]
struct PatternOperations {
    #[arg(long, value_name = "PATTERN")]
    replace_pattern: Option<String>,
    #[arg(long, value_name = "CONTENT", requires = "replace_pattern")]
    replace_pattern_content: Option<String>,
    #[arg(long)]
    replace_all_pattern: Option<String>,
    #[arg(long, value_name = "CONTENT", requires = "replace_all_pattern")]
    replace_all_content: Option<String>,
    #[arg(long, value_name = "PATTERN")]
    delete_pattern: Option<String>,
}

#[derive(Args, Default)]
struct FileModifications {
    #[arg(long, value_name = "CONTENT")]
    append: Option<String>,
    #[arg(long, value_name = "CONTENT")]
    prepend: Option<String>,
}

#[derive(Serialize)]

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

pub struct FileEditOutput {
    command: String,
    project_id: String,
    base_path: Option<String>,
    path: String,
    changes_made: Vec<LineChange>,
    change_count: usize,
    success: bool,
    error: Option<String>,
}

#[derive(Serialize)]
pub struct FileDownloadOutput {
    command: String,
    project_id: String,
    remote_path: String,
    local_path: String,
    recursive: bool,
    success: bool,
    exit_code: i32,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum FileCommandOutput {
    Standard(FileOutput),
    Find(FileFindOutput),
    Grep(FileGrepOutput),
    Edit(FileEditOutput),
    Download(FileDownloadOutput),
    Raw(String),
}

pub fn is_raw_read(args: &FileArgs) -> bool {
    matches!(&args.command, FileCommand::Read { raw: true, .. })
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
        FileCommand::Read { project_id, path, raw } => {
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

fn list(project_id: &str, path: &str) -> homeboy::Result<(FileOutput, i32)> {
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

fn read(project_id: &str, path: &str) -> homeboy::Result<(FileOutput, i32)> {
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

fn delete(project_id: &str, path: &str, recursive: bool) -> homeboy::Result<(FileOutput, i32)> {
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

fn rename(project_id: &str, old_path: &str, new_path: &str) -> homeboy::Result<(FileOutput, i32)> {
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

fn find(
    project_id: &str,
    path: &str,
    name_pattern: Option<&str>,
    file_type: Option<&str>,
    max_depth: Option<u32>,
) -> homeboy::Result<(FileFindOutput, i32)> {
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
) -> homeboy::Result<(FileGrepOutput, i32)> {
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

fn edit(args: EditArgs) -> homeboy::Result<(FileEditOutput, i32)> {
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
        let content = line_ops
            .replace_line_content
            .ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "content",
                    "Content required for --replace-line",
                    None,
                    None,
                )
            })?;
        files::edit_replace_line(&project_id, &file_path, line_num, &content)?
    } else if let Some(line_num) = line_ops.insert_after {
        let content = line_ops
            .insert_after_content
            .ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "content",
                    "Content required for --insert-after",
                    None,
                    None,
                )
            })?;
        files::edit_insert_after_line(&project_id, &file_path, line_num, &content)?
    } else if let Some(line_num) = line_ops.insert_before {
        let content = line_ops
            .insert_before_content
            .ok_or_else(|| {
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

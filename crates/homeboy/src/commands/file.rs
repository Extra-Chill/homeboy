use clap::{Args, Subcommand};
use homeboy_core::context::resolve_project_ssh;
use serde::Serialize;
use std::io::{self, Read};

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

pub fn run(
    args: FileArgs,
    _global: &crate::commands::GlobalArgs,
) -> homeboy_core::Result<(FileOutput, i32)> {
    match args.command {
        FileCommand::List { project_id, path } => list(&project_id, &path),
        FileCommand::Read { project_id, path } => read(&project_id, &path),
        FileCommand::Write { project_id, path } => write(&project_id, &path),
        FileCommand::Delete {
            project_id,
            path,
            recursive,
        } => delete(&project_id, &path, recursive),
        FileCommand::Rename {
            project_id,
            old_path,
            new_path,
        } => rename(&project_id, &old_path, &new_path),
    }
}

fn list(project_id: &str, path: &str) -> homeboy_core::Result<(FileOutput, i32)> {
    let ctx = resolve_project_ssh(project_id)?;

    let full_path = homeboy_core::base_path::join_remote_path(ctx.base_path.as_deref(), path)?;
    let command = format!("ls -la '{}'", full_path);
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(homeboy_core::Error::other(format!(
            "LIST_FAILED: {}",
            output.stderr
        )));
    }

    let entries = parse_ls_output(&output.stdout, &full_path);

    Ok((
        FileOutput {
            command: "file.list".to_string(),
            project_id: project_id.to_string(),
            base_path: ctx.base_path.clone(),
            path: Some(full_path),
            old_path: None,
            new_path: None,
            recursive: None,
            entries: Some(entries),
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

fn read(project_id: &str, path: &str) -> homeboy_core::Result<(FileOutput, i32)> {
    let ctx = resolve_project_ssh(project_id)?;

    let full_path = homeboy_core::base_path::join_remote_path(ctx.base_path.as_deref(), path)?;
    let command = format!("cat '{}'", full_path);
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(homeboy_core::Error::other(format!(
            "READ_FAILED: {}",
            output.stderr
        )));
    }

    Ok((
        FileOutput {
            command: "file.read".to_string(),
            project_id: project_id.to_string(),
            base_path: ctx.base_path.clone(),
            path: Some(full_path),
            old_path: None,
            new_path: None,
            recursive: None,
            entries: None,
            content: Some(output.stdout),
            bytes_written: None,
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

fn write(project_id: &str, path: &str) -> homeboy_core::Result<(FileOutput, i32)> {
    let ctx = resolve_project_ssh(project_id)?;

    let mut content = String::new();
    io::stdin()
        .read_to_string(&mut content)
        .map_err(|e| homeboy_core::Error::other(format!("STDIN_ERROR: {}", e)))?;

    if content.ends_with('\n') {
        content.pop();
    }

    let full_path = homeboy_core::base_path::join_remote_path(ctx.base_path.as_deref(), path)?;
    let command = format!(
        "cat > '{}' << 'HOMEBOYEOF'\n{}\nHOMEBOYEOF",
        full_path, content
    );
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(homeboy_core::Error::other(format!(
            "WRITE_FAILED: {}",
            output.stderr
        )));
    }

    Ok((
        FileOutput {
            command: "file.write".to_string(),
            project_id: project_id.to_string(),
            base_path: ctx.base_path.clone(),
            path: Some(full_path),
            old_path: None,
            new_path: None,
            recursive: None,
            entries: None,
            content: None,
            bytes_written: Some(content.len()),
            stdout: None,
            stderr: None,
            exit_code: 0,
            success: true,
        },
        0,
    ))
}

fn delete(
    project_id: &str,
    path: &str,
    recursive: bool,
) -> homeboy_core::Result<(FileOutput, i32)> {
    let ctx = resolve_project_ssh(project_id)?;

    let full_path = homeboy_core::base_path::join_remote_path(ctx.base_path.as_deref(), path)?;
    let flags = if recursive { "-rf" } else { "-f" };
    let command = format!("rm {} '{}'", flags, full_path);
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(homeboy_core::Error::other(format!(
            "DELETE_FAILED: {}",
            output.stderr
        )));
    }

    Ok((
        FileOutput {
            command: "file.delete".to_string(),
            project_id: project_id.to_string(),
            base_path: ctx.base_path.clone(),
            path: Some(full_path),
            old_path: None,
            new_path: None,
            recursive: Some(recursive),
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

fn rename(
    project_id: &str,
    old_path: &str,
    new_path: &str,
) -> homeboy_core::Result<(FileOutput, i32)> {
    let ctx = resolve_project_ssh(project_id)?;

    let full_old = homeboy_core::base_path::join_remote_path(ctx.base_path.as_deref(), old_path)?;
    let full_new = homeboy_core::base_path::join_remote_path(ctx.base_path.as_deref(), new_path)?;
    let command = format!("mv '{}' '{}'", full_old, full_new);
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(homeboy_core::Error::other(format!(
            "RENAME_FAILED: {}",
            output.stderr
        )));
    }

    Ok((
        FileOutput {
            command: "file.rename".to_string(),
            project_id: project_id.to_string(),
            base_path: ctx.base_path.clone(),
            path: None,
            old_path: Some(full_old),
            new_path: Some(full_new),
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

#[derive(Serialize)]
struct FileEntry {
    name: String,
    path: String,
    #[serde(rename = "isDirectory")]
    is_directory: bool,
    size: Option<i64>,
    permissions: String,
}

fn parse_ls_output(output: &str, base_path: &str) -> Vec<FileEntry> {
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
        homeboy_core::token::cmp_case_insensitive(&a.name, &b.name)
    });

    entries
}

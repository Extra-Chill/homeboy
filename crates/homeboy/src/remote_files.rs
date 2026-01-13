use serde::Serialize;
use std::io::{self, Read};

use crate::context::resolve_project_ssh;
use crate::error::{Error, Result};
use crate::{base_path, shell, token};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: Option<i64>,
    pub permissions: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub base_path: Option<String>,
    pub path: String,
    pub entries: Vec<FileEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub base_path: Option<String>,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResult {
    pub base_path: Option<String>,
    pub path: String,
    pub bytes_written: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub base_path: Option<String>,
    pub path: String,
    pub recursive: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
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

/// List directory contents on remote server.
pub fn list(project_id: &str, path: &str) -> Result<ListResult> {
    let ctx = resolve_project_ssh(project_id)?;
    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;
    let command = format!("ls -la {}", shell::quote_path(&full_path));
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(Error::other(format!("LIST_FAILED: {}", output.stderr)));
    }

    let entries = parse_ls_output(&output.stdout, &full_path);

    Ok(ListResult {
        base_path: ctx.base_path.clone(),
        path: full_path,
        entries,
    })
}

/// Read file content from remote server.
pub fn read(project_id: &str, path: &str) -> Result<ReadResult> {
    let ctx = resolve_project_ssh(project_id)?;
    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;
    let command = format!("cat {}", shell::quote_path(&full_path));
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(Error::other(format!("READ_FAILED: {}", output.stderr)));
    }

    Ok(ReadResult {
        base_path: ctx.base_path.clone(),
        path: full_path,
        content: output.stdout,
    })
}

/// Write content to file on remote server.
pub fn write(project_id: &str, path: &str, content: &str) -> Result<WriteResult> {
    let ctx = resolve_project_ssh(project_id)?;
    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;
    let command = format!(
        "cat > {} << 'HOMEBOYEOF'\n{}\nHOMEBOYEOF",
        shell::quote_path(&full_path),
        content
    );
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(Error::other(format!("WRITE_FAILED: {}", output.stderr)));
    }

    Ok(WriteResult {
        base_path: ctx.base_path.clone(),
        path: full_path,
        bytes_written: content.len(),
    })
}

/// Delete file or directory on remote server.
pub fn delete(project_id: &str, path: &str, recursive: bool) -> Result<DeleteResult> {
    let ctx = resolve_project_ssh(project_id)?;
    let full_path = base_path::join_remote_path(ctx.base_path.as_deref(), path)?;
    let flags = if recursive { "-rf" } else { "-f" };
    let command = format!("rm {} {}", flags, shell::quote_path(&full_path));
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(Error::other(format!("DELETE_FAILED: {}", output.stderr)));
    }

    Ok(DeleteResult {
        base_path: ctx.base_path.clone(),
        path: full_path,
        recursive,
    })
}

/// Rename or move file on remote server.
pub fn rename(project_id: &str, old_path: &str, new_path: &str) -> Result<RenameResult> {
    let ctx = resolve_project_ssh(project_id)?;
    let full_old = base_path::join_remote_path(ctx.base_path.as_deref(), old_path)?;
    let full_new = base_path::join_remote_path(ctx.base_path.as_deref(), new_path)?;
    let command = format!(
        "mv {} {}",
        shell::quote_path(&full_old),
        shell::quote_path(&full_new)
    );
    let output = ctx.client.execute(&command);

    if !output.success {
        return Err(Error::other(format!("RENAME_FAILED: {}", output.stderr)));
    }

    Ok(RenameResult {
        base_path: ctx.base_path.clone(),
        old_path: full_old,
        new_path: full_new,
    })
}

//! File operations.
//!
//! Provides file browsing, reading, writing, and searching.
//! Routes to local or SSH execution based on project configuration.

use serde::Serialize;
use std::io::{self, Read};

use crate::core::context::{require_project_base_path, resolve_project_ssh_with_base_path};
use crate::core::defaults;
use crate::core::engine::executor::execute_for_project;
use crate::core::engine::text;
use crate::core::engine::{command, shell};
use crate::core::error::{Error, Result};
use crate::core::paths::resolve_path_string;
use crate::core::project;
use crate::core::server::CommandOutput;

use std::path::Path;
use std::process::Command;

mod edit;

const STDIN_CONTENT_LIMIT_BYTES: u64 = 1024 * 1024;

pub use edit::{
    edit_append, edit_append_with_options, edit_delete_line, edit_delete_line_with_options,
    edit_delete_lines, edit_delete_lines_with_options, edit_delete_pattern,
    edit_delete_pattern_with_options, edit_insert_after_line, edit_insert_after_line_with_options,
    edit_insert_before_line, edit_insert_before_line_with_options, edit_prepend,
    edit_prepend_with_options, edit_replace_line, edit_replace_line_with_options,
    edit_replace_pattern, edit_replace_pattern_with_options, EditOptions, EditResult, LineChange,
};

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
    pub size: Option<i64>,
    pub content: String,
}

fn parse_file_size(output: &str) -> Option<i64> {
    output.trim().parse().ok()
}

fn file_size(project: &project::Project, full_path: &str) -> Option<i64> {
    let command = format!("wc -c < {}", shell::quote_path(full_path));
    let output = execute_for_project(project, &command).ok()?;

    if !output.success {
        return None;
    }

    parse_file_size(&output.stdout)
}

fn require_file_command_success(
    output: &CommandOutput,
    operation: &str,
    resolved_path: &str,
) -> Result<()> {
    if output.success {
        return Ok(());
    }

    let low_level_error = if output.stderr.trim().is_empty() {
        format!(
            "command exited without stderr (exit_code={})",
            output.exit_code
        )
    } else {
        format!(
            "exit_code={}; stderr={}",
            output.exit_code,
            output.stderr.trim()
        )
    };

    Err(Error::internal_io(
        format!(
            "{}_FAILED: path={}; {}",
            operation, resolved_path, low_level_error
        ),
        Some(operation.to_string()),
    ))
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
fn parse_ls_output(output: &str, base_path: &str) -> Vec<FileEntry> {
    let mut entries: Vec<FileEntry> =
        text::lines_filtered(output, |line| !line.starts_with("total "))
            .filter_map(|line| parse_ls_line(line, base_path))
            .collect();

    entries.sort_by(|a, b| {
        if a.is_directory != b.is_directory {
            return b.is_directory.cmp(&a.is_directory);
        }
        text::cmp_case_insensitive(&a.name, &b.name)
    });

    entries
}

fn parse_ls_line(line: &str, base_path: &str) -> Option<FileEntry> {
    let parts = text::split_whitespace(line, 9)?;

    let permissions = parts[0];
    let name = parts[8..].join(" ");

    if name == "." || name == ".." {
        return None;
    }

    Some(FileEntry {
        name: name.clone(),
        path: resolve_path_string(base_path, &name),
        is_directory: permissions.starts_with('d'),
        size: parts[4].parse().ok(),
        permissions: permissions[1..].to_string(),
    })
}

/// Read content from stdin, stripping trailing newline.
pub fn read_stdin() -> Result<String> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(STDIN_CONTENT_LIMIT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to read stdin: {}", e),
                Some("read stdin".to_string()),
            )
        })?;
    if bytes.len() > STDIN_CONTENT_LIMIT_BYTES as usize {
        return Err(Error::validation_invalid_argument(
            "stdin",
            "stdin content exceeds the retained byte limit",
            Some(format!("limit_bytes={STDIN_CONTENT_LIMIT_BYTES}")),
            None,
        ));
    }
    let mut content = String::from_utf8(bytes).map_err(|e| {
        Error::internal_io(
            format!("Failed to decode stdin as UTF-8: {}", e),
            Some("read stdin".to_string()),
        )
    })?;

    if content.ends_with('\n') {
        content.pop();
    }

    Ok(content)
}

/// Resolve a remote file path against a project, honoring managed path_roots.
///
/// Relative paths matching an extension-declared managed prefix (e.g.
/// `wp-content`) resolve through the project's `path_roots` exactly like
/// deploy, so file inspection agrees with the deployed path even when the
/// active component directory lives outside `base_path` (e.g. WP Cloud). (#5456)
fn resolve_remote_path(
    project: &project::Project,
    base_path_value: &str,
    path: &str,
) -> Result<String> {
    super::resolve_project_remote_path(project, base_path_value, path)
}

/// List directory contents.
pub fn list(project_id: &str, path: &str) -> Result<ListResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = resolve_remote_path(&project, &project_base_path, path)?;
    let command = format!("ls -la {}", shell::quote_path(&full_path));
    let output = execute_for_project(&project, &command)?;
    require_file_command_success(&output, "LIST", &full_path)?;

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
    let full_path = resolve_remote_path(&project, &project_base_path, path)?;
    let command = format!("cat {}", shell::quote_path(&full_path));
    let output = execute_for_project(&project, &command)?;
    require_file_command_success(&output, "READ", &full_path)?;
    let size = file_size(&project, &full_path);

    Ok(ReadResult {
        base_path: Some(project_base_path),
        path: full_path,
        size,
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

/// Build the shell command that writes `content` to `quoted_path` byte-for-byte.
///
/// A heredoc always appends a single trailing newline after its body, so writing
/// `content` directly would silently add a `\n` that the caller never asked for
/// (e.g. a pattern replacement on a file with no final newline). To preserve the
/// caller's exact trailing-newline state we strip one trailing newline from the
/// heredoc body (the heredoc re-adds exactly one) and, when the original content
/// had no trailing newline at all, drop the heredoc's extra byte afterwards.
fn write_content_command(quoted_path: &str, content: &str) -> String {
    let delimiter = generate_unique_delimiter(content);
    // The heredoc body emits `body` + a single trailing newline. Removing one
    // trailing newline from `content` keeps multi-newline endings intact while
    // letting the heredoc supply the final newline for content that ends in one.
    let body = content.strip_suffix('\n').unwrap_or(content);
    let mut command = format!("cat > {quoted_path} << '{delimiter}'\n{body}\n{delimiter}");

    if !content.ends_with('\n') {
        // Heredoc added a trailing newline the caller never wanted; drop it so the
        // written file matches `content` exactly.
        command.push_str(&format!("\ntruncate -s -1 {quoted_path}"));
    }

    command
}

/// Write content to file.
pub fn write(project_id: &str, path: &str, content: &str) -> Result<WriteResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_path = resolve_remote_path(&project, &project_base_path, path)?;
    let command = write_content_command(&shell::quote_path(&full_path), content);
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
    let full_path = resolve_remote_path(&project, &project_base_path, path)?;
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
    let full_old = resolve_remote_path(&project, &project_base_path, old_path)?;
    let full_new = resolve_remote_path(&project, &project_base_path, new_path)?;
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

/// Parse find output into list of matching paths.
fn parse_find_output(output: &str) -> Vec<String> {
    text::lines(output).map(|s| s.to_string()).collect()
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
    let full_path = resolve_remote_path(&project, &project_base_path, path)?;

    let mut cmd = format!("find {}", shell::quote_path(&full_path));

    if let Some(depth) = max_depth {
        cmd.push_str(&format!(" -maxdepth {}", depth));
    }

    if let Some(t) = file_type {
        match t {
            "f" | "d" | "l" => cmd.push_str(&format!(" -type {}", t)),
            _ => {
                return Err(Error::validation_invalid_argument(
                    "file_type",
                    "Invalid file type. Use 'f', 'd', or 'l'.",
                    Some(t.to_string()),
                    Some(vec!["f".to_string(), "d".to_string(), "l".to_string()]),
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
    let full_path = resolve_remote_path(&project, &project_base_path, path)?;

    if pattern.trim().is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "pattern".to_string()
        ]));
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
    let full_remote_path = resolve_remote_path(&ctx.project, &project_base_path, remote_path)?;

    // Create local parent directories if needed
    let local = Path::new(local_path);
    if let Some(parent) = local.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    format!("Failed to create local directory: {}", e),
                    Some("create local directory".to_string()),
                )
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
    log_status!(
        "download",
        "Downloading {}: {}@{}:{} -> {}",
        label,
        ctx.client.user,
        ctx.client.host,
        full_remote_path,
        local_path
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.txt");
        let content = "hello\nworld";
        std::fs::write(&path, content).expect("write sample file");

        let project = project::Project::default();

        assert_eq!(
            file_size(&project, &path.to_string_lossy()),
            Some(content.len() as i64)
        );
    }

    #[test]
    fn test_read_stdin() {
        let read_stdin_fn: fn() -> Result<String> = read_stdin;

        let _ = read_stdin_fn;
    }

    #[test]
    fn test_list() {
        let entries = parse_ls_output(
            "-rw-r--r--  1 user group 12 Jan  1 00:00 file.txt\n",
            "/tmp",
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/tmp/file.txt");
        assert_eq!(entries[0].size, Some(12));
    }

    #[test]
    fn test_write() {
        let write_fn: fn(&str, &str, &str) -> Result<WriteResult> = write;

        let _ = write_fn;
    }

    #[test]
    fn test_delete() {
        let delete_fn: fn(&str, &str, bool) -> Result<DeleteResult> = delete;

        let _ = delete_fn;
    }

    #[test]
    fn test_rename() {
        let rename_fn: fn(&str, &str, &str) -> Result<RenameResult> = rename;

        let _ = rename_fn;
    }

    #[test]
    fn test_find() {
        let matches = parse_find_output("/tmp/a\n/tmp/b\n");

        assert_eq!(matches, vec!["/tmp/a", "/tmp/b"]);
    }

    #[test]
    fn test_grep() {
        let matches = parse_grep_output("/tmp/file.txt:3:needle\n");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file, "/tmp/file.txt");
        assert_eq!(matches[0].line, 3);
        assert_eq!(matches[0].content, "needle");
    }

    #[test]
    fn test_download() {
        let download_fn: fn(&str, &str, &str, bool) -> Result<DownloadResult> = download;

        let _ = download_fn;
    }

    #[test]
    fn parse_file_size_accepts_wc_output() {
        assert_eq!(parse_file_size("      123\n"), Some(123));
    }

    #[test]
    fn parse_file_size_rejects_unavailable_output() {
        assert_eq!(parse_file_size(""), None);
        assert_eq!(parse_file_size("not a size"), None);
    }

    #[test]
    fn read_failure_message_includes_path_and_exit_code_when_stderr_is_blank() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: false,
            exit_code: 126,
            child_resource: None,
        };

        let error = require_file_command_success(&output, "READ", "/srv/site/blocked.txt")
            .expect_err("blank stderr read failure should return diagnostics");
        let details = error.details;
        let message = details
            .get("error")
            .and_then(|value| value.as_str())
            .expect("internal IO details include error message");

        assert!(message.contains("READ_FAILED: path=/srv/site/blocked.txt"));
        assert!(message.contains("command exited without stderr (exit_code=126)"));
    }

    #[test]
    fn list_failure_message_includes_path_exit_code_and_stderr() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "permission denied\n".to_string(),
            success: false,
            exit_code: 2,
            child_resource: None,
        };

        let error = require_file_command_success(&output, "LIST", "/srv/site/private")
            .expect_err("list failure should return diagnostics");
        let details = error.details;
        let message = details
            .get("error")
            .and_then(|value| value.as_str())
            .expect("internal IO details include error message");

        assert!(message.contains("LIST_FAILED: path=/srv/site/private"));
        assert!(message.contains("exit_code=2; stderr=permission denied"));
    }
}

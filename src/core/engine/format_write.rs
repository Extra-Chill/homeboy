//! Post-write formatting for code-modifying commands.
//!
//! Any command that writes source code (`refactor --write`) should
//! call `format_after_write()` after writing files. This runs the project's
//! extension-owned formatter to ensure generated code matches project style.
//!
//! Unlike `validate_write`, formatting failure is non-fatal — it logs a warning
//! but never rolls back. Generated code that compiles but isn't formatted is
//! better than no code at all.
//!
//! The format command is resolved via extension manifest `scripts.format`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::error::{Error, Result};
use crate::core::extension;

/// Result of a post-write format operation.
#[derive(Debug, Clone, Serialize)]
pub struct FormatResult {
    /// Whether the formatter ran successfully.
    pub success: bool,
    /// The format command that was run (or None if skipped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Formatter output (stdout/stderr combined).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Number of files in scope.
    pub files_in_scope: usize,
}

impl FormatResult {
    fn skipped(files_in_scope: usize) -> Self {
        Self {
            success: true,
            command: None,
            output: None,
            files_in_scope,
        }
    }

    fn passed(command: String, files_in_scope: usize) -> Self {
        Self {
            success: true,
            command: Some(command),
            output: None,
            files_in_scope,
        }
    }

    fn failed(command: String, output: String, files_in_scope: usize) -> Self {
        Self {
            success: false,
            command: Some(command),
            output: Some(output),
            files_in_scope,
        }
    }
}

/// Format written files using the project's language-specific formatter.
///
/// Non-fatal: formatting failure logs a warning but does not roll back or fail.
///
/// # Arguments
/// * `root` - Project root directory
/// * `changed_files` - Files that were modified/created (absolute paths)
pub fn format_after_write(root: &Path, changed_files: &[PathBuf]) -> Result<FormatResult> {
    if changed_files.is_empty() {
        return Ok(FormatResult::skipped(0));
    }

    let format_command = match resolve_format_command(root, changed_files) {
        Some(cmd) => cmd,
        None => return Ok(FormatResult::skipped(changed_files.len())),
    };

    crate::log_status!("format", "Running post-write formatter: {}", format_command);

    let output = std::process::Command::new("sh")
        .args(["-c", &format_command])
        .current_dir(root)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to run format command: {}", e),
                Some("format_after_write".to_string()),
            )
        })?;

    if output.status.success() {
        crate::log_status!("format", "Formatting complete");
        return Ok(FormatResult::passed(format_command, changed_files.len()));
    }

    // Formatting failed — log warning but do NOT rollback
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let error_output = if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        stderr.trim().to_string()
    };

    crate::log_status!(
        "format",
        "Warning: formatter exited non-zero (continuing anyway)"
    );

    Ok(FormatResult::failed(
        format_command,
        error_output,
        changed_files.len(),
    ))
}

/// Resolve the format command for a set of changed files.
///
/// Checks installed extensions for `scripts.format`.
fn resolve_format_command(_root: &Path, changed_files: &[PathBuf]) -> Option<String> {
    // Collect unique file extensions
    let extensions: Vec<String> = changed_files
        .iter()
        .filter_map(|f| {
            f.extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_string())
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Check installed extensions for a format script
    for ext in &extensions {
        if let Some(manifest) = find_extension_with_format(ext) {
            let ext_path = manifest.extension_path.as_deref()?;
            let script_rel = manifest.format_script()?;
            let script_path = std::path::Path::new(ext_path).join(script_rel);

            if script_path.exists() {
                // Invoke the script directly so its shebang resolves the interpreter.
                // Wrapping with `sh <script>` bypasses `#!/usr/bin/env bash` and runs
                // under POSIX sh — which breaks scripts using bash-only features. See #1276.
                return Some(
                    crate::core::engine::shell::quote_path(&script_path.to_string_lossy())
                        .to_string(),
                );
            }
        }
    }

    None
}

/// Find an installed extension that handles a file extension and has scripts.format.
fn find_extension_with_format(file_ext: &str) -> Option<extension::ExtensionManifest> {
    extension::load_all_extensions().ok().and_then(|manifests| {
        manifests
            .into_iter()
            .find(|m| m.handles_file_extension(file_ext) && m.format_script().is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn skipped_when_no_files() {
        let dir = TempDir::new().expect("temp dir");
        let result = format_after_write(dir.path(), &[]).unwrap();
        assert!(result.success);
        assert!(result.command.is_none());
        assert_eq!(result.files_in_scope, 0);
    }

    #[test]
    fn skipped_when_no_formatter_found() {
        let dir = TempDir::new().expect("temp dir");
        let files = vec![dir.path().join("unknown.xyz")];
        let result = format_after_write(dir.path(), &files).unwrap();
        assert!(result.success);
        assert!(result.command.is_none());
    }
}

//! Post-write formatting for code-modifying commands.
//!
//! Any command that writes source code (`refactor --write`) should
//! call `format_after_write()` after writing files. This runs the project's
//! language-specific formatter (e.g., `cargo fmt` for Rust, `prettier --write`
//! for TypeScript) to ensure generated code matches project style.
//!
//! Unlike `validate_write`, formatting failure is non-fatal — it logs a warning
//! but never rolls back. Generated code that compiles but isn't formatted is
//! better than no code at all.
//!
//! The format command is resolved via:
//! 1. Extension manifest `scripts.format` (if an extension provides one)
//! 2. Builtin fallbacks based on project marker files (Cargo.toml, tsconfig.json, etc.)

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::extension;

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

    // cargo fmt failed — try rustfmt directly on individual files.
    // This handles environments where cargo fmt needs target/ for module
    // resolution but it may not be available.
    if format_command.starts_with("cargo fmt") {
        let rust_files: Vec<&PathBuf> = changed_files
            .iter()
            .filter(|f| f.extension().and_then(|e| e.to_str()) == Some("rs"))
            .collect();

        if !rust_files.is_empty() {
            crate::log_status!(
                "format",
                "cargo fmt failed, falling back to rustfmt on {} file(s)",
                rust_files.len()
            );

            let mut all_succeeded = true;
            for file in &rust_files {
                let rustfmt_output = std::process::Command::new("rustfmt")
                    .arg(file)
                    .current_dir(root)
                    .output();

                match rustfmt_output {
                    Ok(o) if o.status.success() => {}
                    _ => {
                        all_succeeded = false;
                    }
                }
            }

            if all_succeeded {
                crate::log_status!("format", "rustfmt fallback complete");
                return Ok(FormatResult::passed(
                    "rustfmt (fallback)".to_string(),
                    changed_files.len(),
                ));
            }
        }
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
/// Checks installed extensions first (via `scripts.format`), then falls back
/// to builtin project-level formatters.
fn resolve_format_command(root: &Path, changed_files: &[PathBuf]) -> Option<String> {
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
                return Some(format!(
                    "sh {}",
                    crate::engine::shell::quote_path(&script_path.to_string_lossy())
                ));
            }
        }
    }

    // Fallback: builtin project-level formatters
    resolve_builtin_format_command(root)
}

/// Find an installed extension that handles a file extension and has scripts.format.
fn find_extension_with_format(file_ext: &str) -> Option<extension::ExtensionManifest> {
    extension::load_all_extensions().ok().and_then(|manifests| {
        manifests
            .into_iter()
            .find(|m| m.handles_file_extension(file_ext) && m.format_script().is_some())
    })
}

/// Fallback formatting using well-known project-level commands.
fn resolve_builtin_format_command(root: &Path) -> Option<String> {
    // Rust: Cargo.toml → cargo fmt
    if root.join("Cargo.toml").exists() {
        return Some("cargo fmt 2>&1".to_string());
    }

    // TypeScript/JavaScript: package.json + prettier → npx prettier --write
    if root.join("tsconfig.json").exists() || root.join("package.json").exists() {
        // Only use prettier if it's available in the project
        if root.join("node_modules/.bin/prettier").exists() {
            return Some("npx prettier --write . 2>&1".to_string());
        }
    }

    // Go: go.mod → gofmt
    if root.join("go.mod").exists() {
        return Some("gofmt -w . 2>&1".to_string());
    }

    // PHP: composer.json + phpcbf
    if root.join("composer.json").exists() && root.join("vendor/bin/phpcbf").exists() {
        return Some("vendor/bin/phpcbf 2>&1".to_string());
    }

    None
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

    #[test]
    fn test_format_after_write_changed_files_is_empty() {
        let root = Path::new("");
        let changed_files = Vec::new();
        let result = format_after_write(&root, &changed_files);
        let inner = result.unwrap();
        // Branch returns Ok(FormatResult::skipped(0) when: changed_files.is_empty()
        assert_eq!(inner.success, false);
        assert_eq!(inner.command, None);
        assert_eq!(inner.output, None);
        assert_eq!(inner.files_in_scope, 0);
    }

    #[test]
    fn test_format_after_write_changed_files_is_empty_2() {
        let root = Path::new("");
        let changed_files = Vec::new();
        let _result = format_after_write(&root, &changed_files);
    }

    #[test]
    fn test_format_after_write_none_return_ok_formatresult_skipped_changed_files_len() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let result = format_after_write(&root, &changed_files);
        let inner = result.unwrap();
        // Branch returns Ok(FormatResult::skipped(changed_files.len() when: None => return Ok(FormatResult::skipped(changed_files.len())),
        assert_eq!(inner.success, false);
        assert_eq!(inner.command, None);
        assert_eq!(inner.output, None);
        assert_eq!(inner.files_in_scope, 0);
    }

    #[test]
    fn test_format_after_write_some_format_after_write_to_string() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let _result = format_after_write(&root, &changed_files);
    }

    #[test]
    fn test_format_after_write_default_path() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let _result = format_after_write(&root, &changed_files);
    }

    #[test]
    fn test_format_after_write_output_status_success() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let result = format_after_write(&root, &changed_files);
        let inner = result.unwrap();
        // Branch returns Ok(FormatResult::passed(format_command, changed_files.len() when: output.status.success()
        assert_eq!(inner.success, false);
        assert_eq!(inner.command, None);
        assert_eq!(inner.output, None);
        assert_eq!(inner.files_in_scope, 0);
    }

    #[test]
    fn test_format_after_write_format_command_starts_with_cargo_fmt() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let _result = format_after_write(&root, &changed_files);
    }

    #[test]
    fn test_format_after_write_match_rustfmt_output() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let result = format_after_write(&root, &changed_files);
        let inner = result.unwrap();
        // Branch returns Ok(o) when: match rustfmt_output
        assert_eq!(inner.success, false);
        assert_eq!(inner.command, None);
        assert_eq!(inner.output, None);
        assert_eq!(inner.files_in_scope, 0);
    }

    #[test]
    fn test_format_after_write_has_expected_effects() {
        // Expected effects: process_spawn, logging
        let root = Path::new("");
        let changed_files = Vec::new();
        let _ = format_after_write(&root, &changed_files);
    }


    #[test]
    fn test_format_after_write_changed_files_is_empty() {
        let root = Path::new("");
        let changed_files = Vec::new();
        let result = format_after_write(&root, &changed_files);
        let inner = result.unwrap();
        // Branch returns Ok(FormatResult::skipped(0) when: changed_files.is_empty()
        assert_eq!(inner.success, false);
        assert_eq!(inner.command, None);
        assert_eq!(inner.output, None);
        assert_eq!(inner.files_in_scope, 0);
    }

    #[test]
    fn test_format_after_write_changed_files_is_empty_2() {
        let root = Path::new("");
        let changed_files = Vec::new();
        let _result = format_after_write(&root, &changed_files);
    }

    #[test]
    fn test_format_after_write_none_return_ok_formatresult_skipped_changed_files_len() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let result = format_after_write(&root, &changed_files);
        let inner = result.unwrap();
        // Branch returns Ok(FormatResult::skipped(changed_files.len() when: None => return Ok(FormatResult::skipped(changed_files.len())),
        assert_eq!(inner.success, false);
        assert_eq!(inner.command, None);
        assert_eq!(inner.output, None);
        assert_eq!(inner.files_in_scope, 0);
    }

    #[test]
    fn test_format_after_write_some_format_after_write_to_string() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let _result = format_after_write(&root, &changed_files);
    }

    #[test]
    fn test_format_after_write_default_path() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let _result = format_after_write(&root, &changed_files);
    }

    #[test]
    fn test_format_after_write_output_status_success() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let result = format_after_write(&root, &changed_files);
        let inner = result.unwrap();
        // Branch returns Ok(FormatResult::passed(format_command, changed_files.len() when: output.status.success()
        assert_eq!(inner.success, false);
        assert_eq!(inner.command, None);
        assert_eq!(inner.output, None);
        assert_eq!(inner.files_in_scope, 0);
    }

    #[test]
    fn test_format_after_write_format_command_starts_with_cargo_fmt() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let _result = format_after_write(&root, &changed_files);
    }

    #[test]
    fn test_format_after_write_match_rustfmt_output() {
        let root = Path::new("");
        let changed_files = vec![Default::default()];
        let result = format_after_write(&root, &changed_files);
        let inner = result.unwrap();
        // Branch returns Ok(o) when: match rustfmt_output
        assert_eq!(inner.success, false);
        assert_eq!(inner.command, None);
        assert_eq!(inner.output, None);
        assert_eq!(inner.files_in_scope, 0);
    }

    #[test]
    fn test_format_after_write_has_expected_effects() {
        // Expected effects: process_spawn, logging
        let root = Path::new("");
        let changed_files = Vec::new();
        let _ = format_after_write(&root, &changed_files);
    }

}

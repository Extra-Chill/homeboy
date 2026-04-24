//! Post-write compilation validation gate for code-modifying commands.
//!
//! Any command that writes source code (`refactor decompose`, `refactor move`,
//! `refactor transform`, `refactor --from audit --write`) should call `validate_write()`
//! after writing files and before reporting success. If validation fails,
//! the changed files are rolled back to their pre-write state.
//!
//! The validation command is determined by the project's extension — each language
//! extension can provide a `scripts.validate` command (e.g., `cargo check` for Rust,
//! `php -l` for PHP, `tsc --noEmit` for TypeScript).
//!
//! When no extension provides a validate script, validation is skipped (no-op success).
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/798

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::undo::InMemoryRollback;
use crate::error::{Error, Result};
use crate::extension;

/// Result of a post-write validation check.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationResult {
    /// Whether validation passed.
    pub success: bool,
    /// The validation command that was run (or None if skipped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Compiler/validator output on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Whether files were rolled back due to failure.
    pub rolled_back: bool,
    /// Number of files that were checked.
    pub files_checked: usize,
}

impl ValidationResult {
    fn skipped(files_checked: usize) -> Self {
        Self {
            success: true,
            command: None,
            output: None,
            rolled_back: false,
            files_checked,
        }
    }

    fn passed(command: String, files_checked: usize) -> Self {
        Self {
            success: true,
            command: Some(command),
            output: None,
            rolled_back: false,
            files_checked,
        }
    }

    fn failed(command: String, output: String, rolled_back: bool, files_checked: usize) -> Self {
        Self {
            success: false,
            command: Some(command),
            output: Some(output),
            rolled_back,
            files_checked,
        }
    }
}

/// Validate that written code compiles/parses correctly, with automatic rollback on failure.
///
/// # Arguments
/// * `root` - Project root directory (git root or component source path)
/// * `changed_files` - Files that were modified/created (absolute paths)
/// * `rollback` - Pre-captured file states for rollback on validation failure
///
/// # Behavior
/// 1. Finds an extension that handles the changed files' language
/// 2. Runs the extension's `scripts.validate` command
/// 3. If validation fails → rolls back all changed files, returns error details
/// 4. If validation passes → returns success
/// 5. If no validate script exists → returns success (no-op)
pub fn validate_write(
    root: &Path,
    changed_files: &[PathBuf],
    rollback: &InMemoryRollback,
) -> Result<ValidationResult> {
    if changed_files.is_empty() {
        return Ok(ValidationResult::skipped(0));
    }

    // Determine which extension provides validation for these files
    let validate_command = match resolve_validate_command(root, changed_files) {
        Some(cmd) => cmd,
        None => {
            // No extension provides validation — skip (success)
            return Ok(ValidationResult::skipped(changed_files.len()));
        }
    };

    crate::log_status!(
        "validate",
        "Running post-write validation: {}",
        validate_command
    );

    // Run the validation command in the project root
    let output = std::process::Command::new("sh")
        .args(["-c", &validate_command])
        .current_dir(root)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to run validation command: {}", e),
                Some("validate_write".to_string()),
            )
        })?;

    if output.status.success() {
        crate::log_status!("validate", "Validation passed");
        return Ok(ValidationResult::passed(
            validate_command,
            changed_files.len(),
        ));
    }

    // Validation failed — collect output and rollback
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let error_output = if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        stderr.trim().to_string()
    };

    // Truncate to last 30 lines for readability
    let truncated: String = error_output
        .lines()
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    crate::log_status!(
        "validate",
        "Validation FAILED — rolling back {} file(s)",
        rollback.len()
    );

    // Rollback all changed files
    rollback.restore_all();

    crate::log_status!("validate", "Rollback complete");

    Ok(ValidationResult::failed(
        validate_command,
        truncated,
        true,
        changed_files.len(),
    ))
}

/// Validate without rollback — for dry-run preview or when caller manages rollback.
///
/// Returns the validation result without touching any files.
pub fn validate_only(root: &Path, changed_files: &[PathBuf]) -> Result<ValidationResult> {
    if changed_files.is_empty() {
        return Ok(ValidationResult::skipped(0));
    }

    let validate_command = match resolve_validate_command(root, changed_files) {
        Some(cmd) => cmd,
        None => return Ok(ValidationResult::skipped(changed_files.len())),
    };

    let output = std::process::Command::new("sh")
        .args(["-c", &validate_command])
        .current_dir(root)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to run validation command: {}", e),
                Some("validate_only".to_string()),
            )
        })?;

    if output.status.success() {
        Ok(ValidationResult::passed(
            validate_command,
            changed_files.len(),
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let error_output = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };

        Ok(ValidationResult::failed(
            validate_command,
            error_output,
            false,
            changed_files.len(),
        ))
    }
}

/// Resolve the validation command for a set of changed files.
///
/// Looks at the file extensions of changed files, finds an extension that
/// handles that language and has a `scripts.validate` configured, then
/// returns the full command to run.
///
/// For project-level validators (Rust, TypeScript), the validate script
/// is run from the project root. For file-level validators (PHP), individual
/// files could be checked — but we run the project-level command for simplicity.
fn resolve_validate_command(root: &Path, changed_files: &[PathBuf]) -> Option<String> {
    // Collect unique file extensions from changed files
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

    // Find an extension that handles any of these file types AND has a validate script
    for ext in &extensions {
        if let Some(manifest) = find_extension_with_validate(ext) {
            let ext_path = manifest.extension_path.as_deref()?;
            let script_rel = manifest.validate_script()?;
            let script_path = std::path::Path::new(ext_path).join(script_rel);

            if script_path.exists() {
                // Invoke via `bash` rather than `sh`: the bundled
                // extension scripts (e.g. validate-syntax.sh) declare
                // `#!/usr/bin/env bash` and use bash-only constructs
                // like `< <(...)` process substitution. On macOS
                // /bin/sh is bash in sh-compat mode, which disables
                // those constructs and fails with a syntax error.
                // See https://github.com/Extra-Chill/homeboy/issues/1276.
                return Some(format!(
                    "bash {}",
                    crate::engine::shell::quote_path(&script_path.to_string_lossy())
                ));
            }
        }
    }

    // Fallback: check for well-known project-level validators
    resolve_builtin_validate_command(root)
}

/// Find an installed extension that handles a file extension and has scripts.validate.
fn find_extension_with_validate(file_ext: &str) -> Option<extension::ExtensionManifest> {
    extension::load_all_extensions().ok().and_then(|manifests| {
        manifests
            .into_iter()
            .find(|m| m.handles_file_extension(file_ext) && m.validate_script().is_some())
    })
}

/// Fallback validation using well-known project-level commands.
///
/// If no extension provides a validate script, we check for common build tools
/// that can validate without a full build.
fn resolve_builtin_validate_command(root: &Path) -> Option<String> {
    // Rust: Cargo.toml → cargo check --tests
    // --tests includes #[cfg(test)] modules so auto-generated test code
    // is validated before committing. Without it, broken test signatures,
    // duplicate names, and bad format strings slip through.
    if root.join("Cargo.toml").exists() {
        return Some("cargo check --tests 2>&1".to_string());
    }

    // TypeScript: tsconfig.json → tsc --noEmit
    if root.join("tsconfig.json").exists() {
        return Some("npx tsc --noEmit 2>&1".to_string());
    }

    // Go: go.mod → go vet
    if root.join("go.mod").exists() {
        return Some("go vet ./... 2>&1".to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_builtin_for_rust_project() {
        let dir = TempDir::new().expect("temp dir");
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"t\"").unwrap();
        let cmd = resolve_builtin_validate_command(dir.path());
        assert_eq!(cmd, Some("cargo check --tests 2>&1".to_string()));
    }

    #[test]
    fn resolve_builtin_for_typescript_project() {
        let dir = TempDir::new().expect("temp dir");
        fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        let cmd = resolve_builtin_validate_command(dir.path());
        assert_eq!(cmd, Some("npx tsc --noEmit 2>&1".to_string()));
    }

    #[test]
    fn resolve_builtin_for_go_project() {
        let dir = TempDir::new().expect("temp dir");
        fs::write(dir.path().join("go.mod"), "module example.com/t").unwrap();
        let cmd = resolve_builtin_validate_command(dir.path());
        assert_eq!(cmd, Some("go vet ./... 2>&1".to_string()));
    }

    #[test]
    fn resolve_builtin_returns_none_for_unknown() {
        let dir = TempDir::new().expect("temp dir");
        let cmd = resolve_builtin_validate_command(dir.path());
        assert!(cmd.is_none());
    }

    #[test]
    fn validation_result_skipped_is_success() {
        let result = ValidationResult::skipped(5);
        assert!(result.success);
        assert!(!result.rolled_back);
        assert!(result.command.is_none());
    }

    #[test]
    fn validate_write_with_no_files_is_success() {
        let dir = TempDir::new().expect("temp dir");
        let rollback = InMemoryRollback::new();
        let result = validate_write(dir.path(), &[], &rollback).expect("should succeed");
        assert!(result.success);
        assert_eq!(result.files_checked, 0);
    }

    #[test]
    fn validate_write_rolls_back_on_failure() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();

        // Create a Rust project with intentionally broken code
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"validate-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn good() {}\n").unwrap();

        // Capture good state
        let mut rollback = InMemoryRollback::new();
        let lib_path = root.join("src/lib.rs");
        rollback.capture(&lib_path);

        // Write broken code
        fs::write(&lib_path, "pub fn broken( {}\n").unwrap();

        let changed = vec![lib_path.clone()];
        let result = validate_write(root, &changed, &rollback).expect("should not error");

        assert!(!result.success, "validation should fail for broken code");
        assert!(result.rolled_back, "should have rolled back");
        assert!(result.output.is_some(), "should have compiler output");

        // Verify rollback happened — file should be restored
        let content = fs::read_to_string(&lib_path).unwrap();
        assert_eq!(content, "pub fn good() {}\n", "file should be restored");
    }
}

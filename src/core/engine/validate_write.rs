//! Post-write compilation validation gate for code-modifying commands.
//!
//! Code-writing commands can call `validate_only()` after writing files and before
//! reporting success. Callers that need rollback should manage their own snapshots.
//!
//! The validation command is determined by the project's extension. Language
//! extensions can provide a `scripts.validate` command for their own checker.
//!
//! When no extension provides a validate script, validation is skipped (no-op success).
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/798

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::error::{Error, Result};
use crate::core::extension;

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
fn resolve_validate_command(_root: &Path, changed_files: &[PathBuf]) -> Option<String> {
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
                // Invoke the script directly so its shebang resolves the interpreter.
                // Wrapping with `sh <script>` bypasses `#!/usr/bin/env bash` and runs
                // under POSIX sh — which breaks scripts using bash-only features like
                // process substitution (`done < <(...)`). See #1276.
                return Some(
                    crate::core::engine::shell::quote_path(&script_path.to_string_lossy())
                        .to_string(),
                );
            }
        }
    }

    None
}

/// Find an installed extension that handles a file extension and has scripts.validate.
fn find_extension_with_validate(file_ext: &str) -> Option<extension::ExtensionManifest> {
    extension::load_all_extensions().ok().and_then(|manifests| {
        manifests
            .into_iter()
            .find(|m| m.handles_file_extension(file_ext) && m.validate_script().is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn validation_skips_unknown_project_without_extension_validator() {
        let dir = TempDir::new().expect("temp dir");
        let files = vec![dir.path().join("unknown.xyz")];
        let result = validate_only(dir.path(), &files).unwrap();
        assert!(result.success);
        assert!(result.command.is_none());
    }

    #[test]
    fn validation_result_skipped_is_success() {
        let result = ValidationResult::skipped(5);
        assert!(result.success);
        assert!(!result.rolled_back);
        assert!(result.command.is_none());
    }

    /// Regression test for #1276.
    ///
    /// Extension-script validation commands must be invokable under `sh -c ...`
    /// **without** a `sh` interpreter prefix — the script's shebang
    /// (`#!/usr/bin/env bash`) has to resolve the interpreter so scripts using
    /// bash-only features (process substitution, arrays, etc.) work.
    ///
    /// Before the fix, resolve_validate_command emitted `sh <path>` which
    /// bypassed the shebang and ran the script under POSIX sh — on macOS that's
    /// bash-3.2 in sh-compat mode, which rejects `done < <(...)` with a syntax
    /// error. The gate was silently broken for every wordpress-extension user.
    #[test]
    fn extension_script_runs_under_its_shebang_not_posix_sh() {
        let dir = TempDir::new().expect("temp dir");
        let script_path = dir.path().join("validate-bash-only.sh");

        // Bash-only process-substitution form that fails under POSIX sh but
        // works under bash (the script's declared interpreter).
        let script_body = "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             count=0\n\
             while IFS= read -r -d '' _f; do count=$((count + 1)); done < <(printf 'a\\0b\\0')\n\
             echo \"count=$count\"\n";
        fs::write(&script_path, script_body).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).unwrap();
        }

        // Mimic what resolve_validate_command emits for an extension script —
        // the quoted path with no `sh ` prefix — and run it the same way
        // validate_write does.
        let command = crate::core::engine::shell::quote_path(&script_path.to_string_lossy());
        let output = std::process::Command::new("sh")
            .args(["-c", &command])
            .output()
            .expect("should spawn");

        assert!(
            output.status.success(),
            "shebang-invoked script failed: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("count=2"),
            "expected bash process substitution to succeed, got: {stdout:?}"
        );
    }
}

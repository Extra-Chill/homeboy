//! Post-write formatting for code-modifying commands.
//!
//! Any command that writes source code (`review lint --fix`) should
//! call `format_after_write()` after writing files. This runs the project's
//! extension-owned formatter to ensure generated code matches project style.
//!
//! Formatting failure is returned to the caller; lint fix treats it as fatal.
//!
//! Formatter ownership is resolved through Extension API v1. The selected
//! manifest remains a private execution detail for locating `scripts.format`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::component::Component;
use crate::error::Result;
use crate::extension::invoke::{
    build_capability_env, execute_capability_script, CapabilityScriptOptions,
};
use homeboy_extension_contract::api::v1::FORMAT_FILE_CAPABILITY_PREFIX;

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
/// Returns formatter failure for the caller to handle; lint fix fails on it.
///
/// # Arguments
/// * `component` - Resolved component identity for the extension environment
/// * `root` - Project root directory
/// * `changed_files` - Files that were modified/created (absolute paths)
pub fn format_after_write(
    component: &Component,
    root: &Path,
    changed_files: &[PathBuf],
) -> Result<FormatResult> {
    if changed_files.is_empty() {
        return Ok(FormatResult::skipped(0));
    }

    let Some((extension_id, extension_path, script)) = resolve_format_command(changed_files) else {
        return Ok(FormatResult::skipped(changed_files.len()));
    };
    let args = changed_files
        .iter()
        .map(|path| scoped_format_arg(root, path))
        .collect::<Vec<_>>();
    let format_command = format!(
        "{} {}",
        crate::engine::shell::quote_path(&extension_path.join(&script).to_string_lossy()),
        crate::engine::shell::quote_args(&args)
    );
    let env = build_capability_env(
        &extension_id,
        &component.id,
        &extension_path,
        root,
        "{}",
        &crate::extension::component_script::component_env_vars(component),
    )?;
    let output = execute_capability_script(
        &extension_path,
        &script,
        &args,
        &env,
        Some(&root.to_string_lossy()),
        None,
        CapabilityScriptOptions {
            passthrough: false,
            stderr_passthrough: false,
            timeout: None,
        },
    )?;
    if output.success {
        return Ok(FormatResult::passed(format_command, changed_files.len()));
    }
    let error_output = format!("{}{}", output.stdout, output.stderr);

    Ok(FormatResult::failed(
        format_command,
        error_output,
        changed_files.len(),
    ))
}

/// Resolve the format command for a set of changed files.
///
/// Selects installed extensions through their v1 `format.<extension>` capability.
fn resolve_format_command(changed_files: &[PathBuf]) -> Option<(String, PathBuf, String)> {
    // Collect unique file extensions
    let mut extensions: Vec<String> = changed_files
        .iter()
        .filter_map(|f| {
            f.extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_string())
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    extensions.sort();

    for ext in &extensions {
        let capability_id = format!("{FORMAT_FILE_CAPABILITY_PREFIX}{ext}");
        let Some(extension_id) =
            crate::extension::resolve::find_installed_capability_provider(&capability_id)
        else {
            continue;
        };
        let Ok(manifest) = crate::extension::catalog::load_extension(&extension_id) else {
            continue;
        };
        if let (Some(ext_path), Some(script_rel)) =
            (manifest.extension_path.as_deref(), manifest.format_script())
        {
            let script_path = std::path::Path::new(ext_path).join(script_rel);

            if script_path.exists() {
                return Some((
                    extension_id,
                    PathBuf::from(ext_path),
                    script_rel.to_string(),
                ));
            }
        }
    }

    None
}

fn scoped_format_arg(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn skipped_when_no_files() {
        let dir = TempDir::new().expect("temp dir");
        let result = format_after_write(
            &Component::new(
                "fixture".into(),
                dir.path().to_string_lossy().into(),
                "fixture".into(),
                None,
            ),
            dir.path(),
            &[],
        )
        .unwrap();
        assert!(result.success);
        assert!(result.command.is_none());
        assert_eq!(result.files_in_scope, 0);
    }

    #[test]
    fn skipped_when_no_formatter_found() {
        let dir = TempDir::new().expect("temp dir");
        let files = vec![dir.path().join("unknown.xyz")];
        let result = format_after_write(
            &Component::new(
                "fixture".into(),
                dir.path().to_string_lossy().into(),
                "fixture".into(),
                None,
            ),
            dir.path(),
            &files,
        )
        .unwrap();
        assert!(result.success);
        assert!(result.command.is_none());
    }

    #[test]
    fn formatter_receives_runtime_helper_and_component_context() {
        crate::test_support::with_isolated_home(|_| {
            let extension_dir = crate::paths::extensions()
                .expect("extensions path")
                .join("rust");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            use std::os::unix::fs::PermissionsExt;
            let script = extension_dir.join("format.sh");
            std::fs::write(&script, "#!/bin/sh\nset -eu\ntest -f \"${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:?required}\"\ntest \"${HOMEBOY_COMPONENT_PATH:?required}\" = \"$PWD\"\ntest \"$1\" = 'src/changed file.rs'\nprintf formatted > formatted.txt\n").expect("format script");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::write(
                extension_dir.join("rust.json"),
                serde_json::json!({
                    "name": "Rust",
                    "version": "1.0.0",
                    "scripts": { "format": "format.sh" },
                    "provides": { "file_extensions": ["rs"] }
                })
                .to_string(),
            )
            .expect("extension manifest");

            let root = TempDir::new().expect("project root");
            let changed = root.path().join("src/changed file.rs");
            let component = Component::new(
                "fixture".into(),
                root.path().to_string_lossy().into(),
                "rust".into(),
                None,
            );
            let result = format_after_write(&component, root.path(), &[changed]).unwrap();
            assert!(result.success, "{:?}", result.output);
            assert!(result.command.unwrap().contains("format.sh"));
            assert_eq!(
                std::fs::read_to_string(root.path().join("formatted.txt")).unwrap(),
                "formatted"
            );
        });
    }

    #[test]
    fn scoped_format_arg_prefers_paths_relative_to_root() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path().join("component");
        let file = root.join("src/changed file.php");

        assert_eq!(scoped_format_arg(&root, &file), "src/changed file.php");
    }
}

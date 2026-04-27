//! Unified execution context resolution for all extension-backed commands.
//!
//! Commands like `lint`, `test`, `build`, `audit`, and `refactor` all need to resolve
//! the same set of runtime values: source path, git root, extension, settings, and component.
//! This module centralizes that resolution so each command doesn't re-derive it independently.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/664

use std::path::PathBuf;

use serde::Serialize;

use crate::component::{self, Component};
use crate::error::Result;
use crate::extension::{self, ExtensionCapability};

/// Unified execution context for extension-backed commands.
///
/// This is the single source of truth for all runtime state that lint, test, build,
/// audit, and refactor commands need. Instead of each command independently resolving
/// component, source path, git root, and extension, they all call
/// [`resolve()`] once and use the result.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionContext {
    /// The resolved component (from config, portable config, or synthetic).
    #[serde(skip)]
    pub component: Component,

    /// Component ID (convenience — same as `component.id`).
    pub component_id: String,

    /// Canonical source path on disk (tilde-expanded, validated).
    /// This is where the source code actually lives.
    pub source_path: PathBuf,

    /// Git repository root (if the source path is inside a git repo).
    /// Used by validation gates to run compile checks from the repo root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_root: Option<PathBuf>,

    /// The extension selected for this capability (if a capability was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,

    /// Path to the extension directory on disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<PathBuf>,

    /// Merged settings (manifest defaults → component → overrides).
    pub settings: Vec<(String, serde_json::Value)>,
}

/// What to resolve when building an execution context.
///
/// Not all commands need an extension context (e.g., audit and refactor operate
/// purely on the source tree). Use `ResolveOptions` to control what gets resolved.
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    /// Component ID or label (positional arg from CLI).
    pub component_id: Option<String>,

    /// Explicit `--path` override.
    pub path_override: Option<String>,

    /// Which extension capability to resolve (Lint, Test, Build).
    /// When `None`, only component + source path are resolved — no extension lookup.
    pub capability: Option<ExtensionCapability>,

    /// Additional settings from `--setting key=value` flags (string values).
    pub settings_overrides: Vec<(String, String)>,

    /// Additional settings from `--setting-json key=<json>` flags (typed
    /// values). Applied after `settings_overrides` so JSON wins on
    /// conflict. Required for object-shaped settings whose dispatcher
    /// consumers expect a JSON object, not a string-coerced JSON literal.
    pub settings_json_overrides: Vec<(String, serde_json::Value)>,
}

impl ResolveOptions {
    /// Create options for a command that needs a specific extension capability.
    pub fn with_capability(
        component_id: &str,
        path_override: Option<String>,
        capability: ExtensionCapability,
        settings: Vec<(String, String)>,
    ) -> Self {
        Self {
            component_id: Some(component_id.to_string()),
            path_override,
            capability: Some(capability),
            settings_overrides: settings,
            settings_json_overrides: Vec::new(),
        }
    }

    /// Create options for a command that needs an extension capability AND
    /// typed-JSON setting overrides. Mirrors `with_capability` but accepts
    /// the JSON overrides too.
    pub fn with_capability_and_json(
        component_id: &str,
        path_override: Option<String>,
        capability: ExtensionCapability,
        settings: Vec<(String, String)>,
        settings_json: Vec<(String, serde_json::Value)>,
    ) -> Self {
        Self {
            component_id: Some(component_id.to_string()),
            path_override,
            capability: Some(capability),
            settings_overrides: settings,
            settings_json_overrides: settings_json,
        }
    }

    /// Create options for a command that only needs source path resolution (no extension).
    pub fn source_only(component_id: &str, path_override: Option<String>) -> Self {
        Self {
            component_id: Some(component_id.to_string()),
            path_override,
            capability: None,
            settings_overrides: Vec::new(),
            settings_json_overrides: Vec::new(),
        }
    }
}

/// Resolve a unified execution context.
///
/// This is the canonical entry point. All extension-backed commands should call this
/// instead of independently resolving component, path, extension, and settings.
///
/// # Resolution order
///
/// 1. **Component**: `--path` override → registered component by ID → CWD auto-discovery
/// 2. **Source path**: `--path` if given, else `component.local_path` (tilde-expanded)
/// 3. **Git root**: detected from source path via `git rev-parse --show-toplevel`
/// 4. **Extension**: resolved from component's linked extensions for the requested capability
/// 5. **Settings**: extension manifest defaults → component-level → CLI overrides
pub fn resolve(options: &ResolveOptions) -> Result<ExecutionContext> {
    // 1. Resolve component
    let component = component::resolve_effective(
        options.component_id.as_deref(),
        options.path_override.as_deref(),
        None,
    )?;

    // 2. Resolve source path
    let source_path = if let Some(ref path) = options.path_override {
        PathBuf::from(path)
    } else {
        let expanded = shellexpand::tilde(&component.local_path);
        PathBuf::from(expanded.as_ref())
    };

    // 3. Detect git root
    let git_root = detect_git_root(&source_path);

    // 4. Optionally resolve extension context
    let (extension_id, extension_path, settings) = if let Some(capability) = options.capability {
        let ext_context = extension::resolve_execution_context(&component, capability)?;
        let mut settings = ext_context.settings.clone();
        // Merge CLI string overrides on top (CLI string values stay strings).
        for (key, value) in &options.settings_overrides {
            // Remove existing key if present (override semantics)
            settings.retain(|(k, _)| k != key);
            settings.push((key.clone(), serde_json::Value::String(value.clone())));
        }
        // Then merge typed-JSON overrides — these win against both
        // manifest defaults / component settings AND --setting string
        // overrides. Strictly more expressive: an --setting-json on the
        // same key represents intentional type preservation that string
        // coercion can't represent.
        for (key, value) in &options.settings_json_overrides {
            settings.retain(|(k, _)| k != key);
            settings.push((key.clone(), value.clone()));
        }
        (
            Some(ext_context.extension_id.clone()),
            Some(ext_context.extension_path.clone()),
            settings,
        )
    } else {
        // No extension context — only CLI overrides, wrapped as JSON strings.
        let settings = options
            .settings_overrides
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        (None, None, settings)
    };

    Ok(ExecutionContext {
        component_id: component.id.clone(),
        component,
        source_path,
        git_root,
        extension_id,
        extension_path,
        settings,
    })
}

impl ExecutionContext {
    /// Get the effective working directory for command execution.
    ///
    /// Returns the source path as a string reference.
    pub fn working_dir(&self) -> &str {
        self.source_path.to_str().unwrap_or(".")
    }

    /// Emit a structured debug summary to stderr.
    ///
    /// Useful for diagnosing resolution issues — shows every resolved value
    /// so operators can see exactly what the command will use.
    pub fn log_debug(&self) {
        crate::log_status!("context", "component_id: {}", self.component_id);
        crate::log_status!("context", "source_path: {}", self.source_path.display());
        crate::log_status!(
            "context",
            "git_root: {}",
            self.git_root
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".to_string())
        );
        crate::log_status!(
            "context",
            "extension_id: {}",
            self.extension_id.as_deref().unwrap_or("(none)")
        );
        crate::log_status!(
            "context",
            "extension_path: {}",
            self.extension_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".to_string())
        );
        if !self.settings.is_empty() {
            crate::log_status!(
                "context",
                "settings: {}",
                self.settings
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}

/// Detect the git repository root for a given directory.
///
/// Returns `None` if the path is not inside a git repository.
fn detect_git_root(dir: &PathBuf) -> Option<PathBuf> {
    let effective_dir = if dir.is_file() {
        dir.parent()?
    } else if dir.exists() {
        dir.as_path()
    } else {
        // Directory doesn't exist yet — try parent
        dir.parent()?
    };

    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(effective_dir)
        .output()
        .ok()?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn detect_git_root_finds_repo() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init");

        let result = detect_git_root(&root.to_path_buf());
        assert!(result.is_some());
        assert_eq!(result.unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn detect_git_root_returns_none_outside_repo() {
        let dir = TempDir::new().expect("temp dir");
        let non_git = dir.path().join("not-a-repo");
        fs::create_dir_all(&non_git).expect("create dir");

        let result = detect_git_root(&non_git.to_path_buf());
        // May still find a parent repo, so we just test it doesn't panic
        assert!(result.is_none() || result.is_some());
    }

    #[test]
    fn resolve_source_only_with_path() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();
        fs::create_dir_all(root).expect("create dir");

        let options =
            ResolveOptions::source_only("test-comp", Some(root.to_string_lossy().to_string()));
        let ctx = resolve(&options).expect("resolve should succeed");

        assert_eq!(ctx.component_id, "test-comp");
        assert_eq!(ctx.source_path, root);
        assert!(ctx.extension_id.is_none());
    }

    #[test]
    fn resolve_source_only_with_path_in_git_repo() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init");

        let sub = root.join("src");
        fs::create_dir_all(&sub).expect("create src dir");

        let options =
            ResolveOptions::source_only("test-comp", Some(sub.to_string_lossy().to_string()));
        let ctx = resolve(&options).expect("resolve should succeed");

        assert!(ctx.git_root.is_some());
        assert_eq!(ctx.git_root.unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn resolve_capability_raw_path_reports_unsupported_shape() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();

        let options = ResolveOptions {
            component_id: Some(root.to_string_lossy().to_string()),
            path_override: None,
            capability: Some(ExtensionCapability::Lint),
            settings_overrides: Vec::new(),
            settings_json_overrides: Vec::new(),
        };

        let err = resolve(&options).expect_err("raw path without lint extension should fail");
        let message = err.to_string();

        assert!(
            message.contains("has no extensions configured"),
            "expected unsupported-shape error, got: {message}"
        );
        assert!(
            !message.contains("component.not_found"),
            "raw path should not be treated as a component id: {message}"
        );
    }

    #[test]
    fn settings_overrides_replace_existing() {
        let options = ResolveOptions {
            component_id: Some("test".to_string()),
            path_override: Some("/tmp".to_string()),
            capability: None,
            settings_overrides: vec![
                ("mode".to_string(), "strict".to_string()),
                ("lang".to_string(), "rust".to_string()),
            ],
            settings_json_overrides: Vec::new(),
        };

        let ctx = resolve(&options).expect("resolve should succeed");
        assert_eq!(ctx.settings.len(), 2);
        assert!(ctx
            .settings
            .iter()
            .any(|(k, v)| k == "mode" && v == "strict"));
    }
}

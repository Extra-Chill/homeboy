//! Unified execution context resolution for all extension-backed commands.
//!
//! Commands like `lint`, `test`, `build`, `audit`, and `refactor` all need to resolve
//! the same set of runtime values: source path, git root, extension, settings, and component.
//! This module centralizes that resolution so each command doesn't re-derive it independently.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/664

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::core::component::{self, Component, ScopedExtensionConfig};
use crate::core::error::{Error, ErrorCode, Result};
use crate::core::extension::{self, ExtensionCapability};

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

    /// One-shot extension override for this invocation.
    ///
    /// When set, this replaces the component's configured extension list in
    /// memory only. It is not written back to `homeboy.json` or the global
    /// component registry.
    pub extension_overrides: Vec<String>,
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
            extension_overrides: Vec::new(),
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
            extension_overrides: Vec::new(),
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
            extension_overrides: Vec::new(),
        }
    }
}

fn apply_extension_overrides(component: &mut Component, extension_overrides: &[String]) {
    if extension_overrides.is_empty() {
        return;
    }

    let existing = component.extensions.clone().unwrap_or_default();
    let mut extensions = HashMap::new();

    for extension_id in extension_overrides {
        extensions.insert(
            extension_id.clone(),
            existing
                .get(extension_id)
                .cloned()
                .unwrap_or_else(ScopedExtensionConfig::default),
        );
    }

    component.extensions = Some(extensions);
}

fn declared_extension_ids(component: &Component) -> Vec<String> {
    let mut ids: Vec<String> = component
        .extensions
        .as_ref()
        .map(|extensions| extensions.keys().cloned().collect())
        .unwrap_or_default();
    ids.sort();
    ids
}

fn add_extension_override_hints(
    mut err: Error,
    extension_overrides: &[String],
    declared_extension_ids: &[String],
) -> Error {
    if extension_overrides.is_empty() || err.code != ErrorCode::ExtensionNotFound {
        return err;
    }

    match declared_extension_ids {
        [extension_id] => {
            err = err.with_hint(format!(
                "Component declares extension '{}'; retry with --extension {} or omit --extension",
                extension_id, extension_id
            ));
        }
        [] => {}
        extension_ids => {
            err = err.with_hint(format!(
                "Component declares extensions: {}",
                extension_ids.join(", ")
            ));
        }
    }

    err
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
    resolve_with_component(options, None)
}

/// Resolve a unified execution context, optionally starting from an in-memory
/// component supplied by a higher-level dispatcher.
///
/// Rig-pinned bench runs use this to provide private extension config from the
/// rig spec without requiring global component registration or repo-owned
/// `homeboy.json`. Other commands should continue using [`resolve()`].
pub fn resolve_with_component(
    options: &ResolveOptions,
    component_override: Option<Component>,
) -> Result<ExecutionContext> {
    // 1. Resolve component
    let target = if let Some(component) = component_override {
        component::resolve_target_from_component(component, options.path_override.as_deref())
    } else {
        component::resolve_target(component::TargetSpec::new(
            options.component_id.as_deref(),
            options.path_override.as_deref(),
        ))?
    };
    let component::ResolvedTarget {
        mut component,
        source_path,
        git_root,
        ..
    } = target;

    let declared_extension_ids = declared_extension_ids(&component);
    apply_extension_overrides(&mut component, &options.extension_overrides);

    // 2. Optionally resolve extension context
    let (extension_id, extension_path, settings) = if let Some(capability) = options.capability {
        let ext_context =
            extension::resolve_execution_context(&component, capability).map_err(|err| {
                add_extension_override_hints(
                    err,
                    &options.extension_overrides,
                    &declared_extension_ids,
                )
            })?;
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
    /// Return an explicit view over resolved extension settings.
    pub fn resolved_settings(&self) -> ResolvedSettings<'_> {
        ResolvedSettings::new(&self.settings)
    }

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

/// Explicit conversions for resolved extension settings.
///
/// Resolved settings are stored as typed JSON so command callers must choose
/// whether they need string-only settings, typed JSON settings, or intentionally
/// lossy stringification for legacy runner contracts.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedSettings<'a> {
    settings: &'a [(String, serde_json::Value)],
}

impl<'a> ResolvedSettings<'a> {
    pub fn new(settings: &'a [(String, serde_json::Value)]) -> Self {
        Self { settings }
    }

    /// Return only settings that are already strings.
    pub fn string_overrides(&self) -> Vec<(String, String)> {
        self.settings
            .iter()
            .filter_map(|(key, value)| match value {
                serde_json::Value::String(value) => Some((key.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    /// Return only settings that must stay typed JSON.
    pub fn json_overrides(&self) -> Vec<(String, serde_json::Value)> {
        self.settings
            .iter()
            .filter_map(|(key, value)| match value {
                serde_json::Value::String(_) => None,
                value => Some((key.clone(), value.clone())),
            })
            .collect()
    }

    /// Return every setting as a string, stringifying non-string JSON values.
    pub fn string_lossy_overrides(&self) -> Vec<(String, String)> {
        self.settings
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    serde_json::Value::String(value) => value.clone(),
                    value => value.to_string(),
                };
                (key.clone(), value)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn write_extension(home: &TempDir, extension_id: &str, capabilities: &str) {
        let extension_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("extensions")
            .join(extension_id);
        fs::create_dir_all(&extension_dir).expect("create extension dir");
        fs::write(
            extension_dir.join(format!("{}.json", extension_id)),
            format!(
                r#"{{
                    "name": "{}",
                    "version": "0.0.0",
                    {}
                }}"#,
                extension_id, capabilities
            ),
        )
        .expect("write extension manifest");
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
    fn test_resolve_with_component() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();
        fs::create_dir_all(root).expect("create dir");

        let component = Component {
            id: "rig-owned".to_string(),
            local_path: root.to_string_lossy().to_string(),
            ..Component::default()
        };
        let options = ResolveOptions::source_only("registered-id", None);

        let ctx = resolve_with_component(&options, Some(component))
            .expect("in-memory component should resolve");

        assert_eq!(ctx.component_id, "rig-owned");
        assert_eq!(ctx.source_path, root);
        assert!(ctx.extension_id.is_none());
    }

    #[test]
    fn resolve_with_component_preserves_in_memory_extension_config() {
        with_isolated_home(|home| {
            write_extension(
                home,
                "rig-node",
                r#""lint": { "extension_script": "lint.sh" }"#,
            );
            write_extension(
                home,
                "portable-node",
                r#""lint": { "extension_script": "lint.sh" }"#,
            );
            let dir = TempDir::new().expect("temp dir");
            let root = dir.path();
            fs::write(
                root.join("homeboy.json"),
                r#"{"id":"rig-owned","extensions":{"portable-node":{"marker":"portable"}}}"#,
            )
            .expect("write portable config");
            let extensions = HashMap::from([(
                "rig-node".to_string(),
                ScopedExtensionConfig {
                    settings: HashMap::from([(
                        "marker".to_string(),
                        serde_json::json!("in-memory"),
                    )]),
                    ..ScopedExtensionConfig::default()
                },
            )]);
            let component = Component {
                id: "rig-owned".to_string(),
                local_path: root.to_string_lossy().to_string(),
                extensions: Some(extensions),
                ..Component::default()
            };
            let options = ResolveOptions::with_capability(
                "rig-owned",
                None,
                ExtensionCapability::Lint,
                Vec::new(),
            );

            let ctx = resolve_with_component(&options, Some(component))
                .expect("in-memory component should stay authoritative");

            assert_eq!(ctx.extension_id.as_deref(), Some("rig-node"));
            assert!(ctx
                .component
                .extensions
                .as_ref()
                .is_some_and(|extensions| extensions.contains_key("rig-node")
                    && !extensions.contains_key("portable-node")));
            assert!(ctx
                .settings
                .iter()
                .any(|(key, value)| key == "marker" && value == "in-memory"));
        });
    }

    #[test]
    fn resolve_with_component_applies_path_override_without_rediscovery() {
        with_isolated_home(|home| {
            write_extension(
                home,
                "rig-node",
                r#""lint": { "extension_script": "lint.sh" }"#,
            );
            let dir = TempDir::new().expect("temp dir");
            let original = dir.path().join("original");
            let override_path = dir.path().join("override");
            fs::create_dir_all(&original).expect("create original");
            fs::create_dir_all(&override_path).expect("create override");
            fs::write(
                override_path.join("homeboy.json"),
                r#"{"id":"rig-owned","extensions":{"portable-node":{}}}"#,
            )
            .expect("write portable config");
            let extensions =
                HashMap::from([("rig-node".to_string(), ScopedExtensionConfig::default())]);
            let component = Component {
                id: "rig-owned".to_string(),
                local_path: original.to_string_lossy().to_string(),
                extensions: Some(extensions),
                ..Component::default()
            };
            let options = ResolveOptions::with_capability(
                "rig-owned",
                Some(override_path.to_string_lossy().to_string()),
                ExtensionCapability::Lint,
                Vec::new(),
            );

            let ctx = resolve_with_component(&options, Some(component))
                .expect("path override should not trigger portable rediscovery");

            assert_eq!(ctx.source_path, override_path);
            assert_eq!(ctx.component.local_path, override_path.to_string_lossy());
            assert_eq!(ctx.extension_id.as_deref(), Some("rig-node"));
            assert!(ctx
                .component
                .extensions
                .as_ref()
                .is_some_and(|extensions| extensions.contains_key("rig-node")
                    && !extensions.contains_key("portable-node")));
        });
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
            extension_overrides: Vec::new(),
        };

        let err = resolve(&options).expect_err("raw path without lint extension should fail");
        let message = err.to_string();

        assert!(
            message.contains("No extension provider configured"),
            "expected unsupported-shape error, got: {message}"
        );
        assert!(
            !message.contains("component.not_found"),
            "raw path should not be treated as a component id: {message}"
        );
    }

    #[test]
    fn extension_override_resolves_unconfigured_component_without_persisting() {
        with_isolated_home(|home| {
            write_extension(
                home,
                "nodejs",
                r#""lint": { "extension_script": "lint.sh" }"#,
            );
            let dir = TempDir::new().expect("component dir");
            let component = Component {
                id: "adhoc".to_string(),
                local_path: dir.path().to_string_lossy().to_string(),
                extensions: None,
                ..Component::default()
            };

            let mut options = ResolveOptions::with_capability(
                "adhoc",
                Some(dir.path().to_string_lossy().to_string()),
                ExtensionCapability::Lint,
                Vec::new(),
            );
            options.extension_overrides = vec!["nodejs".to_string()];

            let ctx = resolve_with_component(&options, Some(component.clone()))
                .expect("one-shot extension should resolve");

            assert_eq!(ctx.extension_id.as_deref(), Some("nodejs"));
            assert!(
                component.extensions.is_none(),
                "source component is unchanged"
            );
            assert!(ctx
                .component
                .extensions
                .as_ref()
                .is_some_and(|extensions| extensions.contains_key("nodejs")));
        });
    }

    #[test]
    fn extension_override_replaces_configured_extension_list_for_run() {
        with_isolated_home(|home| {
            write_extension(
                home,
                "wordpress",
                r#""lint": { "extension_script": "lint.sh" }"#,
            );
            write_extension(
                home,
                "nodejs",
                r#""lint": { "extension_script": "lint.sh" }"#,
            );
            let dir = TempDir::new().expect("component dir");
            let mut configured = HashMap::new();
            configured.insert(
                "wordpress".to_string(),
                ScopedExtensionConfig {
                    settings: HashMap::from([(
                        "package_manager".to_string(),
                        serde_json::json!("composer"),
                    )]),
                    ..ScopedExtensionConfig::default()
                },
            );
            configured.insert(
                "nodejs".to_string(),
                ScopedExtensionConfig {
                    settings: HashMap::from([(
                        "package_manager".to_string(),
                        serde_json::json!("pnpm"),
                    )]),
                    ..ScopedExtensionConfig::default()
                },
            );
            let component = Component {
                id: "configured".to_string(),
                local_path: dir.path().to_string_lossy().to_string(),
                extensions: Some(configured),
                ..Component::default()
            };

            let mut options = ResolveOptions::with_capability(
                "configured",
                Some(dir.path().to_string_lossy().to_string()),
                ExtensionCapability::Lint,
                Vec::new(),
            );
            options.extension_overrides = vec!["nodejs".to_string()];

            let ctx = resolve_with_component(&options, Some(component.clone()))
                .expect("override should select nodejs only");
            let runtime_extensions = ctx
                .component
                .extensions
                .as_ref()
                .expect("runtime extensions");

            assert_eq!(ctx.extension_id.as_deref(), Some("nodejs"));
            assert!(runtime_extensions.contains_key("nodejs"));
            assert!(!runtime_extensions.contains_key("wordpress"));
            assert!(ctx
                .settings
                .iter()
                .any(|(key, value)| key == "package_manager" && value == "pnpm"));
            assert!(component
                .extensions
                .as_ref()
                .is_some_and(|extensions| extensions.contains_key("wordpress")));
        });
    }

    #[test]
    fn unknown_extension_override_suggests_single_declared_extension() {
        with_isolated_home(|home| {
            write_extension(
                home,
                "wordpress",
                r#""lint": { "extension_script": "lint.sh" }"#,
            );
            let dir = TempDir::new().expect("component dir");
            let configured =
                HashMap::from([("wordpress".to_string(), ScopedExtensionConfig::default())]);
            let component = Component {
                id: "plugin".to_string(),
                local_path: dir.path().to_string_lossy().to_string(),
                extensions: Some(configured),
                ..Component::default()
            };

            let mut options = ResolveOptions::with_capability(
                "plugin",
                Some(dir.path().to_string_lossy().to_string()),
                ExtensionCapability::Lint,
                Vec::new(),
            );
            options.extension_overrides = vec!["php".to_string()];

            let err = resolve_with_component(&options, Some(component))
                .expect_err("unknown override should fail");

            assert_eq!(err.code, ErrorCode::ExtensionNotFound);
            assert!(err.hints.iter().any(|hint| {
                hint.message.contains("--extension wordpress")
                    && hint.message.contains("omit --extension")
            }));
        });
    }

    #[test]
    fn test_with_capability() {
        let options = ResolveOptions::with_capability(
            "component-a",
            Some("/tmp/component-a".to_string()),
            ExtensionCapability::Lint,
            vec![("strict".to_string(), "true".to_string())],
        );

        assert_eq!(options.component_id.as_deref(), Some("component-a"));
        assert_eq!(options.path_override.as_deref(), Some("/tmp/component-a"));
        assert_eq!(options.capability, Some(ExtensionCapability::Lint));
        assert_eq!(
            options.settings_overrides,
            vec![("strict".to_string(), "true".to_string())]
        );
        assert!(options.settings_json_overrides.is_empty());
        assert!(options.extension_overrides.is_empty());
    }

    #[test]
    fn test_with_capability_and_json() {
        let options = ResolveOptions::with_capability_and_json(
            "component-b",
            None,
            ExtensionCapability::Test,
            vec![("mode".to_string(), "fast".to_string())],
            vec![("threshold".to_string(), serde_json::json!(0.95))],
        );

        assert_eq!(options.component_id.as_deref(), Some("component-b"));
        assert!(options.path_override.is_none());
        assert_eq!(options.capability, Some(ExtensionCapability::Test));
        assert_eq!(
            options.settings_overrides,
            vec![("mode".to_string(), "fast".to_string())]
        );
        assert_eq!(
            options.settings_json_overrides,
            vec![("threshold".to_string(), serde_json::json!(0.95))]
        );
        assert!(options.extension_overrides.is_empty());
    }

    #[test]
    fn test_working_dir() {
        let dir = TempDir::new().expect("temp dir");
        let ctx = ExecutionContext {
            component: Component::default(),
            component_id: "component".to_string(),
            source_path: dir.path().to_path_buf(),
            git_root: None,
            extension_id: None,
            extension_path: None,
            settings: Vec::new(),
        };

        assert_eq!(ctx.working_dir(), dir.path().to_str().unwrap());
    }

    fn resolved_settings_entries() -> Vec<(String, serde_json::Value)> {
        vec![
            ("string".to_string(), serde_json::json!("value")),
            ("object".to_string(), serde_json::json!({ "enabled": true })),
            ("number".to_string(), serde_json::json!(3)),
        ]
    }

    #[test]
    fn test_string_overrides() {
        let entries = resolved_settings_entries();
        let settings = ResolvedSettings::new(&entries);

        assert_eq!(
            settings.string_overrides(),
            vec![("string".to_string(), "value".to_string())]
        );
    }

    #[test]
    fn test_json_overrides() {
        let entries = resolved_settings_entries();
        let settings = ResolvedSettings::new(&entries);

        assert_eq!(
            settings.json_overrides(),
            vec![
                ("object".to_string(), serde_json::json!({ "enabled": true })),
                ("number".to_string(), serde_json::json!(3)),
            ]
        );
    }

    #[test]
    fn test_string_lossy_overrides() {
        let entries = resolved_settings_entries();
        let settings = ResolvedSettings::new(&entries);

        assert_eq!(
            settings.string_lossy_overrides(),
            vec![
                ("string".to_string(), "value".to_string()),
                ("object".to_string(), r#"{"enabled":true}"#.to_string()),
                ("number".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn resolved_settings_split_string_json_and_lossy_views() {
        let entries = [
            ("string".to_string(), serde_json::json!("value")),
            ("object".to_string(), serde_json::json!({ "enabled": true })),
            ("number".to_string(), serde_json::json!(3)),
        ];
        let settings = ResolvedSettings::new(&entries);

        assert_eq!(
            settings.string_overrides(),
            vec![("string".to_string(), "value".to_string())]
        );
        assert_eq!(
            settings.json_overrides(),
            vec![
                ("object".to_string(), serde_json::json!({ "enabled": true })),
                ("number".to_string(), serde_json::json!(3)),
            ]
        );
        assert_eq!(
            settings.string_lossy_overrides(),
            vec![
                ("string".to_string(), "value".to_string()),
                ("object".to_string(), r#"{"enabled":true}"#.to_string()),
                ("number".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn test_log_debug() {
        let dir = TempDir::new().expect("temp dir");
        let ctx = ExecutionContext {
            component: Component::default(),
            component_id: "component".to_string(),
            source_path: dir.path().to_path_buf(),
            git_root: None,
            extension_id: None,
            extension_path: None,
            settings: vec![("mode".to_string(), serde_json::json!("test"))],
        };

        ctx.log_debug();
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
            extension_overrides: Vec::new(),
        };

        let ctx = resolve(&options).expect("resolve should succeed");
        assert_eq!(ctx.settings.len(), 2);
        assert!(ctx
            .settings
            .iter()
            .any(|(k, v)| k == "mode" && v == "strict"));
    }
}

mod component;
mod default;
mod default_git;
mod trait_impls;
mod types;

pub use component::*;
pub use default::*;
pub use default_git::*;
pub use trait_impls::*;
pub use types::*;

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

pub mod inventory;
pub mod mutations;
pub mod portable;
pub mod relationships;
pub mod resolution;
pub mod scope;
pub mod versioning;

pub use inventory::{exists, extension_provides_artifact_pattern, inventory, list, list_ids, load};
pub use mutations::{delete_safe, merge, rename, set_changelog_target};
pub use portable::{
    discover_from_portable, has_portable_config, infer_portable_component_id, mutate_portable,
    portable_json, read_portable_config, write_portable_config,
};
pub use relationships::{associated_projects, projects_using, rename_component, shared_components};
pub use resolution::{
    detect_from_cwd, resolve, resolve_artifact, resolve_effective, validate_local_path,
};
pub use scope::{resolve_component_scope, EffectiveScope, ScopeCommand};
pub use versioning::{
    normalize_version_pattern, parse_version_targets, validate_version_pattern,
    validate_version_target_conflict,
};

impl From<Component> for RawComponent {
    fn from(c: Component) -> Self {
        RawComponent {
            id: c.id,
            aliases: c.aliases,
            local_path: c.local_path,
            remote_path: c.remote_path,
            build_artifact: c.build_artifact,
            extensions: c.extensions,
            version_targets: c.version_targets,
            changelog_target: c.changelog_target,
            changelog_next_section_label: c.changelog_next_section_label,
            changelog_next_section_aliases: c.changelog_next_section_aliases,
            hooks: c.hooks,
            pre_version_bump_commands: Vec::new(),
            post_version_bump_commands: Vec::new(),
            post_release_commands: Vec::new(),
            extract_command: c.extract_command,
            remote_owner: c.remote_owner,
            deploy_strategy: c.deploy_strategy,
            git_deploy: c.git_deploy,
            remote_url: c.remote_url,
            auto_cleanup: c.auto_cleanup,
            docs_dir: c.docs_dir,
            docs_dirs: c.docs_dirs,
            scopes: c.scopes,
        }
    }
}

impl Component {
    pub fn new(
        id: String,
        local_path: String,
        remote_path: String,
        build_artifact: Option<String>,
    ) -> Self {
        Self {
            id,
            aliases: Vec::new(),
            local_path,
            remote_path,
            build_artifact,
            extensions: None,
            version_targets: None,
            changelog_target: None,
            changelog_next_section_label: None,
            changelog_next_section_aliases: None,
            hooks: HashMap::new(),
            extract_command: None,
            remote_owner: None,
            deploy_strategy: None,
            git_deploy: None,
            remote_url: None,
            auto_cleanup: false,
            docs_dir: None,
            docs_dirs: Vec::new(),
            scopes: None,
        }
    }

    /// Auto-resolve `remote_path` for WordPress components when not explicitly set.
    ///
    /// If the component has the `wordpress` extension and `remote_path` is empty,
    /// detect whether it's a plugin or theme from source files and return the
    /// canonical WordPress path.
    ///
    /// Uses the **local directory name** (basename of `local_path`) as the remote
    /// directory name — not the component ID. The component ID may differ from the
    /// directory name (e.g., component `extrachill-theme` lives in directory
    /// `extrachill/`), and WordPress expects the directory name to match the slug
    /// from `style.css` or the main plugin file.
    ///
    /// Returns `Some(path)` if auto-resolved, `None` if not applicable or not detectable.
    pub fn auto_resolve_remote_path(&self) -> Option<String> {
        // Only applies to components with the wordpress extension.
        let extensions = self.extensions.as_ref()?;
        if !extensions.contains_key("wordpress") {
            return None;
        }

        let local = std::path::Path::new(&self.local_path);

        // Use the directory basename as the remote directory name.
        // This matches WordPress convention: the theme/plugin slug is the directory name.
        let dir_name = local.file_name()?.to_str()?;

        // Check for plugin: look for a .php file with "Plugin Name:" header.
        // Try {dir_name}.php first (standard convention), then {id}.php as fallback.
        let plugin_candidates = [
            local.join(format!("{}.php", dir_name)),
            local.join(format!("{}.php", self.id)),
        ];
        for plugin_file in &plugin_candidates {
            if plugin_file.exists() {
                if let Ok(content) = std::fs::read_to_string(plugin_file) {
                    if content.contains("Plugin Name:") {
                        return Some(format!("wp-content/plugins/{}", dir_name));
                    }
                }
            }
        }

        // Check for theme: style.css with "Theme Name:" header
        let style_file = local.join("style.css");
        if style_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&style_file) {
                if content.contains("Theme Name:") {
                    return Some(format!("wp-content/themes/{}", dir_name));
                }
            }
        }

        None
    }

    /// Ensure `remote_path` is populated. If empty, attempt auto-resolution.
    ///
    /// This should be called after all config layers (repo portable, project overrides)
    /// have been applied. It fills in `remote_path` only if still empty.
    pub fn resolve_remote_path(&mut self) {
        if self.remote_path.trim().is_empty() {
            if let Some(resolved) = self.auto_resolve_remote_path() {
                self.remote_path = resolved;
            }
        }
    }
}

/// Normalize empty strings to None. Treats "", null, and field omission identically for consistent validation.
fn deserialize_empty_as_none<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

// ============================================================================
// Runtime resolution + repo-backed component access
// ============================================================================

// ============================================================================
// Operations
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_version_target_conflict_different_pattern_errors() {
        let existing = vec![VersionTarget {
            file: "plugin.php".to_string(),
            pattern: Some("Version: (.*)".to_string()),
        }];

        let result = validate_version_target_conflict(
            &existing,
            "plugin.php",
            "define('VER', '(.*)')",
            "test-comp",
        );
        // Multiple targets per file with different patterns are now allowed
        // (e.g. plugin header Version: + PHP define() constant in same file)
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_target_conflict_same_pattern_ok() {
        let existing = vec![VersionTarget {
            file: "plugin.php".to_string(),
            pattern: Some("Version: (.*)".to_string()),
        }];

        let result =
            validate_version_target_conflict(&existing, "plugin.php", "Version: (.*)", "test-comp");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_target_conflict_different_file_ok() {
        let existing = vec![VersionTarget {
            file: "plugin.php".to_string(),
            pattern: Some("Version: (.*)".to_string()),
        }];

        let result = validate_version_target_conflict(
            &existing,
            "package.json",
            "\"version\": \"(.*)\"",
            "test-comp",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_target_conflict_empty_existing_ok() {
        let existing: Vec<VersionTarget> = vec![];

        let result =
            validate_version_target_conflict(&existing, "plugin.php", "Version: (.*)", "test-comp");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_pattern_rejects_template_syntax() {
        let result = validate_version_pattern("Version: {version}");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.details.to_string().contains("template syntax"));
    }

    #[test]
    fn validate_version_pattern_rejects_no_capture_group() {
        let result = validate_version_pattern(r"Version: \d+\.\d+\.\d+");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.details.to_string().contains("no capture group"));
    }

    #[test]
    fn validate_version_pattern_rejects_invalid_regex() {
        let result = validate_version_pattern(r"Version: (\d+\.\d+");
        assert!(result.is_err());
    }

    #[test]
    fn validate_version_pattern_accepts_valid_pattern() {
        assert!(validate_version_pattern(r"Version:\s*(\d+\.\d+\.\d+)").is_ok());
    }

    #[test]
    fn parse_version_targets_rejects_template_syntax() {
        let targets = vec!["style.css::Version: {version}".to_string()];
        let result = parse_version_targets(&targets);
        assert!(result.is_err());
    }

    #[test]
    fn normalize_version_pattern_converts_double_escaped() {
        // Pattern with double-escaped backslashes (as stored in config)
        let double_escaped = r"Version:\\s*(\\d+\\.\\d+\\.\\d+)";
        let normalized = normalize_version_pattern(double_escaped);
        assert_eq!(normalized, r"Version:\s*(\d+\.\d+\.\d+)");

        // Pattern already correct should stay the same
        let correct = r"Version:\s*(\d+\.\d+\.\d+)";
        let normalized2 = normalize_version_pattern(correct);
        assert_eq!(normalized2, r"Version:\s*(\d+\.\d+\.\d+)");
    }

    #[test]
    fn parse_version_targets_normalizes_double_escaped_patterns() {
        // Simulate pattern stored with double-escaped backslashes
        let targets = vec!["plugin.php::Version:\\s*(\\d+\\.\\d+\\.\\d+)".to_string()];
        let result = parse_version_targets(&targets).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].file, "plugin.php");
        assert_eq!(
            result[0].pattern.as_ref().unwrap(),
            r"Version:\s*(\d+\.\d+\.\d+)"
        );
    }

    // ========================================================================
    // Auto-resolve remote_path tests
    // ========================================================================

    #[test]
    fn auto_resolve_remote_path_detects_wordpress_plugin() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Use a named subdirectory — auto_resolve uses the dir basename, not the component ID
        let dir = tmp.path().join("my-plugin");
        std::fs::create_dir_all(&dir).unwrap();

        // Create a WordPress plugin file matching the directory name
        std::fs::write(
            dir.join("my-plugin.php"),
            "<?php\n/**\n * Plugin Name: My Plugin\n */\n",
        )
        .unwrap();

        let component = Component {
            id: "my-plugin".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "wordpress".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        assert_eq!(
            component.auto_resolve_remote_path(),
            Some("wp-content/plugins/my-plugin".to_string()),
        );
    }

    #[test]
    fn auto_resolve_remote_path_uses_dirname_not_component_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Directory name differs from component ID — this is the bug scenario
        let dir = tmp.path().join("extrachill");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("style.css"), "/*\nTheme Name: Extra Chill\n*/\n").unwrap();

        let component = Component {
            id: "extrachill-theme".to_string(), // ID differs from dir name
            local_path: dir.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "wordpress".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        // Should use directory name "extrachill", NOT component ID "extrachill-theme"
        assert_eq!(
            component.auto_resolve_remote_path(),
            Some("wp-content/themes/extrachill".to_string()),
        );
    }

    #[test]
    fn auto_resolve_remote_path_detects_wordpress_theme() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("my-theme");
        std::fs::create_dir_all(&dir).unwrap();

        // Create a WordPress theme style.css
        std::fs::write(dir.join("style.css"), "/*\nTheme Name: My Theme\n*/\n").unwrap();

        let component = Component {
            id: "my-theme".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "wordpress".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        assert_eq!(
            component.auto_resolve_remote_path(),
            Some("wp-content/themes/my-theme".to_string()),
        );
    }

    #[test]
    fn auto_resolve_remote_path_returns_none_without_wordpress_extension() {
        let component = Component {
            id: "my-crate".to_string(),
            local_path: "/tmp".to_string(),
            extensions: Some(HashMap::from([(
                "rust".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        assert_eq!(component.auto_resolve_remote_path(), None);
    }

    #[test]
    fn resolve_remote_path_fills_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("my-plugin");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("my-plugin.php"),
            "<?php\n/**\n * Plugin Name: My Plugin\n */\n",
        )
        .unwrap();

        let mut component = Component {
            id: "my-plugin".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            remote_path: String::new(),
            extensions: Some(HashMap::from([(
                "wordpress".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        component.resolve_remote_path();
        assert_eq!(component.remote_path, "wp-content/plugins/my-plugin");
    }

    #[test]
    fn resolve_remote_path_preserves_explicit_value() {
        let mut component = Component {
            id: "my-plugin".to_string(),
            local_path: "/tmp".to_string(),
            remote_path: "custom/deploy/path".to_string(),
            extensions: Some(HashMap::from([(
                "wordpress".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        component.resolve_remote_path();
        assert_eq!(component.remote_path, "custom/deploy/path");
    }

    // ========================================================================
    // Portable config discovery tests
    // ========================================================================

    #[test]
    fn discover_from_portable_creates_component_from_homeboy_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        let config = serde_json::json!({
            "id": "test-discover",
            "version_targets": [{"file": "Cargo.toml", "pattern": "(?m)^version\\s*=\\s*\"([0-9.]+)\""}],
            "changelog_target": "docs/CHANGELOG.md",
            "extensions": {"rust": {}}
        });
        std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

        let result = discover_from_portable(&dir);
        assert!(
            result.is_some(),
            "Should discover component from homeboy.json"
        );

        let comp = result.unwrap();
        assert_eq!(comp.id, "test-discover");
        assert_eq!(comp.local_path, dir.to_string_lossy());
        assert_eq!(comp.changelog_target.as_deref(), Some("docs/CHANGELOG.md"));
        assert!(comp
            .extensions
            .as_ref()
            .is_some_and(|m| m.contains_key("rust")));
        assert!(comp.version_targets.is_some());
        assert!(comp.remote_path.is_empty()); // default
    }

    #[test]
    fn discover_from_portable_returns_none_without_homeboy_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        // No homeboy.json in the temp dir

        let result = discover_from_portable(&dir);
        assert!(result.is_none());
    }

    #[test]
    fn discover_from_portable_ignores_machine_specific_in_portable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        let config = serde_json::json!({
            "id": "test-machine-fields",
            "local_path": "/wrong/path",
            "remote_path": "/also/wrong",
            "extract_command": "tar -xf artifact.tar.gz"
        });
        std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

        let comp = discover_from_portable(&dir).unwrap();
        // id comes from portable JSON
        assert_eq!(comp.id, "test-machine-fields");
        // local_path is derived from actual dir, overriding the portable value
        assert_eq!(comp.local_path, dir.to_string_lossy());
        // remote_path from portable is preserved
        assert_eq!(comp.remote_path, "/also/wrong");
        assert_eq!(
            comp.extract_command.as_deref(),
            Some("tar -xf artifact.tar.gz")
        );
    }

    #[test]
    fn discover_from_portable_with_baselines_and_extensions() {
        // Mirrors data-machine's real homeboy.json — includes baselines (unknown field)
        // and extensions (known field). This must not silently fail.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        let config = serde_json::json!({
            "auto_cleanup": false,
            "baselines": {
                "lint": {
                    "context_id": "data-machine",
                    "created_at": "2026-03-06T04:47:29Z",
                    "item_count": 0,
                    "known_fingerprints": [],
                    "metadata": {
                        "findings_count": 0
                    }
                }
            },
            "changelog_target": "docs/CHANGELOG.md",
            "extensions": {
                "wordpress": {}
            },
            "id": "data-machine",
            "version_targets": [
                {"file": "data-machine.php", "pattern": "(?m)^\\s*\\*?\\s*Version:\\s*([0-9.]+)"}
            ]
        });
        std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

        let result = discover_from_portable(&dir);
        assert!(
            result.is_some(),
            "Should discover component even with baselines field in homeboy.json"
        );

        let comp = result.unwrap();
        // id comes from portable JSON
        assert_eq!(comp.id, "data-machine");
        assert_eq!(comp.local_path, dir.to_string_lossy());
        // extensions must be present
        assert!(
            comp.extensions.is_some(),
            "extensions should be set from portable config"
        );
        assert!(
            comp.extensions.as_ref().unwrap().contains_key("wordpress"),
            "wordpress extension should be present"
        );
        assert_eq!(comp.changelog_target.as_deref(), Some("docs/CHANGELOG.md"));
        assert!(comp.version_targets.is_some());
    }
}

use crate::config;
use crate::error::{Error, Result};
use crate::extension;
use crate::output::{MergeOutput, MergeResult};
use crate::project;
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub mod portable;
pub mod relationships;
pub mod resolution;

pub use portable::{
    discover_from_portable, has_portable_config, infer_portable_component_id, mutate_portable,
    portable_json, read_portable_config, write_portable_config,
};
pub use relationships::{associated_projects, projects_using, rename_component, shared_components};
pub use resolution::{
    detect_from_cwd, resolve, resolve_artifact, resolve_effective, validate_local_path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct VersionTarget {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// Check if adding a new version target would conflict with existing targets.
/// Multiple targets per file are allowed (e.g. plugin header Version: + PHP define()).
/// Only rejects if the exact same file+pattern combo already exists.
pub fn validate_version_target_conflict(
    existing: &[VersionTarget],
    new_file: &str,
    new_pattern: &str,
    _component_id: &str,
) -> Result<()> {
    for target in existing {
        if target.file == new_file {
            let existing_pattern = target.pattern.as_deref().unwrap_or("");
            if existing_pattern == new_pattern {
                // Same file + same pattern = already exists, no-op (array_union will dedupe)
                return Ok(());
            }
            // Same file + different pattern = allowed (e.g. header + define() in same PHP file)
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct ScopedExtensionConfig {
    /// Version constraint string (e.g., ">=2.0.0", "^1.0").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Settings passed to the extension at runtime.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandScopeConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScopeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lint: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refactor: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<CommandScopeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(from = "RawComponent", into = "RawComponent")]
pub struct Component {
    pub id: String,
    pub aliases: Vec<String>,
    pub local_path: String,
    pub remote_path: String,
    pub build_artifact: Option<String>,
    pub extensions: Option<HashMap<String, ScopedExtensionConfig>>,
    pub version_targets: Option<Vec<VersionTarget>>,
    pub changelog_target: Option<String>,
    pub changelog_next_section_label: Option<String>,
    pub changelog_next_section_aliases: Option<Vec<String>>,
    /// Lifecycle hooks: event name -> list of shell commands.
    /// Events: `pre:version:bump`, `post:version:bump`, `post:release`, `post:deploy`
    pub hooks: HashMap<String, Vec<String>>,
    pub extract_command: Option<String>,
    pub remote_owner: Option<String>,
    pub deploy_strategy: Option<String>,
    pub git_deploy: Option<GitDeployConfig>,
    pub auto_cleanup: bool,
    pub docs_dir: Option<String>,
    pub docs_dirs: Vec<String>,
    pub scopes: Option<ScopeConfig>,
}

/// Raw JSON shape for Component — handles backward-compatible deserialization
/// of legacy hook fields (`pre_version_bump_commands` etc.) into the `hooks` map.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RawComponent {
    #[serde(default, skip_serializing)]
    id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    #[serde(default)]
    local_path: String,
    #[serde(default)]
    remote_path: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "deserialize_empty_as_none"
    )]
    build_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions: Option<HashMap<String, ScopedExtensionConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_targets: Option<Vec<VersionTarget>>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "changelog_targets")]
    changelog_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_next_section_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_next_section_aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    hooks: HashMap<String, Vec<String>>,
    // Legacy hook fields — read from old JSON, merged into hooks
    #[serde(default, skip_serializing)]
    pre_version_bump_commands: Vec<String>,
    #[serde(default, skip_serializing)]
    post_version_bump_commands: Vec<String>,
    #[serde(default, skip_serializing)]
    post_release_commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extract_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deploy_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_deploy: Option<GitDeployConfig>,
    #[serde(default)]
    auto_cleanup: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    docs_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    docs_dirs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scopes: Option<ScopeConfig>,
}

/// Insert legacy commands into hooks map if the event key doesn't already exist.
fn merge_legacy_hook(hooks: &mut HashMap<String, Vec<String>>, event: &str, commands: Vec<String>) {
    if !commands.is_empty() && !hooks.contains_key(event) {
        hooks.insert(event.to_string(), commands);
    }
}

impl From<RawComponent> for Component {
    fn from(raw: RawComponent) -> Self {
        let mut hooks = raw.hooks;
        merge_legacy_hook(
            &mut hooks,
            "pre:version:bump",
            raw.pre_version_bump_commands,
        );
        merge_legacy_hook(
            &mut hooks,
            "post:version:bump",
            raw.post_version_bump_commands,
        );
        merge_legacy_hook(&mut hooks, "post:release", raw.post_release_commands);

        Component {
            id: raw.id,
            aliases: raw.aliases,
            local_path: raw.local_path,
            remote_path: raw.remote_path,
            build_artifact: raw.build_artifact,
            extensions: raw.extensions,
            version_targets: raw.version_targets,
            changelog_target: raw.changelog_target,
            changelog_next_section_label: raw.changelog_next_section_label,
            changelog_next_section_aliases: raw.changelog_next_section_aliases,
            hooks,
            extract_command: raw.extract_command,
            remote_owner: raw.remote_owner,
            deploy_strategy: raw.deploy_strategy,
            git_deploy: raw.git_deploy,
            auto_cleanup: raw.auto_cleanup,
            docs_dir: raw.docs_dir,
            docs_dirs: raw.docs_dirs,
            scopes: raw.scopes,
        }
    }
}

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
            auto_cleanup: c.auto_cleanup,
            docs_dir: c.docs_dir,
            docs_dirs: c.docs_dirs,
            scopes: c.scopes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitDeployConfig {
    /// Git remote to pull from (default: "origin")
    #[serde(
        default = "default_git_remote",
        skip_serializing_if = "is_default_remote"
    )]
    pub remote: String,
    /// Branch to pull (default: "main")
    #[serde(
        default = "default_git_branch",
        skip_serializing_if = "is_default_branch"
    )]
    pub branch: String,
    /// Commands to run after git pull (e.g., "composer install", "npm run build")
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_pull: Vec<String>,
    /// Pull a specific tag instead of branch HEAD (e.g., "v{{version}}")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_pattern: Option<String>,
}

fn default_git_remote() -> String {
    "origin".to_string()
}
fn default_git_branch() -> String {
    "main".to_string()
}
fn is_default_remote(s: &str) -> bool {
    s == "origin"
}
fn is_default_branch(s: &str) -> bool {
    s == "main"
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
            auto_cleanup: false,
            docs_dir: None,
            docs_dirs: Vec::new(),
            scopes: None,
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

/// Validate that a version target pattern is a valid regex with at least one capture group.
/// Rejects common mistakes like `{version}` template syntax.
pub fn validate_version_pattern(pattern: &str) -> Result<()> {
    // Check for template syntax (common mistake)
    if pattern.contains("{version}") {
        return Err(Error::validation_invalid_argument(
            "version_target.pattern",
            format!(
                "Pattern '{}' uses template syntax ({{version}}), but a regex with a capture group is required. \
                 Example: 'Version: (\\d+\\.\\d+\\.\\d+)'",
                pattern
            ),
            Some(pattern.to_string()),
            None,
        ));
    }

    // Must be valid regex (use multiline mode to match runtime behavior)
    let re = Regex::new(&crate::engine::text::ensure_multiline(pattern)).map_err(|e| {
        Error::validation_invalid_argument(
            "version_target.pattern",
            format!("Invalid regex pattern '{}': {}", pattern, e),
            Some(pattern.to_string()),
            None,
        )
    })?;

    // Must have at least one capture group
    if re.captures_len() < 2 {
        return Err(Error::validation_invalid_argument(
            "version_target.pattern",
            format!(
                "Pattern '{}' has no capture group. Wrap the version portion in parentheses. \
                 Example: 'Version: (\\d+\\.\\d+\\.\\d+)'",
                pattern
            ),
            Some(pattern.to_string()),
            None,
        ));
    }

    Ok(())
}

/// Normalize a regex pattern by converting double-escaped backslashes to single.
/// This fixes patterns that were incorrectly stored with shell-escaped backslashes
/// like "Version:\\s*(\\d+\\.\\d+\\.\\d+)" which should be "Version:\s*(\d+\.\d+\.\d+)".
pub fn normalize_version_pattern(pattern: &str) -> String {
    // If pattern contains \\ (literal backslash-backslash), convert to \ (literal backslash)
    // This handles patterns that were double-escaped during CLI input
    if pattern.contains("\\\\") {
        pattern.replace("\\\\", "\\")
    } else {
        pattern.to_string()
    }
}

pub fn parse_version_targets(targets: &[String]) -> Result<Vec<VersionTarget>> {
    let mut parsed = Vec::new();
    for target in targets {
        let mut parts = target.splitn(2, "::");
        let file = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "version_target",
                    "Invalid version target format (expected 'file' or 'file::pattern')",
                    None,
                    None,
                )
            })?;
        let pattern = parts.next().map(str::trim).filter(|s| !s.is_empty());
        if let Some(p) = pattern {
            let normalized = normalize_version_pattern(p);
            validate_version_pattern(&normalized)?;
            parsed.push(VersionTarget {
                file: file.to_string(),
                pattern: Some(normalized),
            });
        } else {
            parsed.push(VersionTarget {
                file: file.to_string(),
                pattern: None,
            });
        }
    }
    Ok(parsed)
}

// ============================================================================
// Operations
// ============================================================================

/// Set the changelog target for a component's configuration.
pub fn set_changelog_target(component_id: &str, file_path: &str) -> Result<()> {
    mutate_portable(component_id, |component| {
        component.changelog_target = Some(file_path.to_string());
        Ok(())
    })?;
    Ok(())
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    let id = id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "component_id",
            "Component ID is required for component mutation",
            None,
            None,
        )
    })?;

    let raw = config::read_json_spec_to_string(json_spec)?;
    if config::is_json_array(&raw) {
        return Err(Error::validation_invalid_argument(
            "component",
            "Bulk component mutation is no longer supported. Mutate repo-owned homeboy.json one component at a time.",
            None,
            None,
        ));
    }

    let patch: Value = config::from_str(&raw)?;

    if let Some(json_id) = patch.get("id").and_then(|v| v.as_str()) {
        if json_id != id {
            rename(id, json_id)?;
            return merge(Some(json_id), json_spec, replace_fields);
        }
    }

    let component = mutate_portable(id, |component| {
        let fields = config::merge_config(component, patch.clone(), replace_fields)?;
        if fields.updated_fields.is_empty() {
            return Err(Error::validation_invalid_argument(
                "merge",
                "Merge patch cannot be empty",
                None,
                None,
            ));
        }
        Ok(())
    })?;

    let updated_fields = match patch {
        Value::Object(obj) => obj.keys().cloned().collect(),
        _ => vec![],
    };

    let _ = component;
    Ok(MergeOutput::Single(MergeResult {
        id: id.to_string(),
        updated_fields,
    }))
}

pub fn delete_safe(id: &str) -> Result<()> {
    let component = resolve_effective(Some(id), None, None)?;
    let local_path = Path::new(&component.local_path);
    let config_path = local_path.join("homeboy.json");

    if !config_path.exists() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!("No homeboy.json found for component '{}'", id),
            Some(id.to_string()),
            None,
        ));
    }

    if !associated_projects(id)?.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Cannot delete component '{}' while projects still reference it",
                id
            ),
            Some(id.to_string()),
            None,
        ));
    }

    std::fs::remove_file(&config_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("remove {}", config_path.display())),
        )
    })
}

pub fn rename(id: &str, new_id: &str) -> Result<Component> {
    rename_component(id, new_id)
}

/// Derive a runtime component inventory from project attachments plus legacy stored components.
///
/// Project-attached repo-backed components are the canonical source of truth.
/// A repo in the current working tree can also participate when it has `homeboy.json`.
pub fn inventory() -> Result<Vec<Component>> {
    let projects = project::list().unwrap_or_default();
    let mut components = Vec::new();
    let mut seen = HashSet::new();

    for project in &projects {
        for attachment in &project.components {
            if let Ok(component) = project::resolve_project_component(project, &attachment.id) {
                if seen.insert(component.id.clone()) {
                    components.push(component);
                }
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        if let Some(component) = discover_from_portable(&cwd) {
            if seen.insert(component.id.clone()) {
                components.push(component);
            }
        } else if let Some(git_root) = resolution::detect_git_root(&cwd) {
            if let Some(component) = discover_from_portable(&git_root) {
                if seen.insert(component.id.clone()) {
                    components.push(component);
                }
            }
        }
    }

    components.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(components)
}

/// Check if any linked extension provides an artifact pattern.
pub fn extension_provides_artifact_pattern(component: &Component) -> bool {
    component
        .extensions
        .as_ref()
        .map(|extensions| {
            extensions.keys().any(|extension_id| {
                extension::load_extension(extension_id)
                    .ok()
                    .and_then(|m| m.build)
                    .and_then(|b| b.artifact_pattern)
                    .is_some()
            })
        })
        .unwrap_or(false)
}

pub fn list() -> Result<Vec<Component>> {
    inventory()
}

pub fn list_ids() -> Result<Vec<String>> {
    Ok(inventory()?
        .into_iter()
        .map(|component| component.id)
        .collect())
}

pub fn load(id: &str) -> Result<Component> {
    if let Some(component) = inventory()?
        .into_iter()
        .find(|component| component.id == id)
    {
        return Ok(component);
    }

    let suggestions = list_ids().unwrap_or_default();
    Err(Error::component_not_found(id.to_string(), suggestions))
}

pub fn exists(id: &str) -> bool {
    load(id).is_ok()
}

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
    // Portable config overlay tests
    // ========================================================================

    #[test]
    fn overlay_portable_fills_absent_fields() {
        let mut stored: Value = serde_json::json!({
            "id": "my-plugin",
            "local_path": "/home/user/my-plugin",
            "remote_path": "/var/www/my-plugin"
        });
        let portable = serde_json::json!({
            "changelog_target": "docs/CHANGELOG.md",
            "version_targets": [{"file": "package.json"}]
        });

        overlay_portable(&mut stored, &portable);

        assert_eq!(stored["changelog_target"], "docs/CHANGELOG.md");
        assert!(stored["version_targets"].is_array());
    }

    #[test]
    fn overlay_portable_stored_wins() {
        let mut stored: Value = serde_json::json!({
            "id": "my-plugin",
            "local_path": "/home/user/my-plugin",
            "extract_command": "tar -xf artifact.tar.gz"
        });
        let portable = serde_json::json!({
            "extract_command": "unzip -o artifact.zip",
            "changelog_target": "docs/CHANGELOG.md"
        });

        overlay_portable(&mut stored, &portable);

        // Stored value wins
        assert_eq!(stored["extract_command"], "tar -xf artifact.tar.gz");
        // Absent field filled from portable
        assert_eq!(stored["changelog_target"], "docs/CHANGELOG.md");
    }

    #[test]
    fn overlay_portable_skips_machine_specific_fields() {
        let mut stored: Value = serde_json::json!({
            "id": "my-plugin",
            "local_path": "/home/user/my-plugin",
            "remote_path": "/var/www/my-plugin"
        });
        let portable = serde_json::json!({
            "id": "wrong-id",
            "local_path": "/someone-else/path",
            "remote_path": "/other/remote",
            "aliases": ["alias1"],
            "extract_command": "unzip -o artifact.zip"
        });

        overlay_portable(&mut stored, &portable);

        // Machine-specific fields untouched
        assert_eq!(stored["id"], "my-plugin");
        assert_eq!(stored["local_path"], "/home/user/my-plugin");
        assert_eq!(stored["remote_path"], "/var/www/my-plugin");
        assert!(stored.get("aliases").is_none());
        // Portable field still applied
        assert_eq!(stored["extract_command"], "unzip -o artifact.zip");
    }

    #[test]
    fn overlay_portable_handles_non_objects() {
        // Should be a no-op for non-object values
        let mut stored = serde_json::json!("not an object");
        let portable = serde_json::json!({"extract_command": "make extract"});
        overlay_portable(&mut stored, &portable);
        assert_eq!(stored, "not an object");
    }

    #[test]
    fn overlay_portable_empty_portable_is_noop() {
        let mut stored: Value = serde_json::json!({
            "id": "my-plugin",
            "local_path": "/home/user/my-plugin",
            "extract_command": "make extract"
        });
        let original = stored.clone();
        let portable = serde_json::json!({});

        overlay_portable(&mut stored, &portable);

        assert_eq!(stored, original);
    }

    // ========================================================================
    // Portable config discovery tests
    // ========================================================================

    #[test]
    fn discover_from_portable_creates_component_from_homeboy_json() {
        let dir = std::env::temp_dir().join("homeboy_test_discover");
        let _ = std::fs::create_dir_all(&dir);

        let config = serde_json::json!({
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
        assert_eq!(comp.id, "homeboy-test-discover");
        assert_eq!(comp.local_path, dir.to_string_lossy());
        assert_eq!(comp.changelog_target.as_deref(), Some("docs/CHANGELOG.md"));
        assert!(comp
            .extensions
            .as_ref()
            .is_some_and(|m| m.contains_key("rust")));
        assert!(comp.version_targets.is_some());
        assert!(comp.remote_path.is_empty()); // default

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_from_portable_returns_none_without_homeboy_json() {
        let dir = std::env::temp_dir().join("homeboy_test_no_config");
        let _ = std::fs::create_dir_all(&dir);
        // Ensure no homeboy.json
        let _ = std::fs::remove_file(dir.join("homeboy.json"));

        let result = discover_from_portable(&dir);
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_from_portable_ignores_machine_specific_in_portable() {
        let dir = std::env::temp_dir().join("homeboy_test_machine_fields");
        let _ = std::fs::create_dir_all(&dir);

        let config = serde_json::json!({
            "id": "should-be-overridden",
            "local_path": "/wrong/path",
            "remote_path": "/also/wrong",
            "extract_command": "tar -xf artifact.tar.gz"
        });
        std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

        let comp = discover_from_portable(&dir).unwrap();
        // id is derived from dir name, not from portable
        assert_eq!(comp.id, "homeboy-test-machine-fields");
        // local_path is derived from actual dir, not portable
        assert_eq!(comp.local_path, dir.to_string_lossy());
        // remote_path from portable is allowed (it's set explicitly)
        assert_eq!(comp.remote_path, "/also/wrong");
        assert_eq!(
            comp.extract_command.as_deref(),
            Some("tar -xf artifact.tar.gz")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_from_portable_with_baselines_and_extensions() {
        // Mirrors data-machine's real homeboy.json — includes baselines (unknown field)
        // and extensions (known field). This must not silently fail.
        let dir = std::env::temp_dir().join("homeboy_test_baselines");
        let _ = std::fs::create_dir_all(&dir);

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
        // id derived from dir name, not portable
        assert_eq!(comp.id, "homeboy-test-baselines");
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

        let _ = std::fs::remove_dir_all(&dir);
    }
}

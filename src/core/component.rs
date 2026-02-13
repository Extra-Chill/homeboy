use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::module;
use crate::output::{CreateOutput, MergeOutput, MergeResult, RemoveResult};
use crate::project::{self, NullableUpdate};
use crate::utils::slugify;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct VersionTarget {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// Check if adding a new version target would conflict with existing targets.
/// Returns error if same file already has a different pattern.
pub fn validate_version_target_conflict(
    existing: &[VersionTarget],
    new_file: &str,
    new_pattern: &str,
    component_id: &str,
) -> Result<()> {
    for target in existing {
        if target.file == new_file {
            let existing_pattern = target.pattern.as_deref().unwrap_or("");
            if existing_pattern != new_pattern {
                return Err(Error::validation_invalid_argument(
                    "version_targets",
                    format!(
                        "File '{}' already has a version target with a different pattern. \
                         Existing: '{}', New: '{}'",
                        new_file, existing_pattern, new_pattern
                    ),
                    None,
                    None,
                )
                .with_hint(format!(
                    "To replace existing targets: homeboy component set {} --replace version_targets --version-target \"{}::{}\"",
                    component_id, new_file, new_pattern
                )));
            }
            // Same file + same pattern = OK (array_union will dedupe)
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct ScopedModuleConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct Component {
    #[serde(skip_deserializing)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub local_path: String,
    pub remote_path: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "deserialize_empty_as_none"
    )]
    pub build_artifact: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub modules: Option<HashMap<String, ScopedModuleConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_targets: Option<Vec<VersionTarget>>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "changelog_targets")]
    pub changelog_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_next_section_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_next_section_aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_version_bump_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_version_bump_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_release_commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_command: Option<String>,

    /// Deployment strategy: "rsync" (default) or "git"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_strategy: Option<String>,
    /// Git deploy configuration (used when deploy_strategy = "git")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_deploy: Option<GitDeployConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitDeployConfig {
    /// Git remote to pull from (default: "origin")
    #[serde(default = "default_git_remote", skip_serializing_if = "is_default_remote")]
    pub remote: String,
    /// Branch to pull (default: "main")
    #[serde(default = "default_git_branch", skip_serializing_if = "is_default_branch")]
    pub branch: String,
    /// Commands to run after git pull (e.g., "composer install", "npm run build")
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_pull: Vec<String>,
    /// Pull a specific tag instead of branch HEAD (e.g., "v{{version}}")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_pattern: Option<String>,
}

fn default_git_remote() -> String { "origin".to_string() }
fn default_git_branch() -> String { "main".to_string() }
fn is_default_remote(s: &str) -> bool { s == "origin" }
fn is_default_branch(s: &str) -> bool { s == "main" }

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
            modules: None,
            version_targets: None,
            changelog_target: None,
            changelog_next_section_label: None,
            changelog_next_section_aliases: None,
            pre_version_bump_commands: Vec::new(),
            post_version_bump_commands: Vec::new(),
            post_release_commands: Vec::new(),
            build_command: None,
            extract_command: None,
            deploy_strategy: None,
            git_deploy: None,
        }
    }
}

/// Normalize empty strings to None. Treats "", null, and field omission identically for consistent validation.
fn deserialize_empty_as_none<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

impl ConfigEntity for Component {
    const ENTITY_TYPE: &'static str = "component";
    const DIR_NAME: &'static str = "components";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::component_not_found(id, suggestions)
    }
    fn aliases(&self) -> &[String] {
        &self.aliases
    }
}

// ============================================================================
// Core CRUD - Thin wrappers around config module
// ============================================================================

pub fn load(id: &str) -> Result<Component> {
    config::load::<Component>(id)
}

pub fn list() -> Result<Vec<Component>> {
    config::list::<Component>()
}

pub fn list_ids() -> Result<Vec<String>> {
    config::list_ids::<Component>()
}

pub fn save(component: &Component) -> Result<()> {
    config::save(component)
}

pub fn delete(id: &str) -> Result<()> {
    config::delete::<Component>(id)
}

pub fn exists(id: &str) -> bool {
    config::exists::<Component>(id)
}

/// Unified merge that auto-detects single vs bulk operations.
/// Array input triggers batch merge, object input triggers single merge.
/// Single merge supports auto-rename if JSON contains a different `id` field.
pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    let raw = config::read_json_spec_to_string(json_spec)?;

    if config::is_json_array(&raw) {
        return Ok(MergeOutput::Bulk(
            config::merge_batch_from_json::<Component>(&raw)?,
        ));
    }

    Ok(MergeOutput::Single(merge_from_json(
        id,
        &raw,
        replace_fields,
    )?))
}

/// Merge JSON into component config with auto-rename support.
/// If JSON contains an `id` field that differs from the target, automatically renames the component.
fn merge_from_json(
    id: Option<&str>,
    json_spec: &str,
    replace_fields: &[String],
) -> Result<MergeResult> {
    let raw = config::read_json_spec_to_string(json_spec)?;
    let parsed: serde_json::Value = config::from_str(&raw)?;

    if let Some(json_id) = parsed.get("id").and_then(|v| v.as_str()) {
        if let Some(current_id) = id {
            if json_id != current_id {
                rename(current_id, json_id)?;
                return config::merge_from_json::<Component>(
                    Some(json_id),
                    json_spec,
                    replace_fields,
                );
            }
        }
    }

    config::merge_from_json::<Component>(id, json_spec, replace_fields)
}

pub fn remove_from_json(id: Option<&str>, json_spec: &str) -> Result<RemoveResult> {
    config::remove_from_json::<Component>(id, json_spec)
}

pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<Component>> {
    config::create::<Component>(json_spec, skip_existing)
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
        parsed.push(VersionTarget {
            file: file.to_string(),
            pattern: pattern.map(|p| p.to_string()),
        });
    }
    Ok(parsed)
}

pub fn slugify_id(name: &str) -> Result<String> {
    slugify::slugify_id(name, "name")
}

// ============================================================================
// Operations
// ============================================================================

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub id: String,
    pub component: Component,
    pub updated_fields: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RenameResult {
    pub old_id: String,
    pub new_id: String,
    pub component: Component,
}

pub fn update(
    component_id: &str,
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
    build_command: NullableUpdate<String>,
    extract_command: NullableUpdate<String>,
) -> Result<UpdateResult> {
    let mut component = load(component_id)?;
    let mut updated = Vec::new();

    if let Some(new_local_path) = local_path {
        component.local_path = new_local_path;
        updated.push("localPath".to_string());
    }

    if let Some(new_remote_path) = remote_path {
        component.remote_path = new_remote_path;
        updated.push("remotePath".to_string());
    }

    if let Some(new_build_artifact) = build_artifact {
        component.build_artifact = Some(new_build_artifact);
        updated.push("buildArtifact".to_string());
    }

    if let Some(new_build_command) = build_command {
        component.build_command = new_build_command;
        updated.push("buildCommand".to_string());
    }

    if let Some(new_extract_command) = extract_command {
        component.extract_command = new_extract_command;
        updated.push("extractCommand".to_string());
    }

    save(&component)?;

    Ok(UpdateResult {
        id: component_id.to_string(),
        component,
        updated_fields: updated,
    })
}

/// Set the changelog target for a component's configuration.
pub fn set_changelog_target(component_id: &str, file_path: &str) -> Result<()> {
    let mut component = load(component_id)?;
    component.changelog_target = Some(file_path.to_string());
    save(&component)
}

pub fn rename(id: &str, new_id: &str) -> Result<Component> {
    let new_id = new_id.to_lowercase();
    config::rename::<Component>(id, &new_id)?;
    update_project_references(id, &new_id)?;
    load(&new_id)
}

fn update_project_references(old_id: &str, new_id: &str) -> Result<()> {
    let projects = project::list().unwrap_or_default();
    for proj in projects {
        if proj.component_ids.contains(&old_id.to_string()) {
            let updated_ids: Vec<String> = proj
                .component_ids
                .iter()
                .map(|comp_id: &String| {
                    if comp_id == old_id {
                        new_id.to_string()
                    } else {
                        comp_id.clone()
                    }
                })
                .collect();
            project::set_components(&proj.id, updated_ids)?;
        }
    }
    Ok(())
}

pub fn projects_using(component_id: &str) -> Result<Vec<String>> {
    let projects = project::list().unwrap_or_default();
    Ok(projects
        .iter()
        .filter(|p| p.component_ids.contains(&component_id.to_string()))
        .map(|p| p.id.clone())
        .collect())
}

/// Returns a map of component_id -> Vec<project_id> for all components used by projects.
/// Only includes components that are used by at least one project.
pub fn shared_components() -> Result<std::collections::HashMap<String, Vec<String>>> {
    let projects = project::list().unwrap_or_default();
    let mut sharing: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    for project in projects {
        for component_id in &project.component_ids {
            sharing
                .entry(component_id.clone())
                .or_default()
                .push(project.id.clone());
        }
    }

    Ok(sharing)
}

pub fn delete_safe(id: &str) -> Result<()> {
    if !exists(id) {
        let suggestions = config::find_similar_ids::<Component>(id);
        return Err(Component::not_found_error(id.to_string(), suggestions));
    }

    let using = projects_using(id)?;

    if !using.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Component '{}' is used by projects: {}. Remove from projects first.",
                id,
                using.join(", ")
            ),
            Some(id.to_string()),
            Some(using),
        ));
    }

    delete(id)
}

/// Resolve effective artifact path for a component.
/// Returns the component's explicit artifact OR the module's pattern (with substitution).
pub fn resolve_artifact(component: &Component) -> Option<String> {
    // 1. Component has explicit artifact
    if let Some(ref artifact) = component.build_artifact {
        return Some(artifact.clone());
    }

    // 2. Check if any linked module provides an artifact pattern
    if let Some(ref modules) = component.modules {
        for module_id in modules.keys() {
            if let Ok(manifest) = module::load_module(module_id) {
                if let Some(ref build) = manifest.build {
                    if let Some(ref pattern) = build.artifact_pattern {
                        // Substitute template variables
                        let resolved = pattern
                            .replace("{component_id}", &component.id)
                            .replace("{local_path}", &component.local_path);
                        return Some(resolved);
                    }
                }
            }
        }
    }

    // 3. No artifact configured and no module pattern
    None
}

/// Check if any linked module provides an artifact pattern.
pub fn module_provides_artifact_pattern(component: &Component) -> bool {
    component
        .modules
        .as_ref()
        .map(|modules| {
            modules.keys().any(|module_id| {
                module::load_module(module_id)
                    .ok()
                    .and_then(|m| m.build)
                    .and_then(|b| b.artifact_pattern)
                    .is_some()
            })
        })
        .unwrap_or(false)
}

/// Validates component local_path is usable (absolute and exists).
/// Expands tilde to home directory before validation.
/// Returns the validated PathBuf on success, or an actionable error with self-healing hints.
pub fn validate_local_path(component: &Component) -> Result<PathBuf> {
    // Expand tilde to home directory (e.g., ~/Developer -> /Users/chubes/Developer)
    let expanded = shellexpand::tilde(&component.local_path);
    let path = PathBuf::from(expanded.as_ref());

    // Check if relative path (no leading /)
    if !path.is_absolute() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            format!(
                "Component '{}' has relative local_path '{}' which cannot be resolved. \
                Use absolute path like /Users/chubes/path/to/component",
                component.id, component.local_path
            ),
            Some(component.id.clone()),
            None,
        )
        .with_hint(format!(
            "Set absolute path: homeboy component set {} --local-path \"/full/path/to/{}\"",
            component.id, component.local_path
        ))
        .with_hint("Use 'pwd' in the component directory to get the absolute path".to_string()));
    }

    // Check if path exists
    if !path.exists() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            format!(
                "Component '{}' local_path does not exist: {}",
                component.id, path.display()
            ),
            Some(component.id.clone()),
            None,
        )
        .with_hint(format!("Verify the path exists: ls -la {}", path.display()))
        .with_hint(format!(
            "Update path: homeboy component set {} --local-path \"/correct/path\"",
            component.id
        )));
    }

    Ok(path)
}

/// Detect component ID from current working directory.
/// Returns Some(component_id) if cwd matches or is within a component's local_path.
pub fn detect_from_cwd() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let components = list().ok()?;

    for component in components {
        let expanded = shellexpand::tilde(&component.local_path);
        let local_path = Path::new(expanded.as_ref());

        if cwd.starts_with(local_path) {
            return Some(component.id);
        }
    }
    None
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

        let result =
            validate_version_target_conflict(&existing, "plugin.php", "define('VER', '(.*)')", "test-comp");
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Check the error details contain expected text
        let details_str = err.details.to_string();
        assert!(
            details_str.contains("already has a version target with a different pattern"),
            "Expected error details to contain 'already has a version target with a different pattern', got: {}",
            details_str
        );
    }

    #[test]
    fn validate_version_target_conflict_same_pattern_ok() {
        let existing = vec![VersionTarget {
            file: "plugin.php".to_string(),
            pattern: Some("Version: (.*)".to_string()),
        }];

        let result = validate_version_target_conflict(&existing, "plugin.php", "Version: (.*)", "test-comp");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_target_conflict_different_file_ok() {
        let existing = vec![VersionTarget {
            file: "plugin.php".to_string(),
            pattern: Some("Version: (.*)".to_string()),
        }];

        let result =
            validate_version_target_conflict(&existing, "package.json", "\"version\": \"(.*)\"", "test-comp");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_target_conflict_empty_existing_ok() {
        let existing: Vec<VersionTarget> = vec![];

        let result = validate_version_target_conflict(&existing, "plugin.php", "Version: (.*)", "test-comp");
        assert!(result.is_ok());
    }
}

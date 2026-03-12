use crate::config;
use crate::error::{Error, Result};
use crate::extension;
use crate::output::{MergeOutput, MergeResult};
use crate::project;
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub mod portable;

pub use portable::{
    discover_from_portable, has_portable_config, infer_portable_component_id, mutate_portable,
    portable_json, read_portable_config, write_portable_config,
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

fn update_project_references(old_id: &str, new_id: &str) -> Result<()> {
    let projects = project::list().unwrap_or_default();
    for proj in projects {
        if project::has_component(&proj, old_id) {
            let updated_components: Vec<project::ProjectComponentAttachment> = proj
                .components
                .into_iter()
                .map(|mut component| {
                    if component.id == old_id {
                        component.id = new_id.to_string();
                    }
                    component
                })
                .collect();
            project::set_component_attachments(&proj.id, updated_components)?;
        }
    }
    Ok(())
}

pub fn projects_using(component_id: &str) -> Result<Vec<String>> {
    let projects = project::list().unwrap_or_default();
    Ok(projects
        .iter()
        .filter(|p| project::has_component(p, component_id))
        .map(|p| p.id.clone())
        .collect())
}

/// Returns a map of component_id -> Vec<project_id> for all components used by projects.
/// Only includes components that are used by at least one project.
pub fn shared_components() -> Result<std::collections::HashMap<String, Vec<String>>> {
    let projects = project::list().unwrap_or_default();
    let mut sharing: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for project in projects {
        for component_id in project::project_component_ids(&project) {
            sharing
                .entry(component_id)
                .or_default()
                .push(project.id.clone());
        }
    }

    Ok(sharing)
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
    let resolved_new_id = crate::engine::identifier::slugify_id(new_id, "component_id")?;
    let component = mutate_portable(id, |component| {
        component.id = resolved_new_id.clone();
        Ok(())
    })?;
    update_project_references(id, &resolved_new_id)?;
    Ok(component)
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
        } else if let Some(git_root) = detect_git_root(&cwd) {
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

/// Find project associations using the canonical project attachment model.
pub fn associated_projects(component_id: &str) -> Result<Vec<String>> {
    let projects = project::list().unwrap_or_default();
    Ok(projects
        .into_iter()
        .filter(|project| project::has_component(project, component_id))
        .map(|project| project.id)
        .collect())
}

/// Resolve effective artifact path for a component.
/// Returns the component's explicit artifact OR the extension's pattern (with substitution).
pub fn resolve_artifact(component: &Component) -> Option<String> {
    // 1. Component has explicit artifact
    if let Some(ref artifact) = component.build_artifact {
        return Some(artifact.clone());
    }

    // 2. Check if any linked extension provides an artifact pattern
    if let Some(ref extensions) = component.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
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

    // 3. No artifact configured and no extension pattern
    None
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
                component.id,
                path.display()
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
    let components = inventory().ok()?;

    for component in components {
        let expanded = shellexpand::tilde(&component.local_path);
        let local_path = Path::new(expanded.as_ref());

        if cwd.starts_with(local_path) {
            return Some(component.id);
        }
    }
    None
}

/// Find the git root directory for a given path.
fn detect_git_root(dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
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

/// Resolve a Component from an optional ID, with CWD auto-discovery fallback.
///
/// Resolution order:
/// 1. Explicit `id` → load from canonical inventory
/// 2. No `id` → try inventory detection from CWD
/// 3. Still no match → try `homeboy.json` in CWD
/// 4. Still no match → try `homeboy.json` at git root (covers subdirectories)
pub fn resolve(id: Option<&str>) -> Result<Component> {
    // Explicit ID: load from config (post_load applies portable layering)
    if let Some(id) = id {
        return load(id);
    }

    // Try registered component detection from CWD
    if let Some(detected_id) = detect_from_cwd() {
        return load(&detected_id);
    }

    // Try portable config discovery from CWD
    let cwd = std::env::current_dir().map_err(|e| Error::internal_io(e.to_string(), None))?;

    if let Some(component) = discover_from_portable(&cwd) {
        return Ok(component);
    }

    // Try git root as fallback (e.g., running from a subdirectory)
    if let Some(git_root) = detect_git_root(&cwd) {
        if git_root != cwd {
            if let Some(component) = discover_from_portable(&git_root) {
                return Ok(component);
            }
        }
    }

    // Nothing found — produce a helpful error
    let mut hints = vec![
        "Provide a component ID: homeboy <command> <component-id>".to_string(),
        "Or run from a directory containing homeboy.json".to_string(),
    ];
    if detect_from_cwd().is_none() {
        hints.push("Initialize the repo: homeboy component create --local-path .".to_string());
        hints.push(
            "Or attach the repo to a project: homeboy project components attach-path <project> ."
                .to_string(),
        );
    }

    Err(Error::validation_invalid_argument(
        "component_id",
        "No component ID provided and no homeboy.json found in current directory",
        None,
        Some(hints),
    ))
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

/// Resolve the effective component for runtime operations.
///
/// Resolution order:
/// 1. If `project` + explicit `id` are provided, use project-owned component resolution.
/// 2. If explicit `id` + `path_override` are provided, require portable discovery at path.
/// 3. If only explicit `id` is provided, use canonical inventory lookup.
/// 4. If no explicit `id`, fall back to `resolve(None)` (CWD / git-root portable discovery).
pub fn resolve_effective(
    id: Option<&str>,
    path_override: Option<&str>,
    project: Option<&crate::project::Project>,
) -> Result<Component> {
    if let (Some(project), Some(id)) = (project, id) {
        let mut component = crate::project::resolve_project_component(project, id)?;
        if let Some(path) = path_override {
            component.local_path = path.to_string();
        }
        return Ok(component);
    }

    if let Some(id) = id {
        if let Some(path) = path_override {
            if let Some(mut discovered) = discover_from_portable(Path::new(path)) {
                discovered.id = id.to_string();
                discovered.local_path = path.to_string();
                Ok(discovered)
            } else {
                Err(Error::validation_invalid_argument(
                    "local_path",
                    format!("No homeboy.json found at {}", path),
                    Some(id.to_string()),
                    None,
                ))
            }
        } else {
            load(id)
        }
    } else {
        let mut component = resolve(None)?;
        if let Some(path) = path_override {
            component.local_path = path.to_string();
        }
        Ok(component)
    }
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

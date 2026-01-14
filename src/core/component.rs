use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::json;
use crate::paths;
use crate::project;
use crate::slugify;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionTarget {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogTarget {
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScopedModuleConfig {
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    #[serde(skip_deserializing)]
    pub id: String,
    pub local_path: String,
    pub remote_path: String,
    pub build_artifact: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub scoped_modules: Option<HashMap<String, ScopedModuleConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_targets: Option<Vec<VersionTarget>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_targets: Option<Vec<ChangelogTarget>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_next_section_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_next_section_aliases: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_command: Option<String>,
}

impl Component {
    pub fn new(
        id: String,
        local_path: String,
        remote_path: String,
        build_artifact: String,
    ) -> Self {
        Self {
            id,
            local_path,
            remote_path,
            build_artifact,
            scoped_modules: None,
            version_targets: None,
            changelog_targets: None,
            changelog_next_section_label: None,
            changelog_next_section_aliases: None,
            build_command: None,
            extract_command: None,
        }
    }
}

impl ConfigEntity for Component {
    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn config_path(id: &str) -> Result<PathBuf> {
        paths::component(id)
    }
    fn config_dir() -> Result<PathBuf> {
        paths::components()
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::component_not_found(id, suggestions)
    }
    fn entity_type() -> &'static str {
        "component"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentRecord {
    pub id: String,
    pub config: Component,
}

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

/// Merge JSON into component config. Accepts JSON string, @file, or - for stdin.
/// ID can be provided as argument or extracted from JSON body.
/// If JSON contains an `id` field that differs from the target, automatically renames the component.
pub fn merge_from_json(id: Option<&str>, json_spec: &str) -> Result<json::MergeResult> {
    let raw = json::read_json_spec_to_string(json_spec)?;
    let parsed: serde_json::Value = json::from_str(&raw)?;

    if let Some(json_id) = parsed.get("id").and_then(|v| v.as_str()) {
        if let Some(current_id) = id {
            if json_id != current_id {
                rename(current_id, json_id)?;
                return config::merge_from_json::<Component>(Some(json_id), json_spec);
            }
        }
    }

    config::merge_from_json::<Component>(id, json_spec)
}

/// Remove items from component config arrays. Accepts JSON string, @file, or - for stdin.
/// ID can be provided as argument or extracted from JSON body.
pub fn remove_from_json(id: Option<&str>, json_spec: &str) -> Result<json::RemoveResult> {
    config::remove_from_json::<Component>(id, json_spec)
}

pub fn delete(id: &str) -> Result<()> {
    config::delete::<Component>(id)
}

pub fn exists(id: &str) -> bool {
    config::exists::<Component>(id)
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
// CLI Entry Points - Accept Option<T> and handle validation
// ============================================================================

#[derive(Debug, Clone)]
pub struct CreateResult {
    pub id: String,
    pub component: Component,
}

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

pub fn create_from_cli(
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
    version_targets: Vec<String>,
    build_command: Option<String>,
    extract_command: Option<String>,
) -> Result<CreateResult> {
    let local_path = local_path.ok_or_else(|| {
        Error::validation_invalid_argument(
            "local_path",
            "Missing required argument: --local-path (or use --json)",
            None,
            None,
        )
    })?;

    let remote_path = remote_path.ok_or_else(|| {
        Error::validation_invalid_argument(
            "remote_path",
            "Missing required argument: --remote-path (or use --json)",
            None,
            None,
        )
    })?;

    let build_artifact = build_artifact.ok_or_else(|| {
        Error::validation_invalid_argument(
            "build_artifact",
            "Missing required argument: --build-artifact (or use --json)",
            None,
            None,
        )
    })?;

    // ID is derived from local_path basename (lowercased)
    let id = slugify::extract_component_id(&local_path)?;
    if exists(&id) {
        return Err(Error::validation_invalid_argument(
            "component.local_path",
            format!("Component '{}' already exists", id),
            Some(id),
            None,
        ));
    }

    let expanded_path = shellexpand::tilde(&local_path).to_string();

    let mut component = Component::new(id.clone(), expanded_path, remote_path, build_artifact);
    if !version_targets.is_empty() {
        component.version_targets = Some(parse_version_targets(&version_targets)?);
    }
    component.build_command = build_command;
    component.extract_command = extract_command;

    save(&component)?;

    Ok(CreateResult { id, component })
}

pub fn update(
    component_id: &str,
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
    build_command: Option<Option<String>>,
    extract_command: Option<Option<String>>,
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
        component.build_artifact = new_build_artifact;
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

pub fn rename(id: &str, new_id: &str) -> Result<CreateResult> {
    let new_id = new_id.to_lowercase();
    config::rename::<Component>(id, &new_id)?;
    update_project_references(id, &new_id)?;
    let component = load(&new_id)?;
    Ok(CreateResult { id: new_id, component })
}

/// Update all projects that reference the old component ID to use the new ID.
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

/// Returns list of project IDs that reference the given component.
pub fn projects_using(component_id: &str) -> Result<Vec<String>> {
    let projects = project::list().unwrap_or_default();
    Ok(projects
        .iter()
        .filter(|p| p.component_ids.contains(&component_id.to_string()))
        .map(|p| p.id.clone())
        .collect())
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

// ============================================================================
// JSON Import
// ============================================================================

pub use config::BatchResult as CreateSummary;
pub use config::BatchResultItem as CreateSummaryItem;

pub fn create_from_json(spec: &str, skip_existing: bool) -> Result<CreateSummary> {
    config::create_from_json::<Component>(spec, skip_existing)
}

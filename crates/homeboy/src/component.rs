use crate::error::{Error, Result};
use crate::files::{self, FileSystem};
use crate::json;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedModuleConfig {
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    pub id: String,
    pub name: String,
    pub local_path: String,
    pub remote_path: String,
    pub build_artifact: String,

    #[serde(default)]
    pub modules: Vec<String>,

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
        name: String,
        local_path: String,
        remote_path: String,
        build_artifact: String,
    ) -> Self {
        Self {
            id,
            name,
            local_path,
            remote_path,
            build_artifact,
            modules: Vec::new(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentRecord {
    pub id: String,
    pub config: Component,
}

pub fn load(id: &str) -> Result<Component> {
    let path = paths::component(id)?;
    if !path.exists() {
        return Err(Error::component_not_found(id.to_string()));
    }
    let content = files::local().read(&path)?;
    json::from_str(&content)
}

pub fn list() -> Result<Vec<Component>> {
    let dir = paths::components()?;
    let entries = files::local().list(&dir)?;

    let mut components: Vec<Component> = entries
        .into_iter()
        .filter(|e| e.is_json() && !e.is_dir)
        .filter_map(|e| {
            let content = files::local().read(&e.path).ok()?;
            json::from_str(&content).ok()
        })
        .collect();
    components.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(components)
}

pub fn list_ids() -> Result<Vec<String>> {
    let dir = paths::components()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let entries = files::local().list(&dir)?;
    let mut ids: Vec<String> = entries
        .into_iter()
        .filter(|e| e.is_json() && !e.is_dir)
        .filter_map(|e| e.path.file_stem().map(|s| s.to_string_lossy().to_string()))
        .collect();
    ids.sort();
    Ok(ids)
}

pub fn save(component: &Component) -> Result<()> {
    let expected_id = slugify_id(&component.name)?;
    if expected_id != component.id {
        return Err(Error::config_invalid_value(
            "component.id",
            Some(component.id.clone()),
            format!(
                "Component id '{}' must match slug(name) '{}'. Use rename to change.",
                component.id, expected_id
            ),
        ));
    }

    let path = paths::component(&component.id)?;
    files::ensure_app_dirs()?;
    let content = json::to_string_pretty(component)?;
    files::local().write(&path, &content)?;
    Ok(())
}

pub fn delete(id: &str) -> Result<()> {
    let path = paths::component(id)?;
    if !path.exists() {
        return Err(Error::component_not_found(id.to_string()));
    }
    files::local().delete(&path)?;
    Ok(())
}

pub fn exists(id: &str) -> bool {
    paths::component(id).map(|p| p.exists()).unwrap_or(false)
}

pub fn slugify_id(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            "name",
            "Name cannot be empty",
            None,
            None,
        ));
    }

    let mut out = String::new();
    let mut prev_was_dash = false;

    for ch in trimmed.chars() {
        let normalized = match ch {
            'a'..='z' | '0'..='9' => Some(ch),
            'A'..='Z' => Some(ch.to_ascii_lowercase()),
            _ if ch.is_whitespace() || ch == '_' || ch == '-' => Some('-'),
            _ => None,
        };

        if let Some(c) = normalized {
            if c == '-' {
                if out.is_empty() || prev_was_dash {
                    continue;
                }
                out.push('-');
                prev_was_dash = true;
            } else {
                out.push(c);
                prev_was_dash = false;
            }
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        return Err(Error::validation_invalid_argument(
            "name",
            "Name must contain at least one letter or number",
            None,
            None,
        ));
    }

    Ok(out)
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

pub fn create_from_cli(
    name: Option<String>,
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
) -> Result<CreateResult> {
    let name = name.ok_or_else(|| {
        Error::validation_invalid_argument(
            "name",
            "Missing required argument: name",
            None,
            None,
        )
    })?;

    let local_path = local_path.ok_or_else(|| {
        Error::validation_invalid_argument(
            "local_path",
            "Missing required argument: local_path",
            None,
            None,
        )
    })?;

    let remote_path = remote_path.ok_or_else(|| {
        Error::validation_invalid_argument(
            "remote_path",
            "Missing required argument: remote_path",
            None,
            None,
        )
    })?;

    let build_artifact = build_artifact.ok_or_else(|| {
        Error::validation_invalid_argument(
            "build_artifact",
            "Missing required argument: build_artifact",
            None,
            None,
        )
    })?;

    let id = slugify_id(&name)?;
    let path = paths::component(&id)?;
    if path.exists() {
        return Err(Error::validation_invalid_argument(
            "component.name",
            format!("Component '{}' already exists", id),
            Some(id),
            None,
        ));
    }

    let component = Component::new(id.clone(), name, local_path, remote_path, build_artifact);
    save(&component)?;

    Ok(CreateResult { id, component })
}

pub fn update(
    component_id: &str,
    name: Option<String>,
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
    build_command: Option<Option<String>>,
    extract_command: Option<Option<String>>,
) -> Result<UpdateResult> {
    let mut component = load(component_id)?;
    let mut updated = Vec::new();

    if let Some(new_name) = name {
        let new_id = slugify_id(&new_name)?;
        if new_id != component_id {
            return Err(Error::validation_invalid_argument(
                "name",
                format!(
                    "Changing name would change id from '{}' to '{}'. Use rename command instead.",
                    component_id, new_id
                ),
                Some(new_name),
                None,
            ));
        }
        component.name = new_name;
        updated.push("name".to_string());
    }

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

pub fn rename(id: &str, new_name: &str) -> Result<CreateResult> {
    let mut component = load(id)?;
    let new_id = slugify_id(new_name)?;

    if new_id == id {
        component.name = new_name.to_string();
        save(&component)?;
        return Ok(CreateResult {
            id: new_id,
            component,
        });
    }

    let old_path = paths::component(id)?;
    let new_path = paths::component(&new_id)?;

    if new_path.exists() {
        return Err(Error::validation_invalid_argument(
            "component.name",
            format!(
                "Cannot rename component '{}' to '{}': destination already exists",
                id, new_id
            ),
            Some(new_id),
            None,
        ));
    }

    component.id = new_id.clone();
    component.name = new_name.to_string();

    files::ensure_app_dirs()?;
    std::fs::rename(&old_path, &new_path).map_err(|e| {
        Error::internal_io(e.to_string(), Some("rename component".to_string()))
    })?;

    if let Err(error) = save(&component) {
        let _ = std::fs::rename(&new_path, &old_path);
        return Err(error);
    }

    Ok(CreateResult {
        id: new_id,
        component,
    })
}

pub fn delete_with_validation(id: &str, force: bool) -> Result<()> {
    if !exists(id) {
        return Err(Error::component_not_found(id.to_string()));
    }

    if !force {
        let component = load(id)?;
        if !component.modules.is_empty() {
            return Err(Error::validation_invalid_argument(
                "component",
                format!(
                    "Component '{}' has {} module(s) configured. Use --force to delete anyway.",
                    id,
                    component.modules.len()
                ),
                Some(id.to_string()),
                None,
            ));
        }
    }

    delete(id)
}

// ============================================================================
// JSON Import
// ============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSummary {
    pub created: u32,
    pub skipped: u32,
    pub errors: u32,
    pub items: Vec<CreateSummaryItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSummaryItem {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn create_from_json(spec: &str, skip_existing: bool) -> Result<CreateSummary> {
    let value: serde_json::Value = json::from_str(spec)?;

    let items: Vec<serde_json::Value> = if value.is_array() {
        value.as_array().unwrap().clone()
    } else {
        vec![value]
    };

    let mut summary = CreateSummary {
        created: 0,
        skipped: 0,
        errors: 0,
        items: Vec::new(),
    };

    for item in items {
        let component: Component = match serde_json::from_value(item.clone()) {
            Ok(c) => c,
            Err(e) => {
                let id = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| slugify_id(n).unwrap_or_else(|_| "unknown".to_string()))
                    .unwrap_or_else(|| "unknown".to_string());

                summary.errors += 1;
                summary.items.push(CreateSummaryItem {
                    id,
                    status: "error".to_string(),
                    error: Some(format!("Parse error: {}", e)),
                });
                continue;
            }
        };

        let id = match slugify_id(&component.name) {
            Ok(id) => id,
            Err(e) => {
                summary.errors += 1;
                summary.items.push(CreateSummaryItem {
                    id: "unknown".to_string(),
                    status: "error".to_string(),
                    error: Some(e.message.clone()),
                });
                continue;
            }
        };

        if exists(&id) {
            if skip_existing {
                summary.skipped += 1;
                summary.items.push(CreateSummaryItem {
                    id,
                    status: "skipped".to_string(),
                    error: None,
                });
            } else {
                summary.errors += 1;
                summary.items.push(CreateSummaryItem {
                    id: id.clone(),
                    status: "error".to_string(),
                    error: Some(format!("Component '{}' already exists", id)),
                });
            }
            continue;
        }

        let component_with_id = Component {
            id: id.clone(),
            ..component
        };

        if let Err(e) = save(&component_with_id) {
            summary.errors += 1;
            summary.items.push(CreateSummaryItem {
                id,
                status: "error".to_string(),
                error: Some(e.message.clone()),
            });
            continue;
        }

        summary.created += 1;
        summary.items.push(CreateSummaryItem {
            id,
            status: "created".to_string(),
            error: None,
        });
    }

    Ok(summary)
}

use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::project;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Fleet {
    #[serde(skip_deserializing, default)]
    pub id: String,

    #[serde(default)]
    pub project_ids: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Fleet {
    pub fn new(id: String, project_ids: Vec<String>) -> Self {
        Self {
            id,
            project_ids,
            description: None,
        }
    }

    /// Returns project IDs that actually exist
    pub fn valid_project_ids(&self) -> Vec<String> {
        self.project_ids
            .iter()
            .filter(|id| project::exists(id))
            .cloned()
            .collect()
    }
}

impl ConfigEntity for Fleet {
    const ENTITY_TYPE: &'static str = "fleet";
    const DIR_NAME: &'static str = "fleets";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::fleet_not_found(id, suggestions)
    }
}

// ============================================================================
// Core CRUD - Thin wrappers around config module
// ============================================================================

pub fn load(id: &str) -> Result<Fleet> {
    config::load::<Fleet>(id)
}

pub fn list() -> Result<Vec<Fleet>> {
    config::list::<Fleet>()
}

pub fn list_ids() -> Result<Vec<String>> {
    config::list_ids::<Fleet>()
}

pub fn save(fleet: &Fleet) -> Result<()> {
    config::save(fleet)
}

pub fn delete(id: &str) -> Result<()> {
    config::delete::<Fleet>(id)
}

pub fn exists(id: &str) -> bool {
    config::exists::<Fleet>(id)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<Fleet>(id, json_spec, replace_fields)
}

pub fn remove_from_json(id: Option<&str>, json_spec: &str) -> Result<RemoveResult> {
    config::remove_from_json::<Fleet>(id, json_spec)
}

pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<Fleet>> {
    config::create::<Fleet>(json_spec, skip_existing)
}

// ============================================================================
// Operations
// ============================================================================

/// Add a project to a fleet
pub fn add_project(fleet_id: &str, project_id: &str) -> Result<Fleet> {
    let mut fleet = load(fleet_id)?;

    // Validate project exists
    if !project::exists(project_id) {
        let suggestions = config::find_similar_ids::<crate::project::Project>(project_id);
        return Err(Error::project_not_found(project_id, suggestions));
    }

    // Don't add duplicates
    if !fleet.project_ids.contains(&project_id.to_string()) {
        fleet.project_ids.push(project_id.to_string());
        save(&fleet)?;
    }

    Ok(fleet)
}

/// Remove a project from a fleet
pub fn remove_project(fleet_id: &str, project_id: &str) -> Result<Fleet> {
    let mut fleet = load(fleet_id)?;

    fleet.project_ids.retain(|id| id != project_id);
    save(&fleet)?;

    Ok(fleet)
}

/// Rename a fleet
pub fn rename(id: &str, new_id: &str) -> Result<Fleet> {
    let new_id = new_id.to_lowercase();
    config::rename::<Fleet>(id, &new_id)?;
    load(&new_id)
}

/// Get all projects in a fleet with full project data
pub fn get_projects(fleet_id: &str) -> Result<Vec<crate::project::Project>> {
    let fleet = load(fleet_id)?;
    let mut projects = Vec::new();

    for project_id in &fleet.project_ids {
        if let Ok(project) = project::load(project_id) {
            projects.push(project);
        }
    }

    Ok(projects)
}

/// Get component usage across a fleet (component_id -> Vec<project_id>)
pub fn component_usage(fleet_id: &str) -> Result<std::collections::HashMap<String, Vec<String>>> {
    let fleet = load(fleet_id)?;
    let mut usage: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for project_id in &fleet.project_ids {
        if let Ok(project) = project::load(project_id) {
            for component_id in &project.component_ids {
                usage
                    .entry(component_id.clone())
                    .or_default()
                    .push(project_id.clone());
            }
        }
    }

    Ok(usage)
}

// Fleet Sync was removed in #101 — it had OpenClaw-specific logic hardcoded
// in core, violating homeboy's platform-agnostic design. Use `homeboy deploy`
// to sync files across servers instead.


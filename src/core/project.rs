use crate::component::ScopedModuleConfig;
use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::server;
use crate::utils::slugify;
use crate::utils::parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents a nullable optional field in update operations.
///
/// Three-state semantics for CLI update commands:
/// - `None`: Field unchanged (flag not provided by user)
/// - `Some(None)`: Explicitly clear the field (user passed empty value)
/// - `Some(Some(T))`: Set to new value (user provided value)
///
/// Example: `--server-id ""` passes `Some(None)` to clear the association.
pub type NullableUpdate<T> = Option<Option<T>>;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct Project {
    #[serde(skip)]
    pub id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub modules: Option<HashMap<String, ScopedModuleConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_prefix: Option<String>,

    #[serde(default)]
    pub remote_files: RemoteFileConfig,
    #[serde(default)]
    pub remote_logs: RemoteLogConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub api: ApiConfig,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_next_section_label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_next_section_aliases: Option<Vec<String>>,

    #[serde(default)]
    pub sub_targets: Vec<SubTarget>,
    #[serde(default)]
    pub shared_tables: Vec<String>,
    #[serde(default)]
    pub component_ids: Vec<String>,
}

impl Project {
    pub fn has_sub_targets(&self) -> bool {
        !self.sub_targets.is_empty()
    }

    pub fn default_sub_target(&self) -> Option<&SubTarget> {
        self.sub_targets
            .iter()
            .find(|t| t.is_default)
            .or_else(|| self.sub_targets.first())
    }

    pub fn find_sub_target(&self, target_id: &str) -> Option<&SubTarget> {
        self.sub_targets
            .iter()
            .find(|t| slugify_id(&t.name).ok().as_deref() == Some(target_id))
    }
}

impl ConfigEntity for Project {
    const ENTITY_TYPE: &'static str = "project";
    const DIR_NAME: &'static str = "projects";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::project_not_found(id, suggestions)
    }

    fn validate(&self) -> Result<()> {
        if let Some(ref sid) = self.server_id {
            if !server::exists(sid) {
                let suggestions = config::find_similar_ids::<server::Server>(sid);
                return Err(Error::server_not_found(sid.clone(), suggestions));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct RemoteFileConfig {
    #[serde(default)]
    pub pinned_files: Vec<PinnedRemoteFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]

pub struct PinnedRemoteFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl PinnedRemoteFile {
    pub fn display_name(&self) -> &str {
        self.label
            .as_deref()
            .unwrap_or_else(|| self.path.rsplit('/').next().unwrap_or(&self.path))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct RemoteLogConfig {
    #[serde(default)]
    pub pinned_logs: Vec<PinnedRemoteLog>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]

pub struct PinnedRemoteLog {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "default_tail_lines")]
    pub tail_lines: u32,
}

fn default_tail_lines() -> u32 {
    100
}

impl PinnedRemoteLog {
    pub fn display_name(&self) -> &str {
        self.label
            .as_deref()
            .unwrap_or_else(|| self.path.rsplit('/').next().unwrap_or(&self.path))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinType {
    File,
    Log,
}

pub struct PinOptions {
    pub label: Option<String>,
    pub tail_lines: u32,
}

impl Default for PinOptions {
    fn default() -> Self {
        Self {
            label: None,
            tail_lines: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct DatabaseConfig {
    #[serde(default = "default_db_host")]
    pub host: String,
    #[serde(default = "default_db_port")]
    pub port: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub user: String,
    #[serde(default = "default_true")]
    pub use_ssh_tunnel: bool,
}

fn default_db_host() -> String {
    "localhost".to_string()
}

fn default_db_port() -> u16 {
    3306
}

fn default_true() -> bool {
    true
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            host: default_db_host(),
            port: default_db_port(),
            name: String::new(),
            user: String::new(),
            use_ssh_tunnel: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct ApiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct AuthConfig {
    pub header: String,
    #[serde(default)]
    pub variables: HashMap<String, VariableSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<AuthFlowConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<AuthFlowConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct VariableSource {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct AuthFlowConfig {
    pub endpoint: String,
    #[serde(default = "default_post_method")]
    pub method: String,
    #[serde(default)]
    pub body: HashMap<String, String>,
    #[serde(default)]
    pub store: HashMap<String, String>,
}

fn default_post_method() -> String {
    "POST".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct SubTarget {
    pub name: String,
    pub domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i32>,
    #[serde(default)]
    pub is_default: bool,
}

impl SubTarget {
    pub fn table_prefix(&self, base_prefix: &str) -> String {
        match self.number {
            Some(n) if n > 1 => format!("{}{}_", base_prefix, n),
            _ => base_prefix.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct ToolsConfig {
    #[serde(default)]
    pub bandcamp_scraper: BandcampScraperConfig,
    #[serde(default)]
    pub newsletter: NewsletterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct BandcampScraperConfig {
    #[serde(default)]
    pub default_tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct NewsletterConfig {
    #[serde(default)]
    pub sendy_list_id: String,
}

// ============================================================================
// Core CRUD - Thin wrappers around config module
// ============================================================================

pub fn load(id: &str) -> Result<Project> {
    config::load::<Project>(id)
}

pub fn list() -> Result<Vec<Project>> {
    config::list::<Project>()
}

pub fn list_ids() -> Result<Vec<String>> {
    config::list_ids::<Project>()
}

pub fn save(project: &Project) -> Result<()> {
    config::save(project)
}

pub fn delete(id: &str) -> Result<()> {
    config::delete::<Project>(id)
}

/// Delete a project with existence check.
/// Unlike server/component, projects have no dependents to check.
pub fn delete_safe(id: &str) -> Result<()> {
    if !exists(id) {
        let suggestions = config::find_similar_ids::<Project>(id);
        return Err(Error::project_not_found(id.to_string(), suggestions));
    }
    delete(id)
}

pub fn exists(id: &str) -> bool {
    config::exists::<Project>(id)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<Project>(id, json_spec, replace_fields)
}

pub fn remove_from_json(id: Option<&str>, json_spec: &str) -> Result<RemoveResult> {
    config::remove_from_json::<Project>(id, json_spec)
}

pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<Project>> {
    config::create::<Project>(json_spec, skip_existing)
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
    pub project: Project,
    pub updated_fields: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RenameResult {
    pub old_id: String,
    pub new_id: String,
    pub project: Project,
}

pub fn update(
    project_id: &str,
    domain: Option<String>,
    server_id: NullableUpdate<String>,
    base_path: NullableUpdate<String>,
    table_prefix: NullableUpdate<String>,
    component_ids: Option<Vec<String>>,
) -> Result<UpdateResult> {
    let mut project = load(project_id)?;
    let mut updated = Vec::new();

    if let Some(new_domain) = domain {
        project.domain = Some(new_domain);
        updated.push("domain".to_string());
    }

    if let Some(new_server_id) = server_id {
        if let Some(ref sid) = new_server_id {
            if !server::exists(sid) {
                let suggestions = config::find_similar_ids::<server::Server>(sid);
                return Err(Error::server_not_found(sid.clone(), suggestions));
            }
        }
        project.server_id = new_server_id;
        updated.push("server_id".to_string());
    }

    if let Some(new_base_path) = base_path {
        project.base_path = new_base_path;
        updated.push("base_path".to_string());
    }

    if let Some(new_table_prefix) = table_prefix {
        project.table_prefix = new_table_prefix;
        updated.push("table_prefix".to_string());
    }

    if let Some(new_component_ids) = component_ids {
        project.component_ids = new_component_ids;
        updated.push("component_ids".to_string());
    }

    save(&project)?;

    Ok(UpdateResult {
        id: project_id.to_string(),
        project,
        updated_fields: updated,
    })
}

pub fn rename(id: &str, new_id: &str) -> Result<RenameResult> {
    let new_id = new_id.to_lowercase();
    config::rename::<Project>(id, &new_id)?;
    let project = load(&new_id)?;
    Ok(RenameResult {
        old_id: id.to_string(),
        new_id,
        project,
    })
}

pub fn set_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    use crate::component;

    if component_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "At least one component ID is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut missing = Vec::new();
    for component_id in &component_ids {
        if !component::exists(component_id) {
            missing.push(component_id.clone());
        }
    }

    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "Unknown component IDs (must exist in `homeboy component list`)",
            Some(project_id.to_string()),
            Some(missing),
        ));
    }

    let deduped = parser::dedupe(component_ids);

    let mut project = load(project_id)?;
    project.component_ids = deduped.clone();
    save(&project)?;
    Ok(deduped)
}

pub fn add_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    use crate::component;

    if component_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "At least one component ID is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut missing = Vec::new();
    for component_id in &component_ids {
        if !component::exists(component_id) {
            missing.push(component_id.clone());
        }
    }

    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "Unknown component IDs (must exist in `homeboy component list`)",
            Some(project_id.to_string()),
            Some(missing),
        ));
    }

    let deduped = parser::dedupe(component_ids);

    let mut project = load(project_id)?;
    for id in deduped {
        if !project.component_ids.contains(&id) {
            project.component_ids.push(id);
        }
    }
    save(&project)?;
    Ok(project.component_ids)
}

pub fn remove_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    if component_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "At least one component ID is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut project = load(project_id)?;

    let mut missing = Vec::new();
    for id in &component_ids {
        if !project.component_ids.contains(id) {
            missing.push(id.clone());
        }
    }

    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "Component IDs not attached to project",
            Some(project_id.to_string()),
            Some(missing),
        ));
    }

    project
        .component_ids
        .retain(|id| !component_ids.contains(id));
    save(&project)?;
    Ok(project.component_ids)
}

pub fn clear_components(project_id: &str) -> Result<()> {
    let mut project = load(project_id)?;
    project.component_ids.clear();
    save(&project)?;
    Ok(())
}

pub fn pin(project_id: &str, pin_type: PinType, path: &str, options: PinOptions) -> Result<()> {
    let mut project = load(project_id)?;

    match pin_type {
        PinType::File => {
            if project
                .remote_files
                .pinned_files
                .iter()
                .any(|f| f.path == path)
            {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "File is already pinned",
                    Some(project_id.to_string()),
                    Some(vec![path.to_string()]),
                ));
            }
            project.remote_files.pinned_files.push(PinnedRemoteFile {
                path: path.to_string(),
                label: options.label,
            });
        }
        PinType::Log => {
            if project
                .remote_logs
                .pinned_logs
                .iter()
                .any(|l| l.path == path)
            {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "Log is already pinned",
                    Some(project_id.to_string()),
                    Some(vec![path.to_string()]),
                ));
            }
            project.remote_logs.pinned_logs.push(PinnedRemoteLog {
                path: path.to_string(),
                label: options.label,
                tail_lines: options.tail_lines,
            });
        }
    }

    save(&project)?;
    Ok(())
}

pub fn unpin(project_id: &str, pin_type: PinType, path: &str) -> Result<()> {
    let mut project = load(project_id)?;

    let (before, after, type_name) = match pin_type {
        PinType::File => {
            let before = project.remote_files.pinned_files.len();
            project.remote_files.pinned_files.retain(|f| f.path != path);
            (before, project.remote_files.pinned_files.len(), "file")
        }
        PinType::Log => {
            let before = project.remote_logs.pinned_logs.len();
            project.remote_logs.pinned_logs.retain(|l| l.path != path);
            (before, project.remote_logs.pinned_logs.len(), "log")
        }
    };

    if after == before {
        return Err(Error::validation_invalid_argument(
            "path",
            format!("{} is not pinned", type_name),
            Some(project_id.to_string()),
            Some(vec![path.to_string()]),
        ));
    }

    save(&project)?;
    Ok(())
}

use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::json;
use crate::local_files;
use crate::paths;
use crate::slugify;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(skip)]
    pub id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub scoped_modules: Option<HashMap<String, ScopedModuleConfig>>,

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
    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn config_path(id: &str) -> Result<PathBuf> {
        paths::project(id)
    }
    fn config_dir() -> Result<PathBuf> {
        paths::projects()
    }
    fn not_found_error(id: String) -> Error {
        Error::project_not_found(id)
    }
    fn entity_type() -> &'static str {
        "project"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScopedModuleConfig {
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileConfig {
    #[serde(default)]
    pub pinned_files: Vec<PinnedRemoteFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedRemoteFile {
    pub id: Uuid,
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
#[serde(rename_all = "camelCase")]
pub struct RemoteLogConfig {
    #[serde(default)]
    pub pinned_logs: Vec<PinnedRemoteLog>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedRemoteLog {
    pub id: Uuid,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct ApiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct VariableSource {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct ToolsConfig {
    #[serde(default)]
    pub bandcamp_scraper: BandcampScraperConfig,
    #[serde(default)]
    pub newsletter: NewsletterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BandcampScraperConfig {
    #[serde(default)]
    pub default_tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewsletterConfig {
    #[serde(default)]
    pub sendy_list_id: String,
}

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

pub fn exists(id: &str) -> bool {
    config::exists::<Project>(id)
}

/// Merge JSON into project config. Accepts JSON string, @file, or - for stdin.
/// ID can be provided as argument or extracted from JSON body.
pub fn merge_from_json(id: Option<&str>, json_spec: &str) -> Result<json::MergeResult> {
    config::merge_from_json::<Project>(id, json_spec)
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
    pub project: Project,
}

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

pub fn create_from_cli(
    id: Option<String>,
    domain: Option<String>,
    server_id: Option<String>,
    base_path: Option<String>,
    table_prefix: Option<String>,
) -> Result<CreateResult> {
    let id = id.ok_or_else(|| {
        Error::validation_invalid_argument("id", "Missing required argument: id", None, None)
    })?;

    slugify::validate_component_id(&id)?;

    if exists(&id) {
        return Err(Error::validation_invalid_argument(
            "project.id",
            format!("Project '{}' already exists", id),
            Some(id),
            None,
        ));
    }

    let project = Project {
        id: id.clone(),
        domain,
        scoped_modules: None,
        server_id,
        base_path,
        table_prefix,
        remote_files: Default::default(),
        remote_logs: Default::default(),
        database: Default::default(),
        tools: Default::default(),
        api: Default::default(),
        changelog_next_section_label: None,
        changelog_next_section_aliases: None,
        sub_targets: Default::default(),
        shared_tables: Default::default(),
        component_ids: Default::default(),
    };

    save(&project)?;

    Ok(CreateResult { id, project })
}

pub fn update(
    project_id: &str,
    domain: Option<String>,
    server_id: Option<Option<String>>,
    base_path: Option<Option<String>>,
    table_prefix: Option<Option<String>>,
    component_ids: Option<Vec<String>>,
) -> Result<UpdateResult> {
    let mut project = load(project_id)?;
    let mut updated = Vec::new();

    if let Some(new_domain) = domain {
        project.domain = Some(new_domain);
        updated.push("domain".to_string());
    }

    if let Some(new_server_id) = server_id {
        project.server_id = new_server_id;
        updated.push("serverId".to_string());
    }

    if let Some(new_base_path) = base_path {
        project.base_path = new_base_path;
        updated.push("basePath".to_string());
    }

    if let Some(new_table_prefix) = table_prefix {
        project.table_prefix = new_table_prefix;
        updated.push("tablePrefix".to_string());
    }

    if let Some(new_component_ids) = component_ids {
        project.component_ids = new_component_ids;
        updated.push("componentIds".to_string());
    }

    save(&project)?;

    Ok(UpdateResult {
        id: project_id.to_string(),
        project,
        updated_fields: updated,
    })
}

pub fn rename(id: &str, new_id: &str) -> Result<RenameResult> {
    let mut project = load(id)?;

    slugify::validate_component_id(new_id)?;

    if new_id == id {
        return Ok(RenameResult {
            old_id: id.to_string(),
            new_id: new_id.to_string(),
            project,
        });
    }

    let old_path = paths::project(id)?;
    let new_path = paths::project(new_id)?;

    if new_path.exists() {
        return Err(Error::validation_invalid_argument(
            "project.id",
            format!(
                "Cannot rename project '{}' to '{}': destination already exists",
                id, new_id
            ),
            Some(new_id.to_string()),
            None,
        ));
    }

    project.id = new_id.to_string();

    local_files::ensure_app_dirs()?;
    std::fs::rename(&old_path, &new_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("rename project".to_string())))?;

    if let Err(error) = save(&project) {
        let _ = std::fs::rename(&new_path, &old_path);
        return Err(error);
    }

    Ok(RenameResult {
        old_id: id.to_string(),
        new_id: new_id.to_string(),
        project,
    })
}

pub fn validate_component_ids(component_ids: Vec<String>, project_id: &str) -> Result<Vec<String>> {
    use crate::component;
    use std::collections::HashSet;

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

    let mut seen = HashSet::new();
    let deduped: Vec<String> = component_ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();

    Ok(deduped)
}

pub fn set_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    use crate::component;
    use std::collections::HashSet;

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

    let mut seen = HashSet::new();
    let deduped: Vec<String> = component_ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();

    let mut project = load(project_id)?;
    project.component_ids = deduped.clone();
    save(&project)?;
    Ok(deduped)
}

pub fn add_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    let mut project = load(project_id)?;
    for id in component_ids {
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

pub fn pin_file(project_id: &str, path: &str, label: Option<String>) -> Result<PinnedRemoteFile> {
    let mut project = load(project_id)?;

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

    let pinned = PinnedRemoteFile {
        id: Uuid::new_v4(),
        path: path.to_string(),
        label,
    };
    project.remote_files.pinned_files.push(pinned.clone());
    save(&project)?;
    Ok(pinned)
}

pub fn pin_log(
    project_id: &str,
    path: &str,
    label: Option<String>,
    tail_lines: u32,
) -> Result<PinnedRemoteLog> {
    let mut project = load(project_id)?;

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

    let pinned = PinnedRemoteLog {
        id: Uuid::new_v4(),
        path: path.to_string(),
        label,
        tail_lines,
    };
    project.remote_logs.pinned_logs.push(pinned.clone());
    save(&project)?;
    Ok(pinned)
}

pub fn unpin_file(project_id: &str, file_id: &str) -> Result<()> {
    let uuid = Uuid::parse_str(file_id).map_err(|_| {
        Error::validation_invalid_argument(
            "file_id",
            "Invalid file ID format",
            Some(file_id.to_string()),
            None,
        )
    })?;

    let mut project = load(project_id)?;
    let before = project.remote_files.pinned_files.len();
    project.remote_files.pinned_files.retain(|f| f.id != uuid);

    if project.remote_files.pinned_files.len() == before {
        return Err(Error::validation_invalid_argument(
            "file_id",
            "Pinned file not found",
            Some(file_id.to_string()),
            None,
        ));
    }

    save(&project)?;
    Ok(())
}

pub fn unpin_log(project_id: &str, log_id: &str) -> Result<()> {
    let uuid = Uuid::parse_str(log_id).map_err(|_| {
        Error::validation_invalid_argument(
            "log_id",
            "Invalid log ID format",
            Some(log_id.to_string()),
            None,
        )
    })?;

    let mut project = load(project_id)?;
    let before = project.remote_logs.pinned_logs.len();
    project.remote_logs.pinned_logs.retain(|l| l.id != uuid);

    if project.remote_logs.pinned_logs.len() == before {
        return Err(Error::validation_invalid_argument(
            "log_id",
            "Pinned log not found",
            Some(log_id.to_string()),
            None,
        ));
    }

    save(&project)?;
    Ok(())
}

pub fn unpin_file_by_path(project_id: &str, path: &str) -> Result<()> {
    let mut project = load(project_id)?;
    let before = project.remote_files.pinned_files.len();
    project.remote_files.pinned_files.retain(|f| f.path != path);

    if project.remote_files.pinned_files.len() == before {
        return Err(Error::validation_invalid_argument(
            "path",
            "file is not pinned",
            Some(project_id.to_string()),
            Some(vec![path.to_string()]),
        ));
    }

    save(&project)?;
    Ok(())
}

pub fn unpin_log_by_path(project_id: &str, path: &str) -> Result<()> {
    let mut project = load(project_id)?;
    let before = project.remote_logs.pinned_logs.len();
    project.remote_logs.pinned_logs.retain(|l| l.path != path);

    if project.remote_logs.pinned_logs.len() == before {
        return Err(Error::validation_invalid_argument(
            "path",
            "log is not pinned",
            Some(project_id.to_string()),
            Some(vec![path.to_string()]),
        ));
    }

    save(&project)?;
    Ok(())
}

// ============================================================================
// JSON Import
// ============================================================================

pub use config::BatchResult as CreateSummary;
pub use config::BatchResultItem as CreateSummaryItem;

pub fn create_from_json(spec: &str, skip_existing: bool) -> Result<CreateSummary> {
    config::create_from_json::<Project>(spec, skip_existing)
}

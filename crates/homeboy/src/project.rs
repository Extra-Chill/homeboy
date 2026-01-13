use crate::error::{Error, Result};
use crate::files::{self, FileSystem};
use crate::json;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    pub id: String,
    pub config: Project,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub name: String,
    pub domain: String,
    #[serde(default)]
    pub modules: Vec<String>,

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
    pub fn has_module(&self, module_id: &str) -> bool {
        self.modules.contains(&module_id.to_string())
    }

    pub fn has_sub_targets(&self) -> bool {
        !self.sub_targets.is_empty()
    }

    pub fn default_sub_target(&self) -> Option<&SubTarget> {
        self.sub_targets
            .iter()
            .find(|t| t.is_default)
            .or_else(|| self.sub_targets.first())
    }

    pub fn find_sub_target(&self, id: &str) -> Option<&SubTarget> {
        self.sub_targets
            .iter()
            .find(|t| slugify_id(&t.name).ok().as_deref() == Some(id))
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
    let record = load_record(id)?;
    Ok(record.config)
}

pub fn load_record(id: &str) -> Result<ProjectRecord> {
    let path = paths::project(id)?;
    if !path.exists() {
        return Err(Error::project_not_found(id.to_string()));
    }
    let content = files::local().read(&path)?;
    let config: Project = json::from_str(&content)?;

    let expected_id = slugify_id(&config.name)?;
    if expected_id != id {
        return Err(Error::config_invalid_value(
            "project.id",
            Some(id.to_string()),
            format!(
                "Project configuration mismatch: file '{}' implies id '{}', but name '{}' implies id '{}'. Run `homeboy project repair {}`.",
                path.display(),
                id,
                config.name,
                expected_id,
                id
            ),
        ));
    }

    Ok(ProjectRecord {
        id: id.to_string(),
        config,
    })
}

pub fn list() -> Result<Vec<ProjectRecord>> {
    let dir = paths::projects()?;
    let entries = files::local().list(&dir)?;

    let mut projects: Vec<ProjectRecord> = entries
        .into_iter()
        .filter(|e| e.is_json() && !e.is_dir)
        .filter_map(|e| {
            let content = files::local().read(&e.path).ok()?;
            let config: Project = json::from_str(&content).ok()?;
            let id = e.path.file_stem()?.to_string_lossy().to_string();
            let expected_id = slugify_id(&config.name).ok()?;
            if expected_id != id {
                return None;
            }
            Some(ProjectRecord { id, config })
        })
        .collect();
    projects.sort_by(|a, b| a.config.name.cmp(&b.config.name));
    Ok(projects)
}

pub fn save(id: &str, project: &Project) -> Result<()> {
    let expected_id = slugify_id(&project.name)?;
    if expected_id != id {
        return Err(Error::config_invalid_value(
            "project.id",
            Some(id.to_string()),
            format!(
                "Project id '{}' must match slug(name) '{}'. Use `homeboy project set {id} --name \"{}\"` to rename.",
                id, expected_id, project.name
            ),
        ));
    }

    let path = paths::project(id)?;
    files::ensure_app_dirs()?;
    let content = json::to_string_pretty(project)?;
    files::local().write(&path, &content)?;
    Ok(())
}

pub fn delete(id: &str) -> Result<()> {
    let path = paths::project(id)?;
    if !path.exists() {
        return Err(Error::project_not_found(id.to_string()));
    }
    files::local().delete(&path)?;
    Ok(())
}

pub fn exists(id: &str) -> bool {
    paths::project(id).map(|p| p.exists()).unwrap_or(false)
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
    name: Option<String>,
    domain: Option<String>,
    modules: Vec<String>,
    server_id: Option<String>,
    base_path: Option<String>,
    table_prefix: Option<String>,
) -> Result<CreateResult> {
    let name = name.ok_or_else(|| {
        Error::validation_invalid_argument(
            "name",
            "Missing required argument: name",
            None,
            None,
        )
    })?;

    let domain = domain.ok_or_else(|| {
        Error::validation_invalid_argument(
            "domain",
            "Missing required argument: domain",
            None,
            None,
        )
    })?;

    let id = slugify_id(&name)?;
    let path = paths::project(&id)?;
    if path.exists() {
        return Err(Error::validation_invalid_argument(
            "project.name",
            format!("Project '{}' already exists", id),
            Some(id),
            None,
        ));
    }

    let project = Project {
        name,
        domain,
        modules,
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

    save(&id, &project)?;

    Ok(CreateResult { id, project })
}

pub fn update(
    project_id: &str,
    name: Option<String>,
    domain: Option<String>,
    modules: Option<Vec<String>>,
    server_id: Option<Option<String>>,
    base_path: Option<Option<String>>,
    table_prefix: Option<Option<String>>,
    component_ids: Option<Vec<String>>,
) -> Result<UpdateResult> {
    let mut project = load(project_id)?;
    let mut updated = Vec::new();

    if let Some(new_name) = name {
        project.name = new_name;
        updated.push("name".to_string());
    }

    if let Some(new_domain) = domain {
        project.domain = new_domain;
        updated.push("domain".to_string());
    }

    if let Some(new_modules) = modules {
        project.modules = new_modules;
        updated.push("modules".to_string());
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

    let new_id = slugify_id(&project.name)?;
    if new_id != project_id {
        rename(project_id, &project.name)?;
    } else {
        save(project_id, &project)?;
    }

    Ok(UpdateResult {
        id: new_id,
        project,
        updated_fields: updated,
    })
}

pub fn rename(id: &str, new_name: &str) -> Result<RenameResult> {
    let mut project = load(id)?;
    project.name = new_name.to_string();

    let new_id = slugify_id(&project.name)?;
    if new_id == id {
        save(id, &project)?;
        return Ok(RenameResult {
            old_id: id.to_string(),
            new_id,
            project,
        });
    }

    let old_path = paths::project(id)?;
    let new_path = paths::project(&new_id)?;

    if new_path.exists() {
        return Err(Error::validation_invalid_argument(
            "project.name",
            format!(
                "Cannot rename project '{}' to '{}': destination already exists",
                id, new_id
            ),
            Some(new_id),
            None,
        ));
    }

    files::ensure_app_dirs()?;
    std::fs::rename(&old_path, &new_path).map_err(|e| {
        Error::internal_io(e.to_string(), Some("rename project".to_string()))
    })?;

    if let Err(error) = save(&new_id, &project) {
        let _ = std::fs::rename(&new_path, &old_path);
        return Err(error);
    }

    Ok(RenameResult {
        old_id: id.to_string(),
        new_id,
        project,
    })
}

pub fn repair(id: &str) -> Result<RenameResult> {
    let path = paths::project(id)?;
    if !path.exists() {
        return Err(Error::project_not_found(id.to_string()));
    }

    let content = files::local().read(&path)?;
    let project: Project = json::from_str(&content)?;
    let expected_id = slugify_id(&project.name)?;

    if expected_id == id {
        return Ok(RenameResult {
            old_id: id.to_string(),
            new_id: id.to_string(),
            project,
        });
    }

    let new_path = paths::project(&expected_id)?;
    if new_path.exists() {
        return Err(Error::validation_invalid_argument(
            "project.name",
            format!(
                "Cannot repair project '{}' to '{}': destination already exists",
                id, expected_id
            ),
            Some(expected_id),
            None,
        ));
    }

    files::ensure_app_dirs()?;
    std::fs::rename(&path, &new_path).map_err(|e| {
        Error::internal_io(e.to_string(), Some("repair project rename".to_string()))
    })?;

    Ok(RenameResult {
        old_id: id.to_string(),
        new_id: expected_id,
        project,
    })
}

pub fn set_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    let mut project = load(project_id)?;
    project.component_ids = component_ids.clone();
    save(project_id, &project)?;
    Ok(component_ids)
}

pub fn add_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    let mut project = load(project_id)?;
    for id in component_ids {
        if !project.component_ids.contains(&id) {
            project.component_ids.push(id);
        }
    }
    save(project_id, &project)?;
    Ok(project.component_ids)
}

pub fn remove_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    let mut project = load(project_id)?;
    project.component_ids.retain(|id| !component_ids.contains(id));
    save(project_id, &project)?;
    Ok(project.component_ids)
}

pub fn clear_components(project_id: &str) -> Result<()> {
    let mut project = load(project_id)?;
    project.component_ids.clear();
    save(project_id, &project)?;
    Ok(())
}

pub fn pin_file(project_id: &str, path: &str, label: Option<String>) -> Result<PinnedRemoteFile> {
    let mut project = load(project_id)?;
    let pinned = PinnedRemoteFile {
        id: Uuid::new_v4(),
        path: path.to_string(),
        label,
    };
    project.remote_files.pinned_files.push(pinned.clone());
    save(project_id, &project)?;
    Ok(pinned)
}

pub fn pin_log(
    project_id: &str,
    path: &str,
    label: Option<String>,
    tail_lines: u32,
) -> Result<PinnedRemoteLog> {
    let mut project = load(project_id)?;
    let pinned = PinnedRemoteLog {
        id: Uuid::new_v4(),
        path: path.to_string(),
        label,
        tail_lines,
    };
    project.remote_logs.pinned_logs.push(pinned.clone());
    save(project_id, &project)?;
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

    save(project_id, &project)?;
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

    save(project_id, &project)?;
    Ok(())
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
        let project: Project = match serde_json::from_value(item.clone()) {
            Ok(p) => p,
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

        let id = match slugify_id(&project.name) {
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
                    error: Some(format!("Project '{}' already exists", id)),
                });
            }
            continue;
        }

        if let Err(e) = save(&id, &project) {
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

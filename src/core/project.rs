use crate::component::ScopedExtensionConfig;
use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::server;
use crate::utils::parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentAttachment {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_deploy: Option<crate::component::GitDeployConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hooks: HashMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<crate::component::ScopeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct Project {
    #[serde(skip)]
    pub id: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<HashMap<String, ScopedExtensionConfig>>,

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ProjectComponentAttachment>,
    #[serde(default)]
    pub component_ids: Vec<String>,
    /// Per-component field overrides applied when a component runs in this project.
    ///
    /// Example: `{"data-machine": {"extract_command": "...", "remote_owner": "opencode:opencode"}}`
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub component_overrides: HashMap<String, ProjectComponentOverrides>,

    /// Service names to check in fleet health status (e.g. ["kimaki", "php8.4-fpm", "nginx"]).
    /// These are checked via `systemctl is-active <name>` on the remote server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
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
    fn aliases(&self) -> &[String] {
        &self.aliases
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

pub(crate) fn default_true() -> bool {
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
// Core CRUD - Generated by entity_crud! macro
// ============================================================================

entity_crud!(Project; list_ids, merge, slugify_id);

// ============================================================================
// Operations
// ============================================================================

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
    project.components = deduped
        .iter()
        .map(|id| ProjectComponentAttachment {
            id: id.clone(),
            local_path: project
                .components
                .iter()
                .find(|component| &component.id == id)
                .and_then(|component| component.local_path.clone()),
        })
        .collect();
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
            project.component_ids.push(id.clone());
            project.components.push(ProjectComponentAttachment {
                id,
                local_path: None,
            });
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
    project
        .components
        .retain(|component| !component_ids.contains(&component.id));
    save(&project)?;
    Ok(project.component_ids)
}

pub fn attach_component_path(project_id: &str, component_id: &str, local_path: &str) -> Result<()> {
    let mut project = load(project_id)?;

    if let Some(component) = project.components.iter_mut().find(|c| c.id == component_id) {
        component.local_path = Some(local_path.to_string());
    } else {
        project.components.push(ProjectComponentAttachment {
            id: component_id.to_string(),
            local_path: Some(local_path.to_string()),
        });
        if !project.component_ids.contains(&component_id.to_string()) {
            project.component_ids.push(component_id.to_string());
        }
    }

    save(&project)
}

pub fn apply_component_overrides(
    component: &crate::component::Component,
    project: &Project,
) -> crate::component::Component {
    let Some(overrides) = project.component_overrides.get(&component.id) else {
        return component.clone();
    };

    let mut merged = component.clone();

    if let Some(build_artifact) = &overrides.build_artifact {
        merged.build_artifact = Some(build_artifact.clone());
    }
    if let Some(extract_command) = &overrides.extract_command {
        merged.extract_command = Some(extract_command.clone());
    }
    if let Some(remote_owner) = &overrides.remote_owner {
        merged.remote_owner = Some(remote_owner.clone());
    }
    if let Some(deploy_strategy) = &overrides.deploy_strategy {
        merged.deploy_strategy = Some(deploy_strategy.clone());
    }
    if let Some(git_deploy) = &overrides.git_deploy {
        merged.git_deploy = Some(git_deploy.clone());
    }
    if !overrides.hooks.is_empty() {
        merged.hooks = overrides.hooks.clone();
    }
    if let Some(scopes) = &overrides.scopes {
        merged.scopes = Some(scopes.clone());
    }

    merged
}

pub fn resolve_project_component(
    project: &Project,
    component_id: &str,
) -> Result<crate::component::Component> {
    let component = if let Some(attachment) = project
        .components
        .iter()
        .find(|component| component.id == component_id)
    {
        if let Some(local_path) = &attachment.local_path {
            crate::component::discover_from_portable(std::path::Path::new(local_path)).ok_or_else(
                || {
                    Error::validation_invalid_argument(
                        "components.local_path",
                        format!(
                            "Project component '{}' points to '{}' but no homeboy.json was found",
                            component_id, local_path
                        ),
                        Some(project.id.clone()),
                        None,
                    )
                },
            )?
        } else {
            crate::component::load(component_id)?
        }
    } else {
        crate::component::load(component_id)?
    };
    Ok(apply_component_overrides(&component, project))
}

pub fn resolve_project_components(project: &Project) -> Result<Vec<crate::component::Component>> {
    project
        .component_ids
        .iter()
        .map(|component_id| resolve_project_component(project, component_id))
        .collect()
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

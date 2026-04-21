use crate::component::ScopedExtensionConfig;
use crate::config::{self, ConfigEntity};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::paths;
use crate::server;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub mod component;
pub mod files;
pub mod logs;
pub mod pins;
mod readiness;
pub mod report;
mod status;

pub use component::{
    apply_component_overrides, attach_component_path, attach_component_path_report,
    attach_discovered_component_path, clear_component_attachments, clear_components, has_component,
    list_components, project_component_ids, remove_components, remove_components_report,
    resolve_project_component, resolve_project_components, set_component_attachments,
    set_components, ProjectComponentsOutput,
};
pub use files::{FileEntry, GrepMatch, LineChange};
pub use logs::{LogContent, LogEntry, LogSearchResult, PinnedLogsContent};
pub use pins::{
    add_pin, list_pins, remove_pin, ProjectPinChange, ProjectPinListItem, ProjectPinOutput,
};
pub use readiness::calculate_deploy_readiness;
pub use report::{
    build_components_output, build_create_output, build_delete_output, build_init_output,
    build_list_output, build_pin_output, build_remove_output, build_rename_output,
    build_set_output, build_show_output, build_status_output, list_report, show_report,
    status_report, ProjectComponentVersion, ProjectListItem, ProjectListReport, ProjectReportExtra,
    ProjectReportOutput, ProjectShowReport, ProjectStatusReport,
};
pub use status::{collect_status, ProjectComponentStatus, ProjectStatusSnapshot};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentAttachment {
    pub id: String,
    pub local_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
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
    /// Override the CLI path used by extension deploy install steps.
    /// For example, Studio sites need "studio wp" instead of the default "wp".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<String>,
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

    /// Project-scoped CLI path used by extension deploy install steps.
    ///
    /// On any given site the WP-CLI entrypoint is fixed (`wp`, `studio wp`,
    /// a Lando wrapper, etc.) and shared by every component deployed there,
    /// so this lives at the project layer. Component-level
    /// `ProjectComponentOverrides::cli_path` still wins as the most-specific
    /// escape hatch.
    ///
    /// If unset, the deploy resolver also auto-detects Studio sites
    /// (projects whose `base_path` is under `~/Studio/`) and defaults them
    /// to `"studio wp"`. See `cli_path_for_project()` for the full cascade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<String>,

    #[serde(default)]
    pub sub_targets: Vec<SubTarget>,
    #[serde(default)]
    pub shared_tables: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ProjectComponentAttachment>,
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

    /// Directory-based config: `~/.config/homeboy/projects/{id}/{id}.json`.
    ///
    /// Falls back to legacy flat file `~/.config/homeboy/projects/{id}.json`
    /// if the directory-based path doesn't exist yet. This allows transparent
    /// migration — existing projects keep working, new projects use directories.
    fn config_path(id: &str) -> Result<PathBuf> {
        let dir_path = paths::project_config(id)?;
        if dir_path.exists() {
            return Ok(dir_path);
        }

        // Check for legacy flat file
        let flat_path = Self::config_dir()?.join(format!("{}.json", id));
        if flat_path.exists() {
            return Ok(flat_path);
        }

        // Default to directory-based for new projects
        Ok(dir_path)
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
// Project directory operations
// ============================================================================

/// Initialize a project directory at `~/.config/homeboy/projects/{id}/`.
///
/// Creates the directory structure and an initial `{id}.json` config file.
/// If the project already exists as a flat file, migrates it to directory form.
pub fn init_project_dir(id: &str) -> Result<PathBuf> {
    let dir = paths::project_dir(id)?;
    let config_path = paths::project_config(id)?;

    // If directory config already exists, nothing to do
    if config_path.exists() {
        return Err(Error::validation_invalid_argument(
            "id",
            format!("Project directory '{}' already exists", id),
            Some(id.to_string()),
            None,
        ));
    }

    // Check if a flat-file project exists that should be migrated
    let flat_path = paths::projects()?.join(format!("{}.json", id));
    if flat_path.exists() {
        return migrate_to_directory(id);
    }

    // Check the project exists in the registry
    if !exists(id) {
        return Err(Error::validation_invalid_argument(
            "id",
            format!(
                "Project '{}' does not exist. Create it first with `homeboy project create`",
                id
            ),
            Some(id.to_string()),
            None,
        ));
    }

    // Load, then re-save — save() now creates the directory via config_path()
    let project = load(id)?;
    // Delete the old flat file if it exists
    if flat_path.exists() {
        let _ = std::fs::remove_file(&flat_path);
    }
    // Force the directory path for the new save
    local_files::local().ensure_dir(&dir)?;
    let content = config::to_string_pretty(&project)?;
    local_files::local().write(&config_path, &content)?;

    Ok(dir)
}

/// Migrate a project from flat file `{id}.json` to directory `{id}/{id}.json`.
fn migrate_to_directory(id: &str) -> Result<PathBuf> {
    let flat_path = paths::projects()?.join(format!("{}.json", id));
    let dir = paths::project_dir(id)?;
    let config_path = paths::project_config(id)?;

    // Create the project directory
    local_files::local().ensure_dir(&dir)?;

    // Move the flat file into the directory with the correct name
    std::fs::rename(&flat_path, &config_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("migrate project '{}' to directory", id)),
        )
    })?;

    Ok(dir)
}

/// Check if a project is using the directory-based config layout.
pub fn is_directory_based(id: &str) -> bool {
    paths::project_config(id)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Check if a project is still using the legacy flat-file layout.
pub fn needs_directory_migration(id: &str) -> bool {
    let flat_exists = paths::projects()
        .map(|p| p.join(format!("{}.json", id)).exists())
        .unwrap_or(false);
    flat_exists && !is_directory_based(id)
}

/// Migrate all flat-file projects to directory-based layout.
///
/// Called during `homeboy upgrade` to transparently move projects from
/// `projects/{id}.json` to `projects/{id}/{id}.json`. Returns a list
/// of (project_id, success) tuples.
pub fn migrate_all_to_directories() -> Vec<(String, bool, String)> {
    let project_ids = match list_ids() {
        Ok(ids) => ids,
        Err(_) => return vec![],
    };

    let mut results = Vec::new();

    for id in &project_ids {
        if !needs_directory_migration(id) {
            continue;
        }

        match migrate_to_directory(id) {
            Ok(dir) => {
                results.push((id.clone(), true, format!("migrated to {}", dir.display())));
            }
            Err(e) => {
                results.push((id.clone(), false, e.message.clone()));
            }
        }
    }

    results
}

/// Get the project directory path for a given project ID.
/// Returns the directory path regardless of whether the project uses
/// directory-based or flat-file config.
pub fn project_dir_path(id: &str) -> Result<PathBuf> {
    paths::project_dir(id)
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

// ============================================================================
// CLI path resolution
// ============================================================================

/// Detect whether a `base_path` lives under a given Studio root directory.
///
/// Pure helper — accepts the Studio root explicitly so tests don't have to
/// mutate `$HOME` (which races under cargo's parallel test runner).
///
/// Studio installs sites under `~/Studio/<site>/`. Tilde and env vars in
/// `base_path` are expanded before the prefix check so configs that store
/// `~/Studio/foo` still match.
fn base_path_is_under_studio(base_path: Option<&str>, studio_root: &str) -> bool {
    let raw = match base_path {
        Some(p) if !p.is_empty() => p,
        _ => return false,
    };
    if studio_root.is_empty() {
        return false;
    }

    let expanded = shellexpand::full(raw)
        .map(|cow| cow.into_owned())
        .unwrap_or_else(|_| raw.to_string());

    let normalized_root = if studio_root.ends_with('/') {
        studio_root.to_string()
    } else {
        format!("{}/", studio_root)
    };

    expanded.starts_with(&normalized_root)
}

/// Detect whether a project's `base_path` looks like a Studio-managed site.
///
/// Reads the user's home directory at runtime to compute the Studio root.
/// If `$HOME` is unset we conservatively return `false`.
pub fn is_studio_project(base_path: Option<&str>) -> bool {
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => h,
        _ => return false,
    };
    let studio_root = format!("{}/Studio", home.trim_end_matches('/'));
    base_path_is_under_studio(base_path, &studio_root)
}

/// Resolve the project-scoped CLI path with auto-detection fallbacks.
///
/// Cascade (highest → lowest precedence):
///   1. Explicit `Project::cli_path` (operator override)
///   2. Studio auto-detect: returns `"studio wp"` when `base_path` lives
///      under `~/Studio/`
///   3. `None` (caller falls back to component override → extension default → `"wp"`)
///
/// Component-level overrides (`ProjectComponentOverrides::cli_path`) are
/// applied earlier in the cascade by `apply_component_overrides()`, so this
/// function only needs to handle the project rung.
pub fn project_cli_path(project: &Project) -> Option<String> {
    if let Some(explicit) = &project.cli_path {
        return Some(explicit.clone());
    }
    if is_studio_project(project.base_path.as_deref()) {
        return Some("studio wp".to_string());
    }
    None
}

#[cfg(test)]
mod cli_path_tests {
    use super::*;

    fn project_with(base_path: Option<&str>, cli_path: Option<&str>) -> Project {
        Project {
            id: "test".to_string(),
            base_path: base_path.map(|s| s.to_string()),
            cli_path: cli_path.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn base_path_under_studio_root_matches() {
        assert!(base_path_is_under_studio(
            Some("/Users/chubes/Studio/my-site"),
            "/Users/chubes/Studio"
        ));
    }

    #[test]
    fn base_path_outside_studio_root_does_not_match() {
        assert!(!base_path_is_under_studio(
            Some("/var/www/my-site"),
            "/Users/chubes/Studio"
        ));
    }

    #[test]
    fn empty_or_missing_base_path_does_not_match() {
        assert!(!base_path_is_under_studio(None, "/Users/chubes/Studio"));
        assert!(!base_path_is_under_studio(Some(""), "/Users/chubes/Studio"));
    }

    #[test]
    fn empty_studio_root_does_not_match() {
        assert!(!base_path_is_under_studio(
            Some("/Users/chubes/Studio/my-site"),
            ""
        ));
    }

    #[test]
    fn studio_root_without_trailing_slash_is_normalized() {
        // Without normalization, "/Users/chubes/Studio" would match
        // "/Users/chubes/StudioOther/x" — which is wrong.
        assert!(!base_path_is_under_studio(
            Some("/Users/chubes/StudioOther/my-site"),
            "/Users/chubes/Studio"
        ));
    }

    // Note: project_cli_path() is tested implicitly via apply_component_overrides
    // tests in overrides.rs; those exercise the full cascade including this rung.
    // We test the explicit-override-wins case here as the only branch that
    // doesn't depend on $HOME.
    #[test]
    fn explicit_project_cli_path_wins() {
        let p = project_with(Some("/anywhere"), Some("lando wp"));
        assert_eq!(project_cli_path(&p), Some("lando wp".to_string()));
    }
}

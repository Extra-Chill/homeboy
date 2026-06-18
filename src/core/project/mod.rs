use crate::core::component::ScopedExtensionConfig;
use crate::core::config::{self, ConfigEntity};
use crate::core::engine::local_files::{self, FileSystem};
use crate::core::error::{Error, Result};
use crate::core::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::core::paths;
use crate::core::server;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub mod component;
pub mod files;
pub mod logs;
pub mod pins;
mod readiness;
pub mod report;
mod status;
mod types;

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
    add_pin, list_pins, remove_pin, rename_pin, update_pin, PinUpdateOptions, ProjectPinChange,
    ProjectPinListItem, ProjectPinOutput,
};
pub use readiness::calculate_deploy_readiness;
pub use report::{
    build_components_output, build_create_output, build_delete_output, build_init_output,
    build_list_output, build_path_resolution_output, build_pin_output, build_remove_output,
    build_rename_output, build_set_output, build_show_output, build_status_output, list_report,
    show_report, status_report, ProjectComponentVersion, ProjectListItem, ProjectListReport,
    ProjectPathResolutionReport, ProjectReportExtra, ProjectReportOutput, ProjectShowReport,
    ProjectStatusReport,
};
pub use status::{collect_status, ProjectComponentStatus, ProjectStatusSnapshot};
pub use types::*;

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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub path_roots: HashMap<String, String>,
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
    /// Example: `{"sample-plugin": {"extract_command": "...", "remote_owner": "opencode:opencode"}}`
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

    fn config_path(id: &str) -> Result<PathBuf> {
        paths::project_config(id)
    }

    fn supports_flat_config_entries() -> bool {
        false
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

// ============================================================================
// Core CRUD - Generated by entity_crud! macro
// ============================================================================

entity_crud!(Project; list_ids, merge, slugify_id);

// ============================================================================
// Project directory operations
// ============================================================================

/// Initialize a project directory at `~/.config/homeboy/projects/{id}/`.
pub fn init_project_dir(id: &str) -> Result<PathBuf> {
    let dir = paths::project_dir(id)?;
    let config_path = paths::project_config(id)?;

    if config_path.exists() {
        return Err(Error::validation_invalid_argument(
            "id",
            format!("Project directory '{}' already exists", id),
            Some(id.to_string()),
            None,
        ));
    }

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

    let project = load(id)?;
    local_files::local().ensure_dir(&dir)?;
    let content = config::to_string_pretty(&project)?;
    local_files::local().write(&config_path, &content)?;

    Ok(dir)
}

/// Get the project directory path for a given project ID.
pub fn project_dir_path(id: &str) -> Result<PathBuf> {
    paths::project_dir(id)
}

pub fn resolve_path(path: &Path) -> Result<ProjectPathResolutionReport> {
    let requested_path = expand_path(path);
    let requested_canonical = canonical_or_original(&requested_path);
    let projects = list()?;

    let mut matches = Vec::new();
    for project in projects {
        let Some(base_path) = project.base_path.as_deref() else {
            continue;
        };
        let base = expand_path(Path::new(base_path));
        let base_canonical = canonical_or_original(&base);

        if requested_canonical == base_canonical || requested_canonical.starts_with(&base_canonical)
        {
            matches.push((project, base_canonical));
        }
    }

    matches.sort_by(|(_, a), (_, b)| b.to_string_lossy().len().cmp(&a.to_string_lossy().len()));

    let (project, matched_base_path) = matches.into_iter().next().ok_or_else(|| {
        Error::validation_invalid_argument(
            "path",
            format!(
                "No configured Homeboy project base_path contains {}",
                requested_path.display()
            ),
            Some(requested_path.display().to_string()),
            Some(vec![
                "List configured projects with: homeboy project list".to_string(),
                "Set a project base path with: homeboy project set <project> --json '{\"base_path\":\"/path/to/site\"}'".to_string(),
            ]),
        )
    })?;

    Ok(ProjectPathResolutionReport {
        requested_path: requested_path.display().to_string(),
        resolved_path: requested_canonical.display().to_string(),
        project_id: project.id,
        project_domain: project.domain,
        base_path: matched_base_path.display().to_string(),
    })
}

fn expand_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let expanded = shellexpand::tilde(&raw);
    PathBuf::from(expanded.as_ref())
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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
/// mutate `$HOME` (which races under parallel test runners).
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
mod config_layout_tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn load_requires_directory_project_config() {
        with_isolated_home(|_| {
            let projects = paths::projects().expect("projects path");
            std::fs::create_dir_all(&projects).expect("projects dir");
            std::fs::write(projects.join("legacy.json"), r#"{"domain":"legacy.test"}"#)
                .expect("legacy project config");

            let error = load("legacy").expect_err("flat project config should not load");
            assert_eq!(error.code.as_str(), "project.not_found");
        });
    }

    #[test]
    fn list_ids_ignores_flat_project_configs() {
        with_isolated_home(|_| {
            let projects = paths::projects().expect("projects path");
            std::fs::create_dir_all(&projects).expect("projects dir");
            std::fs::write(projects.join("legacy.json"), r#"{"domain":"legacy.test"}"#)
                .expect("legacy project config");

            let canonical = Project {
                id: "canonical".to_string(),
                domain: Some("canonical.test".to_string()),
                ..Default::default()
            };
            save(&canonical).expect("canonical project config");

            assert_eq!(list_ids().expect("project ids"), vec!["canonical"]);
        });
    }
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
            Some("/Users/user/Studio/my-site"),
            "/Users/user/Studio"
        ));
    }

    #[test]
    fn base_path_outside_studio_root_does_not_match() {
        assert!(!base_path_is_under_studio(
            Some("/var/www/my-site"),
            "/Users/user/Studio"
        ));
    }

    #[test]
    fn empty_or_missing_base_path_does_not_match() {
        assert!(!base_path_is_under_studio(None, "/Users/user/Studio"));
        assert!(!base_path_is_under_studio(Some(""), "/Users/user/Studio"));
    }

    #[test]
    fn empty_studio_root_does_not_match() {
        assert!(!base_path_is_under_studio(
            Some("/Users/user/Studio/my-site"),
            ""
        ));
    }

    #[test]
    fn studio_root_without_trailing_slash_is_normalized() {
        // Without normalization, "/Users/user/Studio" would match
        // "/Users/user/StudioOther/x" — which is wrong.
        assert!(!base_path_is_under_studio(
            Some("/Users/user/StudioOther/my-site"),
            "/Users/user/Studio"
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

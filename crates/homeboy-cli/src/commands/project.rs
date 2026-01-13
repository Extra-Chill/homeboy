use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::config::{
    create_from_json, ComponentConfiguration, ConfigManager, CreateSummary, PinnedRemoteFile,
    PinnedRemoteLog, ProjectConfiguration, ProjectManager, ProjectRecord,
};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// List all configured projects
    List,
    /// Show project configuration
    Show {
        /// Project ID
        project_id: String,
    },
    /// Create a new project
    Create {
        /// JSON input spec for create/update (supports single or bulk)
        #[arg(long)]
        json: Option<String>,

        /// Skip items that already exist (JSON mode only)
        #[arg(long)]
        skip_existing: bool,

        /// Project name (CLI mode)
        name: Option<String>,
        /// Public site domain (CLI mode)
        domain: Option<String>,
        /// Module to enable (can be specified multiple times)
        #[arg(long = "module", value_name = "MODULE")]
        modules: Vec<String>,
        /// Optional server ID
        #[arg(long)]
        server_id: Option<String>,
        /// Optional remote base path
        #[arg(long)]
        base_path: Option<String>,
        /// Optional table prefix
        #[arg(long)]
        table_prefix: Option<String>,
    },
    /// Update project configuration fields
    Set {
        /// Project ID
        project_id: String,
        /// Project name
        #[arg(long)]
        name: Option<String>,
        /// Public site domain
        #[arg(long)]
        domain: Option<String>,
        /// Replace modules (can be specified multiple times)
        #[arg(long = "module", value_name = "MODULE")]
        modules: Vec<String>,
        /// Server ID
        #[arg(long)]
        server_id: Option<String>,
        /// Remote base path
        #[arg(long)]
        base_path: Option<String>,
        /// Table prefix
        #[arg(long)]
        table_prefix: Option<String>,
        /// Replace project component IDs (comma-separated)
        #[arg(long, value_delimiter = ',')]
        component_ids: Vec<String>,
    },
    /// Repair a project file whose name doesn't match the stored project name
    Repair {
        /// Project ID (file stem)
        project_id: String,
    },
    /// Manage project components
    Components {
        #[command(subcommand)]
        command: ProjectComponentsCommand,
    },
    /// Manage pinned files and logs
    Pin {
        #[command(subcommand)]
        command: ProjectPinCommand,
    },
}

#[derive(Subcommand)]
enum ProjectComponentsCommand {
    /// List associated components
    List {
        /// Project ID
        project_id: String,
    },
    /// Replace project components with the provided list
    Set {
        /// Project ID
        project_id: String,
        /// Component IDs
        component_ids: Vec<String>,
    },
    /// Add one or more components
    Add {
        /// Project ID
        project_id: String,
        /// Component IDs
        component_ids: Vec<String>,
    },
    /// Remove one or more components
    Remove {
        /// Project ID
        project_id: String,
        /// Component IDs
        component_ids: Vec<String>,
    },
    /// Remove all components
    Clear {
        /// Project ID
        project_id: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectComponentsOutput {
    pub action: String,
    pub project_id: String,
    pub component_ids: Vec<String>,
    pub components: Vec<ComponentConfiguration>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListItem {
    id: String,
    name: String,
    domain: String,
    modules: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPinOutput {
    pub action: String,
    pub project_id: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ProjectPinListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<ProjectPinChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<ProjectPinChange>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPinListItem {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPinChange {
    pub path: String,
    pub r#type: String,
}

#[derive(Subcommand)]
enum ProjectPinCommand {
    /// List pinned items
    List {
        /// Project ID
        project_id: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: ProjectPinType,
    },
    /// Pin a file or log
    Add {
        /// Project ID
        project_id: String,
        /// Path to pin (relative to basePath or absolute)
        path: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: ProjectPinType,
        /// Optional display label
        #[arg(long)]
        label: Option<String>,
        /// Number of lines to tail (logs only)
        #[arg(long, default_value = "100")]
        tail: u32,
    },
    /// Unpin a file or log
    Remove {
        /// Project ID
        project_id: String,
        /// Path to unpin
        path: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: ProjectPinType,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ProjectPinType {
    File,
    Log,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOutput {
    command: String,
    project_id: Option<String>,
    project: Option<ProjectRecord>,
    projects: Option<Vec<ProjectListItem>>,
    components: Option<ProjectComponentsOutput>,
    pin: Option<ProjectPinOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    import: Option<CreateSummary>,
}

pub fn run(
    args: ProjectArgs,
    _global: &crate::commands::GlobalArgs,
) -> homeboy::Result<(ProjectOutput, i32)> {
    match args.command {
        ProjectCommand::List => list(),
        ProjectCommand::Show { project_id } => show(&project_id),
        ProjectCommand::Create {
            json,
            skip_existing,
            name,
            domain,
            modules,
            server_id,
            base_path,
            table_prefix,
        } => {
            if let Some(spec) = json {
                return create_json(&spec, skip_existing);
            }

            let name = name.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "name",
                    "Missing required argument: name (or use --json)",
                    None,
                    None,
                )
            })?;
            let domain = domain.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "domain",
                    "Missing required argument: domain (or use --json)",
                    None,
                    None,
                )
            })?;

            create(&name, &domain, modules, server_id, base_path, table_prefix)
        }
        ProjectCommand::Set {
            project_id,
            name,
            domain,
            modules,
            server_id,
            base_path,
            table_prefix,
            component_ids,
        } => set(
            &project_id,
            name,
            domain,
            modules,
            server_id,
            base_path,
            table_prefix,
            component_ids,
        ),
        ProjectCommand::Repair { project_id } => repair(&project_id),
        ProjectCommand::Components { command } => components(command),
        ProjectCommand::Pin { command } => pin(command),
    }
}

fn list() -> homeboy::Result<(ProjectOutput, i32)> {
    let projects = ConfigManager::list_projects()?;

    let items: Vec<ProjectListItem> = projects
        .into_iter()
        .map(|record| ProjectListItem {
            id: record.id,
            name: record.config.name,
            domain: record.config.domain,
            modules: record.config.modules,
        })
        .collect();

    Ok((
        ProjectOutput {
            command: "project.list".to_string(),
            project_id: None,
            project: None,
            projects: Some(items),
            components: None,
            pin: None,
            updated: None,
            import: None,
        },
        0,
    ))
}

fn show(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let project = ConfigManager::load_project_record(project_id)?;

    Ok((
        ProjectOutput {
            command: "project.show".to_string(),
            project_id: Some(project.id.clone()),
            project: Some(project),
            projects: None,
            components: None,
            pin: None,
            updated: None,
            import: None,
        },
        0,
    ))
}

fn create_json(spec: &str, skip_existing: bool) -> homeboy::Result<(ProjectOutput, i32)> {
    let summary = create_from_json::<ProjectConfiguration>(spec, skip_existing)?;
    let exit_code = if summary.errors > 0 { 1 } else { 0 };

    Ok((
        ProjectOutput {
            command: "project.create".to_string(),
            project_id: None,
            project: None,
            projects: None,
            components: None,
            pin: None,
            updated: None,
            import: Some(summary),
        },
        exit_code,
    ))
}

fn create(
    name: &str,
    domain: &str,
    modules: Vec<String>,
    server_id: Option<String>,
    base_path: Option<String>,
    table_prefix: Option<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let (created_project_id, _project) =
        ProjectManager::create_project(name, domain, modules, server_id, base_path, table_prefix)?;

    let project = ConfigManager::load_project_record(&created_project_id)?;

    Ok((
        ProjectOutput {
            command: "project.create".to_string(),
            project_id: Some(created_project_id),
            project: Some(project),
            projects: None,
            components: None,
            pin: None,
            updated: None,
            import: None,
        },
        0,
    ))
}

fn set(
    project_id: &str,
    name: Option<String>,
    domain: Option<String>,
    modules: Vec<String>,
    server_id: Option<String>,
    base_path: Option<String>,
    table_prefix: Option<String>,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let mut updated_fields: Vec<String> = Vec::new();

    if let Some(name) = name {
        let result = ProjectManager::rename_project(project_id, &name)?;
        updated_fields.push("name".to_string());

        if result.new_id != project_id {
            updated_fields.push("id".to_string());
        }

        return Ok((
            ProjectOutput {
                command: "project.set".to_string(),
                project_id: Some(result.new_id.clone()),
                project: Some(ConfigManager::load_project_record(&result.new_id)?),
                projects: None,
                components: None,
                pin: None,
                updated: Some(updated_fields),
                import: None,
            },
            0,
        ));
    }

    let mut project = ConfigManager::load_project(project_id)?;

    if let Some(domain) = domain {
        project.domain = domain;
        updated_fields.push("domain".to_string());
    }

    if !modules.is_empty() {
        project.modules = modules;
        updated_fields.push("modules".to_string());
    }

    if let Some(server_id) = server_id {
        project.server_id = Some(server_id);
        updated_fields.push("serverId".to_string());
    }

    if let Some(base_path) = base_path {
        project.base_path = Some(base_path);
        updated_fields.push("basePath".to_string());
    }

    if let Some(table_prefix) = table_prefix {
        project.table_prefix = Some(table_prefix);
        updated_fields.push("tablePrefix".to_string());
    }

    if !component_ids.is_empty() {
        project.component_ids = resolve_component_ids(component_ids, project_id)?;
        updated_fields.push("componentIds".to_string());
    }

    if updated_fields.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "fields",
            "No fields provided to update",
            Some(project_id.to_string()),
            None,
        ));
    }

    ConfigManager::save_project(project_id, &project)?;

    Ok((
        ProjectOutput {
            command: "project.set".to_string(),
            project_id: Some(project_id.to_string()),
            project: Some(ConfigManager::load_project_record(project_id)?),
            projects: None,
            components: None,
            pin: None,
            updated: Some(updated_fields),
            import: None,
        },
        0,
    ))
}

fn repair(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let result = ProjectManager::repair_project(project_id)?;

    let updated = if result.new_id != result.old_id {
        Some(vec!["id".to_string()])
    } else {
        None
    };

    Ok((
        ProjectOutput {
            command: "project.repair".to_string(),
            project_id: Some(result.new_id.clone()),
            project: Some(ConfigManager::load_project_record(&result.new_id)?),
            projects: None,
            components: None,
            pin: None,
            updated,
            import: None,
        },
        0,
    ))
}

fn components(command: ProjectComponentsCommand) -> homeboy::Result<(ProjectOutput, i32)> {
    match command {
        ProjectComponentsCommand::List { project_id } => components_list(&project_id),
        ProjectComponentsCommand::Set {
            project_id,
            component_ids,
        } => components_set(&project_id, component_ids),
        ProjectComponentsCommand::Add {
            project_id,
            component_ids,
        } => components_add(&project_id, component_ids),
        ProjectComponentsCommand::Remove {
            project_id,
            component_ids,
        } => components_remove(&project_id, component_ids),
        ProjectComponentsCommand::Clear { project_id } => components_clear(&project_id),
    }
}

fn components_list(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let project = ConfigManager::load_project(project_id)?;

    let mut components = Vec::new();
    for component_id in &project.component_ids {
        let component = ConfigManager::load_component(component_id)?;
        components.push(component);
    }

    Ok((
        ProjectOutput {
            command: "project.components.list".to_string(),
            project_id: Some(project_id.to_string()),
            project: None,
            projects: None,
            components: Some(ProjectComponentsOutput {
                action: "list".to_string(),
                project_id: project_id.to_string(),
                component_ids: project.component_ids.clone(),
                components,
            }),
            pin: None,
            updated: None,
            import: None,
        },
        0,
    ))
}

fn resolve_component_ids(
    component_ids: Vec<String>,
    project_id: &str,
) -> homeboy::Result<Vec<String>> {
    if component_ids.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "componentIds",
            "At least one component ID is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut missing = Vec::new();
    for component_id in &component_ids {
        if ConfigManager::load_component(component_id).is_err() {
            missing.push(component_id.clone());
        }
    }

    if !missing.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "componentIds",
            "Unknown component IDs (must exist in `homeboy component list`)",
            Some(project_id.to_string()),
            Some(missing),
        ));
    }

    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for id in component_ids {
        if seen.insert(id.clone()) {
            deduped.push(id);
        }
    }

    Ok(deduped)
}

fn components_set(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let deduped = resolve_component_ids(component_ids, project_id)?;

    let mut project = ConfigManager::load_project(project_id)?;
    project.component_ids = deduped.clone();

    write_project_components(project_id, "set", &project)
}

fn components_add(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let deduped = resolve_component_ids(component_ids, project_id)?;

    let mut project = ConfigManager::load_project(project_id)?;
    for id in deduped {
        if !project.component_ids.contains(&id) {
            project.component_ids.push(id);
        }
    }

    write_project_components(project_id, "add", &project)
}

fn components_remove(
    project_id: &str,
    component_ids: Vec<String>,
) -> homeboy::Result<(ProjectOutput, i32)> {
    if component_ids.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "componentIds",
            "At least one component ID is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut project = ConfigManager::load_project(project_id)?;

    let mut missing_from_project = Vec::new();
    for id in &component_ids {
        if !project.component_ids.contains(id) {
            missing_from_project.push(id.clone());
        }
    }

    if !missing_from_project.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "componentIds",
            "Component IDs not attached to project",
            Some(project_id.to_string()),
            Some(missing_from_project),
        ));
    }

    project
        .component_ids
        .retain(|id| !component_ids.contains(id));

    write_project_components(project_id, "remove", &project)
}

fn components_clear(project_id: &str) -> homeboy::Result<(ProjectOutput, i32)> {
    let mut project = ConfigManager::load_project(project_id)?;
    project.component_ids.clear();

    write_project_components(project_id, "clear", &project)
}

fn write_project_components(
    project_id: &str,
    action: &str,
    project: &homeboy::config::ProjectConfiguration,
) -> homeboy::Result<(ProjectOutput, i32)> {
    ConfigManager::save_project(project_id, project)?;

    let mut components = Vec::new();
    for component_id in &project.component_ids {
        let component = ConfigManager::load_component(component_id)?;
        components.push(component);
    }

    Ok((
        ProjectOutput {
            command: format!("project.components.{action}"),
            project_id: Some(project_id.to_string()),
            project: None,
            projects: None,
            components: Some(ProjectComponentsOutput {
                action: action.to_string(),
                project_id: project_id.to_string(),
                component_ids: project.component_ids.clone(),
                components,
            }),
            pin: None,
            updated: Some(vec!["componentIds".to_string()]),
            import: None,
        },
        0,
    ))
}

fn pin(command: ProjectPinCommand) -> homeboy::Result<(ProjectOutput, i32)> {
    match command {
        ProjectPinCommand::List { project_id, r#type } => pin_list(&project_id, r#type),
        ProjectPinCommand::Add {
            project_id,
            path,
            r#type,
            label,
            tail,
        } => pin_add(&project_id, &path, r#type, label, tail),
        ProjectPinCommand::Remove {
            project_id,
            path,
            r#type,
        } => pin_remove(&project_id, &path, r#type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::config::{slugify_id, AppPaths};
    use std::fs;

    fn seed_component(id: &str) -> ComponentConfiguration {
        ComponentConfiguration::new(
            id.to_string(),
            id.to_string(),
            "/tmp".to_string(),
            "remote".to_string(),
            "artifact".to_string(),
        )
    }

    fn seed_project(name: &str) -> homeboy::config::ProjectConfiguration {
        homeboy::config::ProjectConfiguration {
            name: name.to_string(),
            domain: "example.com".to_string(),
            modules: vec![],
            scoped_modules: None,
            server_id: None,
            base_path: None,
            table_prefix: None,
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
        }
    }

    struct EnvGuard {
        prev_xdg_config_home: Option<std::ffi::OsString>,
        prev_home: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set_var(&self, key: &str, value: &std::path::Path) {
            std::env::set_var(key, value);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(v) = self.prev_xdg_config_home.take() {
                std::env::set_var("XDG_CONFIG_HOME", v);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }

            if let Some(v) = self.prev_home.take() {
                std::env::set_var("HOME", v);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn lock_homeboy_test_env() -> (std::sync::MutexGuard<'static, ()>, EnvGuard) {
        static MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let lock = MUTEX.get_or_init(|| std::sync::Mutex::new(()));
        let guard = lock.lock().unwrap();

        let env_guard = EnvGuard {
            prev_xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            prev_home: std::env::var_os("HOME"),
        };

        (guard, env_guard)
    }

    fn setup_homeboy_dir(test_id: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);

        let base = std::env::temp_dir().join(test_id);
        base.join(format!("{}-{}-{}", std::process::id(), nanos, unique))
    }

    #[test]
    fn project_components_set_dedupes_preserving_order() {
        let test_id = "homeboy-project-components-set-dedupe";
        let base = setup_homeboy_dir(test_id);

        let (_env_lock, env_guard) = lock_homeboy_test_env();
        env_guard.set_var("XDG_CONFIG_HOME", &base);
        env_guard.set_var("HOME", &base);

        AppPaths::ensure_directories().unwrap();

        ConfigManager::save_component("alpha", &seed_component("alpha")).unwrap();
        ConfigManager::save_component("beta", &seed_component("beta")).unwrap();

        let project_id = slugify_id("My Project").unwrap();
        ConfigManager::save_project(&project_id, &seed_project("My Project")).unwrap();

        let (_out, code) = components_set(
            &project_id,
            vec!["alpha".to_string(), "beta".to_string(), "alpha".to_string()],
        )
        .unwrap();
        assert_eq!(code, 0);

        let loaded = ConfigManager::load_project(&project_id).unwrap();
        assert_eq!(loaded.component_ids, vec!["alpha", "beta"]);

        drop(env_guard);
        drop(_env_lock);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn project_components_set_rejects_unknown_component_ids() {
        let test_id = "homeboy-project-components-set-rejects-unknown";
        let base = setup_homeboy_dir(test_id);

        let (_env_lock, env_guard) = lock_homeboy_test_env();
        env_guard.set_var("XDG_CONFIG_HOME", &base);
        env_guard.set_var("HOME", &base);

        AppPaths::ensure_directories().unwrap();

        ConfigManager::save_component("alpha", &seed_component("alpha")).unwrap();

        let project_id = slugify_id("My Project").unwrap();
        ConfigManager::save_project(&project_id, &seed_project("My Project")).unwrap();

        let err = components_set(
            &project_id,
            vec!["alpha".to_string(), "missing".to_string()],
        )
        .unwrap_err();
        assert_eq!(err.code, homeboy::ErrorCode::ValidationInvalidArgument);

        drop(env_guard);
        drop(_env_lock);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn project_components_list_returns_configured_ids() {
        let test_id = "homeboy-project-components-list";
        let base = setup_homeboy_dir(test_id);

        let (_env_lock, env_guard) = lock_homeboy_test_env();
        env_guard.set_var("XDG_CONFIG_HOME", &base);
        env_guard.set_var("HOME", &base);

        AppPaths::ensure_directories().unwrap();

        ConfigManager::save_component("alpha", &seed_component("alpha")).unwrap();
        ConfigManager::save_component("beta", &seed_component("beta")).unwrap();

        let project_id = slugify_id("My Project").unwrap();
        let mut project = seed_project("My Project");
        project.component_ids = vec!["beta".to_string(), "alpha".to_string()];
        ConfigManager::save_project(&project_id, &project).unwrap();

        let (out, code) = components_list(&project_id).unwrap();
        assert_eq!(code, 0);

        let payload = out.components.unwrap();
        assert_eq!(payload.component_ids, vec!["beta", "alpha"]);
        assert_eq!(payload.components.len(), 2);

        drop(env_guard);
        drop(_env_lock);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn project_set_component_ids_replaces() {
        let test_id = "homeboy-project-set-component-ids-replaces";
        let base = setup_homeboy_dir(test_id);

        let (_env_lock, env_guard) = lock_homeboy_test_env();
        env_guard.set_var("XDG_CONFIG_HOME", &base);
        env_guard.set_var("HOME", &base);

        AppPaths::ensure_directories().unwrap();

        ConfigManager::save_component("alpha", &seed_component("alpha")).unwrap();
        ConfigManager::save_component("beta", &seed_component("beta")).unwrap();

        let project_id = slugify_id("My Project").unwrap();
        let mut project = seed_project("My Project");
        project.component_ids = vec!["alpha".to_string()];
        ConfigManager::save_project(&project_id, &project).unwrap();

        let (_out, code) = set(
            &project_id,
            None,
            Some("example.com".to_string()),
            vec![],
            None,
            None,
            None,
            vec!["beta".to_string(), "beta".to_string(), "alpha".to_string()],
        )
        .unwrap();
        assert_eq!(code, 0);

        let loaded = ConfigManager::load_project(&project_id).unwrap();
        assert_eq!(loaded.component_ids, vec!["beta", "alpha"]);

        drop(env_guard);
        drop(_env_lock);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn project_components_add_appends_without_dupes() {
        let test_id = "homeboy-project-components-add-dedupes";
        let base = setup_homeboy_dir(test_id);

        let (_env_lock, env_guard) = lock_homeboy_test_env();
        env_guard.set_var("XDG_CONFIG_HOME", &base);
        env_guard.set_var("HOME", &base);

        AppPaths::ensure_directories().unwrap();

        ConfigManager::save_component("alpha", &seed_component("alpha")).unwrap();
        ConfigManager::save_component("beta", &seed_component("beta")).unwrap();

        let project_id = slugify_id("My Project").unwrap();
        let mut project = seed_project("My Project");
        project.component_ids = vec!["alpha".to_string()];
        ConfigManager::save_project(&project_id, &project).unwrap();

        let (_out, code) = components_add(
            &project_id,
            vec!["alpha".to_string(), "beta".to_string(), "beta".to_string()],
        )
        .unwrap();
        assert_eq!(code, 0);

        let loaded = ConfigManager::load_project(&project_id).unwrap();
        assert_eq!(loaded.component_ids, vec!["alpha", "beta"]);

        drop(env_guard);
        drop(_env_lock);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn project_components_remove_rejects_missing_from_project() {
        let test_id = "homeboy-project-components-remove-rejects-missing";
        let base = setup_homeboy_dir(test_id);

        let (_env_lock, env_guard) = lock_homeboy_test_env();
        env_guard.set_var("XDG_CONFIG_HOME", &base);
        env_guard.set_var("HOME", &base);

        AppPaths::ensure_directories().unwrap();

        ConfigManager::save_component("alpha", &seed_component("alpha")).unwrap();

        let project_id = slugify_id("My Project").unwrap();
        let mut project = seed_project("My Project");
        project.component_ids = vec!["alpha".to_string()];
        ConfigManager::save_project(&project_id, &project).unwrap();

        let err = components_remove(&project_id, vec!["missing".to_string()]).unwrap_err();
        assert_eq!(err.code, homeboy::ErrorCode::ValidationInvalidArgument);

        drop(env_guard);
        drop(_env_lock);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn project_components_clear_removes_all() {
        let test_id = "homeboy-project-components-clear";
        let base = setup_homeboy_dir(test_id);

        let (_env_lock, env_guard) = lock_homeboy_test_env();
        env_guard.set_var("XDG_CONFIG_HOME", &base);
        env_guard.set_var("HOME", &base);

        AppPaths::ensure_directories().unwrap();

        ConfigManager::save_component("alpha", &seed_component("alpha")).unwrap();

        let project_id = slugify_id("My Project").unwrap();
        let mut project = seed_project("My Project");
        project.component_ids = vec!["alpha".to_string()];
        ConfigManager::save_project(&project_id, &project).unwrap();

        let (_out, code) = components_clear(&project_id).unwrap();
        assert_eq!(code, 0);

        let loaded = ConfigManager::load_project(&project_id).unwrap();
        assert!(loaded.component_ids.is_empty());

        drop(env_guard);
        drop(_env_lock);
        let _ = fs::remove_dir_all(&base);
    }
}

fn pin_list(
    project_id: &str,
    pin_type: ProjectPinType,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let project = ConfigManager::load_project(project_id)?;

    let (items, type_string) = match pin_type {
        ProjectPinType::File => (
            project
                .remote_files
                .pinned_files
                .iter()
                .map(|file| ProjectPinListItem {
                    path: file.path.clone(),
                    label: file.label.clone(),
                    display_name: file.display_name().to_string(),
                    tail_lines: None,
                })
                .collect(),
            "file",
        ),
        ProjectPinType::Log => (
            project
                .remote_logs
                .pinned_logs
                .iter()
                .map(|log| ProjectPinListItem {
                    path: log.path.clone(),
                    label: log.label.clone(),
                    display_name: log.display_name().to_string(),
                    tail_lines: Some(log.tail_lines),
                })
                .collect(),
            "log",
        ),
    };

    Ok((
        ProjectOutput {
            command: "project.pin.list".to_string(),
            project_id: Some(project_id.to_string()),
            project: None,
            projects: None,
            components: None,
            pin: Some(ProjectPinOutput {
                action: "list".to_string(),
                project_id: project_id.to_string(),
                r#type: type_string.to_string(),
                items: Some(items),
                added: None,
                removed: None,
            }),
            updated: None,
            import: None,
        },
        0,
    ))
}

fn pin_add(
    project_id: &str,
    path: &str,
    pin_type: ProjectPinType,
    label: Option<String>,
    tail: u32,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let mut project = ConfigManager::load_project(project_id)?;

    let type_string = match pin_type {
        ProjectPinType::File => {
            if project
                .remote_files
                .pinned_files
                .iter()
                .any(|file| file.path == path)
            {
                return Err(homeboy::Error::validation_invalid_argument(
                    "path",
                    "File is already pinned",
                    Some(project_id.to_string()),
                    Some(vec![path.to_string()]),
                ));
            }

            project.remote_files.pinned_files.push(PinnedRemoteFile {
                id: Uuid::new_v4(),
                path: path.to_string(),
                label,
            });

            "file"
        }
        ProjectPinType::Log => {
            if project
                .remote_logs
                .pinned_logs
                .iter()
                .any(|log| log.path == path)
            {
                return Err(homeboy::Error::validation_invalid_argument(
                    "path",
                    "Log is already pinned",
                    Some(project_id.to_string()),
                    Some(vec![path.to_string()]),
                ));
            }

            project.remote_logs.pinned_logs.push(PinnedRemoteLog {
                id: Uuid::new_v4(),
                path: path.to_string(),
                label,
                tail_lines: tail,
            });

            "log"
        }
    };

    ConfigManager::save_project(project_id, &project)?;

    Ok((
        ProjectOutput {
            command: "project.pin.add".to_string(),
            project_id: Some(project_id.to_string()),
            project: None,
            projects: None,
            components: None,
            pin: Some(ProjectPinOutput {
                action: "add".to_string(),
                project_id: project_id.to_string(),
                r#type: type_string.to_string(),
                items: None,
                added: Some(ProjectPinChange {
                    path: path.to_string(),
                    r#type: type_string.to_string(),
                }),
                removed: None,
            }),
            updated: None,
            import: None,
        },
        0,
    ))
}

fn pin_remove(
    project_id: &str,
    path: &str,
    pin_type: ProjectPinType,
) -> homeboy::Result<(ProjectOutput, i32)> {
    let mut project = ConfigManager::load_project(project_id)?;

    let (removed, type_string) = match pin_type {
        ProjectPinType::File => {
            let original_len = project.remote_files.pinned_files.len();
            project
                .remote_files
                .pinned_files
                .retain(|file| file.path != path);

            (
                project.remote_files.pinned_files.len() < original_len,
                "file",
            )
        }
        ProjectPinType::Log => {
            let original_len = project.remote_logs.pinned_logs.len();
            project
                .remote_logs
                .pinned_logs
                .retain(|log| log.path != path);

            (project.remote_logs.pinned_logs.len() < original_len, "log")
        }
    };

    if !removed {
        return Err(homeboy::Error::validation_invalid_argument(
            "path",
            format!("{} is not pinned", type_string),
            Some(project_id.to_string()),
            Some(vec![path.to_string()]),
        ));
    }

    ConfigManager::save_project(project_id, &project)?;

    Ok((
        ProjectOutput {
            command: "project.pin.remove".to_string(),
            project_id: Some(project_id.to_string()),
            project: None,
            projects: None,
            components: None,
            pin: Some(ProjectPinOutput {
                action: "remove".to_string(),
                project_id: project_id.to_string(),
                r#type: type_string.to_string(),
                items: None,
                added: None,
                removed: Some(ProjectPinChange {
                    path: path.to_string(),
                    r#type: type_string.to_string(),
                }),
            }),
            updated: None,
            import: None,
        },
        0,
    ))
}

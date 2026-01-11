use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy_core::config::{
    slugify_id, ComponentConfiguration, ConfigManager, SlugIdentifiable, VersionTarget,
};

use super::CmdResult;

fn parse_version_targets(targets: &[String]) -> homeboy_core::Result<Vec<VersionTarget>> {
    let mut parsed = Vec::new();

    for target in targets {
        let mut parts = target.splitn(2, "::");
        let file = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| homeboy_core::Error::Other("Invalid version target".to_string()))?;

        let pattern = parts.next().map(str::trim).filter(|s| !s.is_empty());

        parsed.push(VersionTarget {
            file: file.to_string(),
            pattern: pattern.map(|p| p.to_string()),
        });
    }

    Ok(parsed)
}

#[derive(Args)]
pub struct ComponentArgs {
    #[command(subcommand)]
    command: ComponentCommand,
}

#[derive(Subcommand)]
enum ComponentCommand {
    /// Create a new component configuration
    Create {
        /// Display name (ID derived from name)
        name: String,
        /// Absolute path to local source directory
        #[arg(long)]
        local_path: String,
        /// Remote path relative to project basePath
        #[arg(long)]
        remote_path: String,
        /// Build artifact path relative to localPath
        #[arg(long)]
        build_artifact: String,
        /// Version targets in the form "file" or "file::pattern" (repeatable)
        #[arg(long = "version-target", value_name = "TARGET")]
        version_targets: Vec<String>,
        /// Build command to run in localPath
        #[arg(long)]
        build_command: Option<String>,
        /// WordPress multisite network-activated plugin
        #[arg(long)]
        is_network: bool,
    },
    /// Bulk create components from JSON
    Import {
        /// JSON array of component objects
        json: String,
        /// Skip components that already exist
        #[arg(long)]
        skip_existing: bool,
    },
    /// Display component configuration
    Show {
        /// Component ID
        id: String,
    },
    /// Update component configuration fields
    Set {
        /// Component ID
        id: String,
        /// Update display name
        #[arg(long)]
        name: Option<String>,
        /// Update local path
        #[arg(long)]
        local_path: Option<String>,
        /// Update remote path
        #[arg(long)]
        remote_path: Option<String>,
        /// Update build artifact path
        #[arg(long)]
        build_artifact: Option<String>,
        /// Replace version targets with the provided list (repeatable "file" or "file::pattern")
        #[arg(long = "version-target", value_name = "TARGET")]
        version_targets: Vec<String>,
        /// Update build command
        #[arg(long)]
        build_command: Option<String>,
        /// Set as network-activated plugin
        #[arg(long)]
        is_network: bool,
        /// Clear network activation flag
        #[arg(long)]
        not_network: bool,
    },
    /// Delete a component configuration
    Delete {
        /// Component ID
        id: String,
        /// Skip confirmation
        #[arg(long)]
        force: bool,
    },
    /// List all available components
    List,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentOutput {
    pub action: String,
    pub component_id: Option<String>,
    pub success: bool,
    pub updated_fields: Vec<String>,
    pub created: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
    pub component: Option<ComponentConfiguration>,
    pub components: Vec<ComponentConfiguration>,
}

pub fn run(args: ComponentArgs) -> CmdResult<ComponentOutput> {
    match args.command {
        ComponentCommand::Create {
            name,
            local_path,
            remote_path,
            build_artifact,
            version_targets,
            build_command,
            is_network,
        } => create(
            &name,
            &local_path,
            &remote_path,
            &build_artifact,
            version_targets,
            build_command,
            is_network,
        ),
        ComponentCommand::Import {
            json,
            skip_existing,
        } => import(&json, skip_existing),
        ComponentCommand::Show { id } => show(&id),
        ComponentCommand::Set {
            id,
            name,
            local_path,
            remote_path,
            build_artifact,
            version_targets,
            build_command,
            is_network,
            not_network,
        } => set(
            &id,
            name,
            local_path,
            remote_path,
            build_artifact,
            version_targets,
            build_command,
            is_network,
            not_network,
        ),
        ComponentCommand::Delete { id, force } => delete(&id, force),
        ComponentCommand::List => list(),
    }
}

fn create(
    name: &str,
    local_path: &str,
    remote_path: &str,
    build_artifact: &str,
    version_targets: Vec<String>,
    build_command: Option<String>,
    is_network: bool,
) -> CmdResult<ComponentOutput> {
    let id = slugify_id(name)?;

    if ConfigManager::load_component(&id).is_ok() {
        return Err(homeboy_core::Error::Other(format!(
            "Component '{}' already exists",
            id
        )));
    }

    let expanded_path = shellexpand::tilde(local_path).to_string();

    let mut component = ComponentConfiguration::new(
        name.to_string(),
        expanded_path,
        remote_path.to_string(),
        build_artifact.to_string(),
    );
    if !version_targets.is_empty() {
        component.version_targets = Some(parse_version_targets(&version_targets)?);
    }
    component.build_command = build_command;
    component.is_network = if is_network { Some(true) } else { None };

    ConfigManager::save_component(&id, &component)?;

    Ok((
        ComponentOutput {
            action: "create".to_string(),
            component_id: Some(id.to_string()),
            success: true,
            updated_fields: vec![],
            created: vec![],
            skipped: vec![],
            errors: vec![],
            component: Some(component),
            components: vec![],
        },
        0,
    ))
}

fn import(json_str: &str, skip_existing: bool) -> CmdResult<ComponentOutput> {
    let mut components: Vec<ComponentConfiguration> = serde_json::from_str(json_str)
        .map_err(|e| homeboy_core::Error::Other(format!("Failed to parse JSON - {}", e)))?;

    if components.is_empty() {
        return Err(homeboy_core::Error::Other(
            "No components in JSON array".to_string(),
        ));
    }

    let mut created: Vec<String> = vec![];
    let mut skipped: Vec<String> = vec![];
    let mut errors: Vec<String> = vec![];

    for component in components.iter_mut() {
        component.local_path = shellexpand::tilde(&component.local_path).to_string();

        let id = match component.slug_id() {
            Ok(id) => id,
            Err(e) => {
                errors.push(format!("{}: {}", component.name, e));
                continue;
            }
        };

        if ConfigManager::load_component(&id).is_ok() {
            if skip_existing {
                skipped.push(id);
                continue;
            }

            errors.push(format!("{}: already exists", id));
            continue;
        }

        if let Err(e) = ConfigManager::save_component(&id, component) {
            errors.push(format!("{}: {}", id, e));
        } else {
            created.push(id);
        }
    }

    let exit_code = if errors.is_empty() { 0 } else { 1 };

    Ok((
        ComponentOutput {
            action: "import".to_string(),
            component_id: None,
            success: errors.is_empty(),
            updated_fields: vec![],
            created,
            skipped,
            errors,
            component: None,
            components: vec![],
        },
        exit_code,
    ))
}

fn show(id: &str) -> CmdResult<ComponentOutput> {
    let component = ConfigManager::load_component(id)?;

    Ok((
        ComponentOutput {
            action: "show".to_string(),
            component_id: Some(id.to_string()),
            success: true,
            updated_fields: vec![],
            created: vec![],
            skipped: vec![],
            errors: vec![],
            component: Some(component),
            components: vec![],
        },
        0,
    ))
}

fn set(
    id: &str,
    name: Option<String>,
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
    version_targets: Vec<String>,
    build_command: Option<String>,
    is_network: bool,
    not_network: bool,
) -> CmdResult<ComponentOutput> {
    let mut component = ConfigManager::load_component(id)?;

    let mut updated_fields: Vec<String> = vec![];

    if let Some(value) = name {
        component.name = value;
        updated_fields.push("name".to_string());
    }

    if let Some(value) = local_path {
        component.local_path = shellexpand::tilde(&value).to_string();
        updated_fields.push("localPath".to_string());
    }

    if let Some(value) = remote_path {
        component.remote_path = value;
        updated_fields.push("remotePath".to_string());
    }

    if let Some(value) = build_artifact {
        component.build_artifact = value;
        updated_fields.push("buildArtifact".to_string());
    }

    if !version_targets.is_empty() {
        component.version_targets = Some(parse_version_targets(&version_targets)?);
        updated_fields.push("versionTargets".to_string());
    }

    if let Some(value) = build_command {
        component.build_command = Some(value);
        updated_fields.push("buildCommand".to_string());
    }

    if is_network {
        component.is_network = Some(true);
        updated_fields.push("isNetwork".to_string());
    }

    if not_network {
        component.is_network = None;
        updated_fields.push("isNetwork".to_string());
    }

    if updated_fields.is_empty() {
        return Err(homeboy_core::Error::Other(
            "No fields specified to update".to_string(),
        ));
    }

    ConfigManager::save_component(id, &component)?;

    Ok((
        ComponentOutput {
            action: "set".to_string(),
            component_id: Some(id.to_string()),
            success: true,
            updated_fields,
            created: vec![],
            skipped: vec![],
            errors: vec![],
            component: Some(component),
            components: vec![],
        },
        0,
    ))
}

fn delete(id: &str, force: bool) -> CmdResult<ComponentOutput> {
    if ConfigManager::load_component(id).is_err() {
        return Err(homeboy_core::Error::Other(format!(
            "Component '{}' not found",
            id
        )));
    }

    if !force {
        let projects = ConfigManager::list_projects().unwrap_or_default();
        let using: Vec<String> = projects
            .iter()
            .filter(|p| p.config.component_ids.contains(&id.to_string()))
            .map(|p| p.id.clone())
            .collect();

        if !using.is_empty() {
            return Err(homeboy_core::Error::Other(format!(
                "Component '{}' is used by projects: {}. Use --force to delete anyway.",
                id,
                using.join(", ")
            )));
        }
    }

    ConfigManager::delete_component(id)?;

    Ok((
        ComponentOutput {
            action: "delete".to_string(),
            component_id: Some(id.to_string()),
            success: true,
            updated_fields: vec![],
            created: vec![],
            skipped: vec![],
            errors: vec![],
            component: None,
            components: vec![],
        },
        0,
    ))
}

fn list() -> CmdResult<ComponentOutput> {
    let components = ConfigManager::list_components()?;

    Ok((
        ComponentOutput {
            action: "list".to_string(),
            component_id: None,
            success: true,
            updated_fields: vec![],
            created: vec![],
            skipped: vec![],
            errors: vec![],
            component: None,
            components,
        },
        0,
    ))
}

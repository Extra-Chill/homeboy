use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::component::{self, Component, CreateSummary};
use homeboy::project;

use super::CmdResult;

#[derive(Args)]
pub struct ComponentArgs {
    #[command(subcommand)]
    command: ComponentCommand,
}

#[derive(Subcommand)]
enum ComponentCommand {
    /// Create a new component configuration
    Create {
        /// JSON input spec for create/update (supports single or bulk)
        #[arg(long)]
        json: Option<String>,

        /// Skip items that already exist (JSON mode only)
        #[arg(long)]
        skip_existing: bool,

        /// Display name (ID derived from name) - required in CLI mode
        name: Option<String>,
        /// Absolute path to local source directory
        #[arg(long)]
        local_path: Option<String>,
        /// Remote path relative to project basePath
        #[arg(long)]
        remote_path: Option<String>,
        /// Build artifact path relative to localPath
        #[arg(long)]
        build_artifact: Option<String>,
        /// Version targets in the form "file" or "file::pattern" (repeatable)
        #[arg(long = "version-target", value_name = "TARGET")]
        version_targets: Vec<String>,
        /// Build command to run in localPath
        #[arg(long)]
        build_command: Option<String>,
        /// Extract command to run after upload (e.g., "unzip -o {artifact} && rm {artifact}")
        #[arg(long)]
        extract_command: Option<String>,
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
        /// Merge JSON object into config (any field, supports @file and - for stdin)
        #[arg(long, value_name = "JSON")]
        merge: Option<String>,
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
        /// Update extract command (e.g., "unzip -o {artifact} && rm {artifact}")
        #[arg(long)]
        extract_command: Option<String>,
    },
    /// Delete a component configuration
    Delete {
        /// Component ID
        id: String,
        /// Skip confirmation
        #[arg(long)]
        force: bool,
    },
    /// Rename a component (changes ID based on new name)
    Rename {
        /// Current component ID
        id: String,
        /// New display name (ID will be derived from this)
        new_name: String,
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
    pub component: Option<Component>,
    pub components: Vec<Component>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<CreateSummary>,
}

pub fn run(
    args: ComponentArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ComponentOutput> {
    match args.command {
        ComponentCommand::Create {
            json,
            skip_existing,
            name,
            local_path,
            remote_path,
            build_artifact,
            version_targets,
            build_command,
            extract_command,
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
            let local_path = local_path.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "localPath",
                    "Missing required argument: --local-path (or use --json)",
                    None,
                    None,
                )
            })?;
            let remote_path = remote_path.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "remotePath",
                    "Missing required argument: --remote-path (or use --json)",
                    None,
                    None,
                )
            })?;
            let build_artifact = build_artifact.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "buildArtifact",
                    "Missing required argument: --build-artifact (or use --json)",
                    None,
                    None,
                )
            })?;

            create(
                &name,
                &local_path,
                &remote_path,
                &build_artifact,
                version_targets,
                build_command,
                extract_command,
            )
        }
        ComponentCommand::Show { id } => show(&id),
        ComponentCommand::Set {
            id,
            merge,
            name,
            local_path,
            remote_path,
            build_artifact,
            version_targets,
            build_command,
            extract_command,
        } => set(SetComponentArgs {
            id,
            merge,
            name,
            local_path,
            remote_path,
            build_artifact,
            version_targets,
            build_command,
            extract_command,
        }),
        ComponentCommand::Delete { id, force } => delete(&id, force),
        ComponentCommand::Rename { id, new_name } => rename(&id, &new_name),
        ComponentCommand::List => list(),
    }
}

fn create_json(spec: &str, skip_existing: bool) -> CmdResult<ComponentOutput> {
    let summary = component::create_from_json(spec, skip_existing)?;
    let exit_code = if summary.errors > 0 { 1 } else { 0 };

    Ok((
        ComponentOutput {
            action: "component.create".to_string(),
            component_id: None,
            success: summary.errors == 0,
            updated_fields: vec![],
            component: None,
            components: vec![],
            import: Some(summary),
        },
        exit_code,
    ))
}

fn create(
    name: &str,
    local_path: &str,
    remote_path: &str,
    build_artifact: &str,
    version_targets: Vec<String>,
    build_command: Option<String>,
    extract_command: Option<String>,
) -> CmdResult<ComponentOutput> {
    let id = component::slugify_id(name)?;

    if component::exists(&id) {
        return Err(homeboy::Error::validation_invalid_argument(
            "component.name",
            format!("Component '{}' already exists", id),
            Some(id),
            None,
        ));
    }

    let expanded_path = shellexpand::tilde(local_path).to_string();

    let mut component = Component::new(
        id.to_string(),
        name.to_string(),
        expanded_path,
        remote_path.to_string(),
        build_artifact.to_string(),
    );
    if !version_targets.is_empty() {
        component.version_targets = Some(component::parse_version_targets(&version_targets)?);
    }
    component.build_command = build_command;
    component.extract_command = extract_command;

    component::save(&component)?;

    Ok((
        ComponentOutput {
            action: "create".to_string(),
            component_id: Some(id.to_string()),
            success: true,
            updated_fields: vec![],
            component: Some(component),
            components: vec![],
            import: None,
        },
        0,
    ))
}

fn show(id: &str) -> CmdResult<ComponentOutput> {
    let component = component::load(id)?;

    Ok((
        ComponentOutput {
            action: "show".to_string(),
            component_id: Some(id.to_string()),
            success: true,
            updated_fields: vec![],
            component: Some(component),
            components: vec![],
            import: None,
        },
        0,
    ))
}

struct SetComponentArgs {
    id: String,
    merge: Option<String>,
    name: Option<String>,
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
    version_targets: Vec<String>,
    build_command: Option<String>,
    extract_command: Option<String>,
}

fn set(args: SetComponentArgs) -> CmdResult<ComponentOutput> {
    let SetComponentArgs {
        id,
        merge,
        name,
        local_path,
        remote_path,
        build_artifact,
        version_targets,
        build_command,
        extract_command,
    } = args;

    // Handle --merge via public API (mutually exclusive with individual flags)
    if let Some(spec) = merge {
        let result = component::merge_from_json(&id, &spec)?;
        let component = component::load(&id)?;
        return Ok((
            ComponentOutput {
                action: "set".to_string(),
                component_id: Some(id.clone()),
                success: true,
                updated_fields: result.updated_fields,
                component: Some(component),
                components: vec![],
                import: None,
            },
            0,
        ));
    }

    let mut component = component::load(&id)?;
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
        component.version_targets = Some(component::parse_version_targets(&version_targets)?);
        updated_fields.push("versionTargets".to_string());
    }

    if let Some(value) = build_command {
        component.build_command = Some(value);
        updated_fields.push("buildCommand".to_string());
    }

    if let Some(value) = extract_command {
        component.extract_command = Some(value);
        updated_fields.push("extractCommand".to_string());
    }

    if updated_fields.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "fields",
            "No fields specified to update",
            Some(id.clone()),
            None,
        ));
    }

    component::save(&component)?;

    Ok((
        ComponentOutput {
            action: "set".to_string(),
            component_id: Some(id.clone()),
            success: true,
            updated_fields,
            component: Some(component),
            components: vec![],
            import: None,
        },
        0,
    ))
}

fn delete(id: &str, force: bool) -> CmdResult<ComponentOutput> {
    if !component::exists(id) {
        return Err(homeboy::Error::component_not_found(id.to_string()));
    }

    if !force {
        let projects = project::list().unwrap_or_default();
        let using: Vec<String> = projects
            .iter()
            .filter(|p| p.config.component_ids.contains(&id.to_string()))
            .map(|p| p.id.clone())
            .collect();

        if !using.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "component",
                format!(
                    "Component '{}' is used by projects: {}. Use --force to delete anyway.",
                    id,
                    using.join(", ")
                ),
                Some(id.to_string()),
                Some(using),
            ));
        }
    }

    component::delete(id)?;

    Ok((
        ComponentOutput {
            action: "delete".to_string(),
            component_id: Some(id.to_string()),
            success: true,
            updated_fields: vec![],
            component: None,
            components: vec![],
            import: None,
        },
        0,
    ))
}

fn rename(id: &str, new_name: &str) -> CmdResult<ComponentOutput> {
    let result = component::rename(id, new_name)?;

    Ok((
        ComponentOutput {
            action: "rename".to_string(),
            component_id: Some(result.id.clone()),
            success: true,
            updated_fields: vec!["id".to_string(), "name".to_string()],
            component: Some(result.component),
            components: vec![],
            import: None,
        },
        0,
    ))
}

fn list() -> CmdResult<ComponentOutput> {
    let components = component::list()?;

    Ok((
        ComponentOutput {
            action: "list".to_string(),
            component_id: None,
            success: true,
            updated_fields: vec![],
            component: None,
            components,
            import: None,
        },
        0,
    ))
}

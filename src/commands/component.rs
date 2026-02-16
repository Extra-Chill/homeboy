use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::Path;

use homeboy::component::{self, Component};
use homeboy::project::{self, Project};
use homeboy::BatchResult;

use super::{CmdResult, DynamicSetArgs};

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

        /// Absolute path to local source directory (ID derived from directory name)
        #[arg(long)]
        local_path: Option<String>,
        /// Remote path relative to project basePath
        #[arg(long)]
        remote_path: Option<String>,
        /// Build artifact path relative to localPath
        #[arg(long)]
        build_artifact: Option<String>,
        /// Version targets in the form "file" or "file::pattern" (repeatable). For complex patterns, use --version-targets @file.json to avoid shell escaping
        #[arg(long = "version-target", value_name = "TARGET")]
        version_targets: Vec<String>,
        /// Version targets as JSON array (supports @file.json and - for stdin)
        #[arg(
            long = "version-targets",
            value_name = "JSON",
            conflicts_with = "version_targets"
        )]
        version_targets_json: Option<String>,
        /// Build command to run in localPath
        #[arg(long)]
        build_command: Option<String>,
        /// Extract command to run after upload (e.g., "unzip -o {artifact} && rm {artifact}")
        #[arg(long)]
        extract_command: Option<String>,
        /// Path to changelog file relative to localPath
        #[arg(long)]
        changelog_target: Option<String>,
    },
    /// Display component configuration
    Show {
        /// Component ID
        id: String,
    },
    /// Update component configuration fields
    ///
    /// When combining --json with dynamic flags, use '--' separator:
    ///   homeboy component set ID --json '{}' -- --build_command "cmd"
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,
    },
    /// Delete a component configuration
    Delete {
        /// Component ID
        id: String,
    },
    /// Rename a component (changes ID directly)
    Rename {
        /// Current component ID
        id: String,
        /// New component ID (should match repository directory name)
        new_id: String,
    },
    /// List all available components
    List,
    /// List projects using this component
    Projects {
        /// Component ID
        id: String,
    },
    /// Show which components are shared across projects
    Shared {
        /// Specific component ID to check (optional, shows all if omitted)
        id: Option<String>,
    },
    /// Add a version target to a component
    AddVersionTarget {
        /// Component ID
        id: String,
        /// Target file path relative to component root
        file: String,
        /// Regex pattern with capture group for version
        pattern: String,
    },
}

#[derive(Default, Serialize)]

pub struct ComponentOutput {
    pub command: String,
    pub component_id: Option<String>,
    pub success: bool,
    pub updated_fields: Vec<String>,
    pub component: Option<Component>,
    pub components: Vec<Component>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<BatchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<Project>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<std::collections::HashMap<String, Vec<String>>>,
}

pub fn run(
    args: ComponentArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ComponentOutput> {
    match args.command {
        ComponentCommand::Create {
            json,
            skip_existing,
            local_path,
            remote_path,
            build_artifact,
            version_targets,
            version_targets_json,
            build_command,
            extract_command,
            changelog_target,
        } => {
            let json_spec = if let Some(spec) = json {
                spec
            } else {
                let local_path = local_path.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "local_path",
                        "Missing required argument: --local-path",
                        None,
                        None,
                    )
                })?;

                let remote_path = remote_path.unwrap_or_default();

                let dir_name = Path::new(&local_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| {
                        homeboy::Error::validation_invalid_argument(
                            "local_path",
                            "Could not derive component ID from local path",
                            Some(local_path.clone()),
                            None,
                        )
                    })?;

                let id = component::slugify_id(dir_name)?;

                let mut new_component = Component::new(id, local_path, remote_path, build_artifact);

                new_component.version_targets = if let Some(json_spec) = version_targets_json {
                    let raw = homeboy::config::read_json_spec_to_string(&json_spec)?;
                    serde_json::from_str::<Vec<homeboy::component::VersionTarget>>(&raw)
                        .map_err(|e| {
                            homeboy::Error::validation_invalid_json(
                                e,
                                Some("parse version targets JSON".to_string()),
                                Some(raw.chars().take(200).collect::<String>()),
                            )
                        })?
                        .into()
                } else if !version_targets.is_empty() {
                    Some(component::parse_version_targets(&version_targets)?)
                } else {
                    None
                };

                new_component.build_command = build_command;
                new_component.extract_command = extract_command;
                new_component.changelog_target = changelog_target;

                serde_json::to_string(&new_component).map_err(|e| {
                    homeboy::Error::internal_unexpected(format!("Failed to serialize: {}", e))
                })?
            };

            match component::create(&json_spec, skip_existing)? {
                homeboy::CreateOutput::Single(result) => Ok((
                    ComponentOutput {
                        command: "component.create".to_string(),
                        component_id: Some(result.id),
                        component: Some(result.entity),
                        ..Default::default()
                    },
                    0,
                )),
                homeboy::CreateOutput::Bulk(summary) => {
                    let exit_code = if summary.errors > 0 { 1 } else { 0 };
                    Ok((
                        ComponentOutput {
                            command: "component.create".to_string(),
                            success: summary.errors == 0,
                            import: Some(summary),
                            ..Default::default()
                        },
                        exit_code,
                    ))
                }
            }
        }
        ComponentCommand::Show { id } => show(&id),
        ComponentCommand::Set { args } => set(args),
        ComponentCommand::Delete { id } => delete(&id),
        ComponentCommand::Rename { id, new_id } => rename(&id, &new_id),
        ComponentCommand::List => list(),
        ComponentCommand::Projects { id } => projects(&id),
        ComponentCommand::Shared { id } => shared(id.as_deref()),
        ComponentCommand::AddVersionTarget { id, file, pattern } => {
            add_version_target(&id, &file, &pattern)
        }
    }
}

fn show(id: &str) -> CmdResult<ComponentOutput> {
    let component = component::load(id).map_err(|e| e.with_contextual_hint())?;

    Ok((
        ComponentOutput {
            command: "component.show".to_string(),
            component_id: Some(id.to_string()),
            component: Some(component),
            ..Default::default()
        },
        0,
    ))
}

fn set(args: DynamicSetArgs) -> CmdResult<ComponentOutput> {
    // Merge JSON sources: positional/--json/--base64 spec + dynamic flags
    let spec = args.json_spec()?;
    let has_input = spec.is_some() || !args.extra.is_empty();
    if !has_input {
        return Err(homeboy::Error::validation_invalid_argument(
            "spec",
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
        ));
    }

    let merged = super::merge_json_sources(spec.as_deref(), &args.extra)?;
    let json_string = serde_json::to_string(&merged).map_err(|e| {
        homeboy::Error::internal_unexpected(format!("Failed to serialize merged JSON: {}", e))
    })?;

    match component::merge(args.id.as_deref(), &json_string, &args.replace)? {
        homeboy::MergeOutput::Single(result) => {
            let comp = component::load(&result.id)?;
            Ok((
                ComponentOutput {
                    command: "component.set".to_string(),
                    success: true,
                    component_id: Some(result.id),
                    updated_fields: result.updated_fields,
                    component: Some(comp),
                    ..Default::default()
                },
                0,
            ))
        }
        homeboy::MergeOutput::Bulk(summary) => {
            let exit_code = if summary.errors > 0 { 1 } else { 0 };
            Ok((
                ComponentOutput {
                    command: "component.set".to_string(),
                    success: summary.errors == 0,
                    batch: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

fn add_version_target(id: &str, file: &str, pattern: &str) -> CmdResult<ComponentOutput> {
    // Validate pattern is a valid regex with capture group
    component::validate_version_pattern(pattern)?;

    // Load component to check existing targets
    let comp = component::load(id).map_err(|e| e.with_contextual_hint())?;

    // Validate no conflicting target exists
    if let Some(ref existing) = comp.version_targets {
        component::validate_version_target_conflict(existing, file, pattern, id)?;
    }

    let version_target = serde_json::json!({
        "version_targets": [{
            "file": file,
            "pattern": pattern
        }]
    });

    let json_string = serde_json::to_string(&version_target).map_err(|e| {
        homeboy::Error::internal_unexpected(format!("Failed to serialize: {}", e))
    })?;

    match component::merge(Some(id), &json_string, &[])? {
        homeboy::MergeOutput::Single(result) => {
            let comp = component::load(&result.id)?;
            Ok((
                ComponentOutput {
                    command: "component.add-version-target".to_string(),
                    success: true,
                    component_id: Some(result.id),
                    updated_fields: result.updated_fields,
                    component: Some(comp),
                    ..Default::default()
                },
                0,
            ))
        }
        homeboy::MergeOutput::Bulk(_) => Err(homeboy::Error::internal_unexpected(
            "Unexpected bulk result for single component".to_string(),
        )),
    }
}

fn delete(id: &str) -> CmdResult<ComponentOutput> {
    component::delete_safe(id)?;

    Ok((
        ComponentOutput {
            command: "component.delete".to_string(),
            component_id: Some(id.to_string()),
            ..Default::default()
        },
        0,
    ))
}

fn rename(id: &str, new_id: &str) -> CmdResult<ComponentOutput> {
    let component = component::rename(id, new_id)?;

    Ok((
        ComponentOutput {
            command: "component.rename".to_string(),
            component_id: Some(component.id.clone()),
            updated_fields: vec!["id".to_string()],
            component: Some(component),
            ..Default::default()
        },
        0,
    ))
}

fn list() -> CmdResult<ComponentOutput> {
    let components = component::list()?;

    Ok((
        ComponentOutput {
            command: "component.list".to_string(),
            components,
            ..Default::default()
        },
        0,
    ))
}

fn projects(id: &str) -> CmdResult<ComponentOutput> {
    let project_ids = component::projects_using(id)?;

    let mut projects_list = Vec::new();
    for pid in &project_ids {
        if let Ok(p) = project::load(pid) {
            projects_list.push(p);
        }
    }

    Ok((
        ComponentOutput {
            command: "component.projects".to_string(),
            component_id: Some(id.to_string()),
            project_ids: Some(project_ids),
            projects: Some(projects_list),
            ..Default::default()
        },
        0,
    ))
}

fn shared(id: Option<&str>) -> CmdResult<ComponentOutput> {
    if let Some(component_id) = id {
        // Show projects for a specific component
        let project_ids = component::projects_using(component_id)?;
        let mut shared_map = std::collections::HashMap::new();
        shared_map.insert(component_id.to_string(), project_ids);

        Ok((
            ComponentOutput {
                command: "component.shared".to_string(),
                component_id: Some(component_id.to_string()),
                shared: Some(shared_map),
                ..Default::default()
            },
            0,
        ))
    } else {
        // Show all components and their projects
        let shared_map = component::shared_components()?;

        Ok((
            ComponentOutput {
                command: "component.shared".to_string(),
                shared: Some(shared_map),
                ..Default::default()
            },
            0,
        ))
    }
}

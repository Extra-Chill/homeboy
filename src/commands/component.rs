use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::Path;

use homeboy::component::{self, Component};
use homeboy::project::{self, Project};
use homeboy::EntityCrudOutput;

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
        /// Extension(s) this component uses (e.g., "wordpress"). Repeatable.
        #[arg(long = "extension", value_name = "EXTENSION")]
        extensions: Vec<String>,
    },
    /// Display component configuration
    Show {
        /// Component ID
        id: String,
    },
    /// Update component configuration fields
    ///
    /// Supports dedicated flags for common fields (e.g., --local-path, --build-command)
    /// as well as --json for arbitrary updates. When combining --json with dynamic
    /// trailing flags, use '--' separator.
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,

        /// Absolute path to local source directory
        #[arg(long)]
        local_path: Option<String>,
        /// Remote path relative to project basePath
        #[arg(long)]
        remote_path: Option<String>,
        /// Build artifact path relative to localPath
        #[arg(long)]
        build_artifact: Option<String>,
        /// Build command to run in localPath
        #[arg(long)]
        build_command: Option<String>,
        /// Extract command to run after upload (e.g., "unzip -o {artifact} && rm {artifact}")
        #[arg(long)]
        extract_command: Option<String>,
        /// Path to changelog file relative to localPath
        #[arg(long)]
        changelog_target: Option<String>,

        /// Version targets in the form "file" or "file::pattern" (repeatable).
        /// Same format as `component create --version-target`.
        #[arg(long = "version-target", value_name = "TARGET")]
        version_targets: Vec<String>,

        /// Extension(s) this component uses (e.g., "wordpress"). Repeatable.
        #[arg(long = "extension", value_name = "EXTENSION")]
        extensions: Vec<String>,
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

/// Entity-specific fields for component commands.
#[derive(Debug, Default, Serialize)]
pub struct ComponentExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<Project>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<std::collections::HashMap<String, Vec<String>>>,
}

pub type ComponentOutput = EntityCrudOutput<Component, ComponentExtra>;

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
            extensions,
        } => {
            let json_spec = if let Some(spec) = json {
                spec
            } else {
                let local_path = local_path.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "local_path",
                        "Missing required argument: --local-path",
                        None,
                        Some(vec![
                            "Create from flags: homeboy component create --local-path <path>".to_string(),
                            "Create from JSON: homeboy component create --json '{...}'".to_string(),
                            "Tip: portable config in homeboy.json is applied automatically at load time".to_string(),
                        ]),
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

                let mut new_component =
                    Component::new(id.clone(), local_path, remote_path, build_artifact);

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

                if !extensions.is_empty() {
                    let mut extension_map = std::collections::HashMap::new();
                    for extension_id in extensions {
                        extension_map
                            .insert(extension_id, component::ScopedExtensionConfig::default());
                    }
                    new_component.extensions = Some(extension_map);
                }

                homeboy::config::serialize_with_id(&new_component, &id)?
            };

            match component::create(&json_spec, skip_existing)? {
                homeboy::CreateOutput::Single(result) => Ok((
                    ComponentOutput {
                        command: "component.create".to_string(),
                        id: Some(result.id),
                        entity: Some(result.entity),
                        ..Default::default()
                    },
                    0,
                )),
                homeboy::CreateOutput::Bulk(summary) => {
                    let exit_code = summary.exit_code();
                    Ok((
                        ComponentOutput {
                            command: "component.create".to_string(),
                            import: Some(summary),
                            ..Default::default()
                        },
                        exit_code,
                    ))
                }
            }
        }
        ComponentCommand::Show { id } => show(&id),
        ComponentCommand::Set {
            args,
            local_path,
            remote_path,
            build_artifact,
            build_command,
            extract_command,
            changelog_target,
            version_targets,
            extensions,
        } => set(
            args,
            ComponentSetFlags {
                local_path,
                remote_path,
                build_artifact,
                build_command,
                extract_command,
                changelog_target,
            },
            version_targets,
            extensions,
        ),
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
            id: Some(id.to_string()),
            entity: Some(component),
            ..Default::default()
        },
        0,
    ))
}

/// Dedicated flags for common component fields on `component set`.
struct ComponentSetFlags {
    local_path: Option<String>,
    remote_path: Option<String>,
    build_artifact: Option<String>,
    build_command: Option<String>,
    extract_command: Option<String>,
    changelog_target: Option<String>,
}

impl ComponentSetFlags {
    fn has_any(&self) -> bool {
        self.local_path.is_some()
            || self.remote_path.is_some()
            || self.build_artifact.is_some()
            || self.build_command.is_some()
            || self.extract_command.is_some()
            || self.changelog_target.is_some()
    }

    /// Insert non-None fields into a JSON object.
    fn apply_to(&self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        if let Some(ref v) = self.local_path {
            obj.insert("local_path".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.remote_path {
            obj.insert("remote_path".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.build_artifact {
            obj.insert("build_artifact".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.build_command {
            obj.insert("build_command".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.extract_command {
            obj.insert("extract_command".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.changelog_target {
            obj.insert("changelog_target".to_string(), serde_json::json!(v));
        }
    }
}

fn set(
    args: DynamicSetArgs,
    flags: ComponentSetFlags,
    version_targets: Vec<String>,
    extensions: Vec<String>,
) -> CmdResult<ComponentOutput> {
    // Check if there's any input at all
    let has_dynamic = args.json_spec()?.is_some() || !args.effective_extra().is_empty();
    if !has_dynamic && !flags.has_any() && version_targets.is_empty() && extensions.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "spec",
            "Provide a flag (e.g., --local-path), --json spec, --base64, --key value, --version-target, or --extension",
            None,
            None,
        ));
    }

    let mut merged = super::merge_dynamic_args(&args)?
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

    // Apply dedicated flags — these override JSON spec values for the same field.
    if let serde_json::Value::Object(ref mut obj) = merged {
        flags.apply_to(obj);
    }

    // Support --version-target flag like `component create`.
    if !version_targets.is_empty() {
        let parsed = component::parse_version_targets(&version_targets)?;
        if let serde_json::Value::Object(ref mut obj) = merged {
            obj.insert("version_targets".to_string(), serde_json::json!(parsed));
        } else {
            return Err(homeboy::Error::validation_invalid_argument(
                "spec",
                "Merged spec must be a JSON object",
                None,
                None,
            ));
        }
    }

    // Support --extension flag. Builds extensions map with default empty configs.
    if !extensions.is_empty() {
        let mut extension_map = serde_json::Map::new();
        for extension_id in &extensions {
            extension_map.insert(extension_id.clone(), serde_json::json!({}));
        }
        if let serde_json::Value::Object(ref mut obj) = merged {
            obj.insert(
                "extensions".to_string(),
                serde_json::Value::Object(extension_map),
            );
        }
    }

    let (json_string, replace_fields) = super::finalize_set_spec(&merged, &args.replace)?;

    match component::merge(args.id.as_deref(), &json_string, &replace_fields)? {
        homeboy::MergeOutput::Single(result) => {
            let comp = component::load(&result.id)?;
            Ok((
                ComponentOutput {
                    command: "component.set".to_string(),
                    id: Some(result.id),
                    updated_fields: result.updated_fields,
                    entity: Some(comp),
                    ..Default::default()
                },
                0,
            ))
        }
        homeboy::MergeOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                ComponentOutput {
                    command: "component.set".to_string(),
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

    let json_string = homeboy::config::to_json_string(&version_target)?;

    match component::merge(Some(id), &json_string, &[])? {
        homeboy::MergeOutput::Single(result) => {
            let comp = component::load(&result.id)?;
            Ok((
                ComponentOutput {
                    command: "component.add-version-target".to_string(),
                    id: Some(result.id),
                    updated_fields: result.updated_fields,
                    entity: Some(comp),
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
            id: Some(id.to_string()),
            deleted: vec![id.to_string()],
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
            id: Some(component.id.clone()),
            updated_fields: vec!["id".to_string()],
            entity: Some(component),
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
            entities: components,
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
            id: Some(id.to_string()),
            extra: ComponentExtra {
                project_ids: Some(project_ids),
                projects: Some(projects_list),
                ..Default::default()
            },
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
                id: Some(component_id.to_string()),
                extra: ComponentExtra {
                    shared: Some(shared_map),
                    ..Default::default()
                },
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
                extra: ComponentExtra {
                    shared: Some(shared_map),
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_set_flags_has_any_all_none() {
        let flags = ComponentSetFlags {
            local_path: None,
            remote_path: None,
            build_artifact: None,
            build_command: None,
            extract_command: None,
            changelog_target: None,
        };
        assert!(!flags.has_any());
    }

    #[test]
    fn test_component_set_flags_has_any_single_field() {
        let flags = ComponentSetFlags {
            local_path: Some("/foo".to_string()),
            remote_path: None,
            build_artifact: None,
            build_command: None,
            extract_command: None,
            changelog_target: None,
        };
        assert!(flags.has_any());
    }

    #[test]
    fn test_component_set_flags_apply_to_inserts_fields() {
        let flags = ComponentSetFlags {
            local_path: Some("/new/path".to_string()),
            remote_path: None,
            build_artifact: None,
            build_command: Some("npm run build".to_string()),
            extract_command: None,
            changelog_target: Some("CHANGELOG.md".to_string()),
        };

        let mut obj = serde_json::Map::new();
        flags.apply_to(&mut obj);

        assert_eq!(obj.len(), 3);
        assert_eq!(obj["local_path"], serde_json::json!("/new/path"));
        assert_eq!(obj["build_command"], serde_json::json!("npm run build"));
        assert_eq!(obj["changelog_target"], serde_json::json!("CHANGELOG.md"));
        assert!(!obj.contains_key("remote_path"));
    }

    #[test]
    fn test_component_set_flags_apply_to_overrides_existing() {
        let flags = ComponentSetFlags {
            local_path: Some("/override".to_string()),
            remote_path: None,
            build_artifact: None,
            build_command: None,
            extract_command: None,
            changelog_target: None,
        };

        let mut obj = serde_json::Map::new();
        obj.insert("local_path".to_string(), serde_json::json!("/original"));
        obj.insert("remote_path".to_string(), serde_json::json!("/keep-this"));

        flags.apply_to(&mut obj);

        assert_eq!(obj["local_path"], serde_json::json!("/override"));
        assert_eq!(obj["remote_path"], serde_json::json!("/keep-this"));
    }
}

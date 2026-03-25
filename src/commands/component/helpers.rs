//! helpers — extracted from component.rs.

use super::super::{CmdResult, DynamicSetArgs};
use super::list;
use super::projects;
use super::shared;
use super::suggest_project_for_path;
use super::ComponentArgs;
use super::ComponentCommand;
use super::ComponentExtra;
use super::ComponentOutput;
use super::ComponentSetFlags;
use clap::{Args, Subcommand};
use homeboy::component::{self, Component};
use homeboy::project::{self, Project};
use homeboy::EntityCrudOutput;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

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
            extract_command,
            changelog_target,
            extensions,
            project,
        } => {
            if json.is_some() || skip_existing {
                return Err(homeboy::Error::validation_invalid_argument(
                    "component.create",
                    "component create now initializes repo-owned homeboy.json from flags; JSON bulk create is legacy and no longer supported here",
                    None,
                    Some(vec![
                        "Use: homeboy component create --local-path <path> [flags]".to_string(),
                        "Then attach it to a project with: homeboy project components attach-path <project> <path>".to_string(),
                    ]),
                ));
            }

            let local_path = local_path.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "local_path",
                    "Missing required argument: --local-path",
                    None,
                    Some(vec![
                        "Initialize a repo: homeboy component create --local-path <path>"
                            .to_string(),
                        "This writes portable config to <path>/homeboy.json".to_string(),
                    ]),
                )
            })?;

            let remote_path = remote_path.unwrap_or_default();
            let repo_path = Path::new(&local_path);
            let dir_name = repo_path
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

            let id = homeboy::engine::identifier::slugify_id(dir_name, "component_id")?;
            let mut new_component =
                Component::new(id.clone(), local_path.clone(), remote_path, build_artifact);

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

            new_component.extract_command = extract_command;
            new_component.changelog_target = changelog_target;

            if !extensions.is_empty() {
                let mut extension_map = std::collections::HashMap::new();
                for extension_id in extensions {
                    extension_map.insert(extension_id, component::ScopedExtensionConfig::default());
                }
                new_component.extensions = Some(extension_map);
            }

            component::write_portable_config(repo_path, &new_component)?;

            // Attach to project if --project was specified (#900)
            let mut attached_project: Option<String> = None;
            if let Some(ref project_id) = project {
                project::attach_component_path(project_id, &id, &local_path)?;
                attached_project = Some(project_id.clone());
            }

            // Build a next-step hint when not attached to a project
            let hint = if attached_project.is_some() {
                None
            } else {
                // Try to suggest a project by checking existing projects
                let suggestion = suggest_project_for_path(&local_path);
                Some(match suggestion {
                    Some(project_id) => format!(
                        "Attach to a project to enable release/deploy:\n  homeboy project components attach-path {} {}",
                        project_id, local_path
                    ),
                    None => format!(
                        "Attach to a project to enable release/deploy:\n  homeboy project components attach-path <project> {}",
                        local_path
                    ),
                })
            };

            Ok((
                ComponentOutput {
                    command: "component.create".to_string(),
                    id: Some(id),
                    entity: Some(component::portable_json(&new_component)?),
                    hint,
                    extra: ComponentExtra {
                        project_ids: attached_project.map(|p| vec![p]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
        ComponentCommand::Show { id } => show(&id),
        ComponentCommand::Set {
            args,
            local_path,
            remote_path,
            build_artifact,
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

pub(crate) fn show(id: &str) -> CmdResult<ComponentOutput> {
    let component = component::load(id).map_err(|e| e.with_contextual_hint())?;

    Ok((
        ComponentOutput {
            command: "component.show".to_string(),
            id: Some(id.to_string()),
            entity: Some({
                let mut value = serde_json::to_value(&component).map_err(|error| {
                    homeboy::Error::validation_invalid_argument(
                        "component",
                        "Failed to serialize component",
                        Some(error.to_string()),
                        None,
                    )
                })?;
                if let Value::Object(ref mut map) = value {
                    map.insert("id".to_string(), Value::String(component.id.clone()));
                }
                value
            }),
            ..Default::default()
        },
        0,
    ))
}

pub(crate) fn set(
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
                    entity: Some({
                        let mut value = serde_json::to_value(&comp).map_err(|error| {
                            homeboy::Error::validation_invalid_argument(
                                "component",
                                "Failed to serialize component",
                                Some(error.to_string()),
                                None,
                            )
                        })?;
                        if let Value::Object(ref mut map) = value {
                            map.insert("id".to_string(), Value::String(comp.id.clone()));
                        }
                        value
                    }),
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

pub(crate) fn add_version_target(
    id: &str,
    file: &str,
    pattern: &str,
) -> CmdResult<ComponentOutput> {
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
                    entity: Some({
                        let mut value = serde_json::to_value(&comp).map_err(|error| {
                            homeboy::Error::validation_invalid_argument(
                                "component",
                                "Failed to serialize component",
                                Some(error.to_string()),
                                None,
                            )
                        })?;
                        if let Value::Object(ref mut map) = value {
                            map.insert("id".to_string(), Value::String(comp.id.clone()));
                        }
                        value
                    }),
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

pub(crate) fn delete(id: &str) -> CmdResult<ComponentOutput> {
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

pub(crate) fn rename(id: &str, new_id: &str) -> CmdResult<ComponentOutput> {
    let component = component::rename(id, new_id)?;

    Ok((
        ComponentOutput {
            command: "component.rename".to_string(),
            id: Some(component.id.clone()),
            updated_fields: vec!["id".to_string()],
            entity: Some({
                let mut value = serde_json::to_value(&component).map_err(|error| {
                    homeboy::Error::validation_invalid_argument(
                        "component",
                        "Failed to serialize component",
                        Some(error.to_string()),
                        None,
                    )
                })?;
                if let Value::Object(ref mut map) = value {
                    map.insert("id".to_string(), Value::String(component.id.clone()));
                }
                value
            }),
            ..Default::default()
        },
        0,
    ))
}

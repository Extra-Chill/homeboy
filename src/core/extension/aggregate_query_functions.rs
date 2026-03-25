//! aggregate_query_functions — extracted from mod.rs.

use crate::component::Component;
use crate::config;
use crate::error::Error;
use crate::error::Result;
use crate::output::MergeOutput;
use crate::paths;
use std::collections::HashMap;
use std::path::PathBuf;
use serde::Serialize;
use crate::core::extension::ActionSummary;
use crate::core::extension::load_all_extensions;
use crate::core::extension::extension_path;
use crate::core::extension::UpdateEntry;
use crate::core::extension::available_extension_ids;
use crate::core::extension::ExtensionSummary;
use crate::core::extension::load_extension;
use crate::core::extension::UpdateAllResult;


/// List all extensions with pre-computed summary fields.
///
/// Aggregates ready status, compatibility, linked status, CLI info, actions,
/// and runtime details into a single summary per extension.
pub fn list_summaries(project: Option<&crate::project::Project>) -> Vec<ExtensionSummary> {
    let extensions = load_all_extensions().unwrap_or_default();

    extensions
        .iter()
        .map(|ext| {
            let ready_status = extension_ready_status(ext);
            let compatible = is_extension_compatible(ext, project);
            let linked = is_extension_linked(&ext.id);

            let (cli_tool, cli_display_name) = ext
                .cli
                .as_ref()
                .map(|cli| (Some(cli.tool.clone()), Some(cli.display_name.clone())))
                .unwrap_or((None, None));

            let actions: Vec<ActionSummary> = ext
                .actions
                .iter()
                .map(|a| ActionSummary {
                    id: a.id.clone(),
                    label: a.label.clone(),
                    action_type: a.action_type.clone(),
                })
                .collect();

            let has_setup = ext
                .runtime()
                .and_then(|r| r.setup_command.as_ref())
                .map(|_| true);
            let has_ready_check = ext
                .runtime()
                .and_then(|r| r.ready_check.as_ref())
                .map(|_| true);

            let source_revision = read_source_revision(&ext.id);

            ExtensionSummary {
                id: ext.id.clone(),
                name: ext.name.clone(),
                version: ext.version.clone(),
                description: ext
                    .description
                    .as_ref()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("")
                    .to_string(),
                runtime: if ext.executable.is_some() {
                    "executable".to_string()
                } else {
                    "platform".to_string()
                },
                compatible,
                ready: ready_status.ready,
                ready_reason: ready_status.reason,
                ready_detail: ready_status.detail,
                linked,
                path: ext.extension_path.clone().unwrap_or_default(),
                source_revision,
                cli_tool,
                cli_display_name,
                actions,
                has_setup,
                has_ready_check,
            }
        })
        .collect()
}

/// Update all installed extensions, skipping linked ones.
///
/// Linked extensions are managed externally (symlinks to dev directories)
/// and should not be updated via git pull.
pub fn update_all(force: bool) -> UpdateAllResult {
    let extension_ids = available_extension_ids();
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for id in &extension_ids {
        if is_extension_linked(id) {
            skipped.push(id.clone());
            continue;
        }

        let old_version = load_extension(id).ok().map(|m| m.version.clone());

        match update(id, force) {
            Ok(_) => {
                let new_version = load_extension(id)
                    .ok()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();

                updated.push(UpdateEntry {
                    extension_id: id.clone(),
                    old_version: old_version.unwrap_or_default(),
                    new_version,
                });
            }
            Err(_) => {
                skipped.push(id.clone());
            }
        }
    }

    UpdateAllResult { updated, skipped }
}

/// Execute a tool from an extension's vendor directory.
///
/// Sets up PATH with the extension's vendor/bin and node_modules/.bin,
/// resolves the working directory from an optional component, and runs
/// the command interactively.
pub fn exec_tool(extension_id: &str, component_id: Option<&str>, args: &[String]) -> Result<i32> {
    use crate::server::execute_local_command_interactive;

    let extension = load_extension(extension_id)?;
    let ext_path = extension
        .extension_path
        .as_deref()
        .ok_or_else(|| Error::config_missing_key("extension_path", Some(extension_id.into())))?;

    // Resolve working directory
    let working_dir = if let Some(cid) = component_id {
        let comp = crate::component::load(cid)?;
        comp.local_path.clone()
    } else {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    };

    // Build PATH with extension vendor directories prepended
    let vendor_bin = format!("{}/vendor/bin", ext_path);
    let node_bin = format!("{}/node_modules/.bin", ext_path);
    let current_path = std::env::var("PATH").unwrap_or_default();
    let enriched_path = format!("{}:{}:{}", vendor_bin, node_bin, current_path);

    let env = vec![
        ("PATH", enriched_path.as_str()),
        (exec_context::EXTENSION_PATH, ext_path),
        (exec_context::EXTENSION_ID, extension_id),
    ];

    let command = args.join(" ");
    Ok(execute_local_command_interactive(
        &command,
        Some(&working_dir),
        Some(&env),
    ))
}

pub fn save_manifest(manifest: &ExtensionManifest) -> Result<()> {
    config::save(manifest)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<ExtensionManifest>(id, json_spec, replace_fields)
}

/// Check if a extension is a symlink (linked, not installed).
pub fn is_extension_linked(extension_id: &str) -> bool {
    paths::extension(extension_id)
        .map(|p| p.is_symlink())
        .unwrap_or(false)
}

/// Validate that all extensions declared in a component's `extensions` field are installed.
///
/// If `component.extensions` contains keys like `{"wordpress": {}}`, those extensions
/// are implicitly required. Returns an actionable error with install commands
/// when any are missing.
pub fn validate_required_extensions(component: &crate::component::Component) -> Result<()> {
    let extensions = match &component.extensions {
        Some(m) if !m.is_empty() => m,
        _ => return Ok(()),
    };

    let mut missing: Vec<String> = Vec::new();
    for extension_id in extensions.keys() {
        if load_extension(extension_id).is_err() {
            missing.push(extension_id.clone());
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    missing.sort();

    let extension_list = missing.join(", ");
    let install_hints: Vec<String> = missing
        .iter()
        .map(|id| {
            format!(
                "homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id {}",
                id
            )
        })
        .collect();

    let message = if missing.len() == 1 {
        format!(
            "Component '{}' requires extension '{}' which is not installed",
            component.id, missing[0]
        )
    } else {
        format!(
            "Component '{}' requires extensions not installed: {}",
            component.id, extension_list
        )
    };

    let mut err = crate::error::Error::new(
        crate::error::ErrorCode::ExtensionNotFound,
        message,
        serde_json::json!({
            "component_id": component.id,
            "missing_extensions": missing,
        }),
    );

    for hint in &install_hints {
        err = err.with_hint(hint.to_string());
    }

    err = err.with_hint(
        "Browse available extensions: https://github.com/Extra-Chill/homeboy-extensions"
            .to_string(),
    );

    Err(err)
}

/// Validate that all extensions declared in a component's `extensions` field are installed
/// and satisfy the declared version constraints.
///
/// Returns an actionable error listing every unsatisfied requirement with install/update hints.
pub fn validate_extension_requirements(component: &crate::component::Component) -> Result<()> {
    let extensions = match &component.extensions {
        Some(e) if !e.is_empty() => e,
        _ => return Ok(()),
    };

    let mut errors: Vec<String> = Vec::new();
    let mut hints: Vec<String> = Vec::new();

    for (extension_id, ext_config) in extensions {
        let constraint_str = match &ext_config.version {
            Some(v) => v.as_str(),
            None => continue, // No version constraint, skip validation
        };

        let constraint = match version::VersionConstraint::parse(constraint_str) {
            Ok(c) => c,
            Err(_) => {
                errors.push(format!(
                    "Invalid version constraint '{}' for extension '{}'",
                    constraint_str, extension_id
                ));
                continue;
            }
        };

        match load_extension(extension_id) {
            Ok(extension) => match extension.semver() {
                Ok(installed_version) => {
                    if !constraint.matches(&installed_version) {
                        errors.push(format!(
                            "'{}' requires {}, but {} is installed",
                            extension_id, constraint, installed_version
                        ));
                        hints.push(format!(
                            "Run `homeboy extension update {}` to get the latest version",
                            extension_id
                        ));
                    }
                }
                Err(_) => {
                    errors.push(format!(
                        "Extension '{}' has invalid version '{}'",
                        extension_id, extension.version
                    ));
                }
            },
            Err(_) => {
                errors.push(format!("Extension '{}' is not installed", extension_id));
                hints.push(format!(
                    "homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id {}",
                    extension_id
                ));
            }
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    let message = if errors.len() == 1 {
        format!(
            "Component '{}' has an unsatisfied extension requirement: {}",
            component.id, errors[0]
        )
    } else {
        format!(
            "Component '{}' has {} unsatisfied extension requirements:\n  - {}",
            component.id,
            errors.len(),
            errors.join("\n  - ")
        )
    };

    let mut err = crate::error::Error::new(
        crate::error::ErrorCode::ExtensionNotFound,
        message,
        serde_json::json!({
            "component_id": component.id,
            "unsatisfied": errors,
        }),
    );

    for hint in &hints {
        err = err.with_hint(hint.to_string());
    }

    Err(err)
}

/// Check if any of the component's linked extensions provide build configuration.
pub fn extension_provides_build(component: &crate::component::Component) -> bool {
    let extensions = match &component.extensions {
        Some(m) => m,
        None => return false,
    };

    for extension_id in extensions.keys() {
        if let Ok(extension) = load_extension(extension_id) {
            if extension.has_build() {
                return true;
            }
        }
    }
    false
}

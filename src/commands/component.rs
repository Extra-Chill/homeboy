use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

use homeboy::core::component::{self, Component};
use homeboy::core::project::{self, Project};
use homeboy::core::EntityCrudOutput;

use super::{CmdResult, DynamicSetArgs};

mod env;

#[derive(Args)]
pub struct ComponentArgs {
    #[command(subcommand)]
    command: ComponentCommand,
}

#[derive(Subcommand)]
enum ComponentCommand {
    /// Initialize portable component config for a repo
    Create {
        /// Absolute path to local source directory (writes homeboy.json there)
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
        /// Extract command to run after upload (e.g., "unzip -o {{artifact}} && rm {{artifact}}")
        #[arg(long)]
        extract_command: Option<String>,
        /// Path to changelog file relative to localPath
        #[arg(long)]
        changelog_target: Option<String>,
        /// Extension(s) this component uses (e.g., a runtime/framework extension id). Repeatable.
        #[arg(long = "extension", value_name = "EXTENSION")]
        extensions: Vec<String>,
        /// Attach component to a project after creation
        #[arg(long)]
        project: Option<String>,
    },
    /// Display component configuration
    Show {
        /// Component ID (optional when --path is provided)
        id: Option<String>,
        /// Discover component from a directory's homeboy.json instead of the registry
        #[arg(long)]
        path: Option<String>,
    },
    /// Update component configuration fields
    ///
    /// Supports dedicated flags for common fields (e.g., --local-path, --changelog-target)
    /// as well as --json/--base64 for arbitrary object updates.
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
        /// Extract command to run after upload (e.g., "unzip -o {{artifact}} && rm {{artifact}}")
        #[arg(long)]
        extract_command: Option<String>,
        /// Path to changelog file relative to localPath
        #[arg(long)]
        changelog_target: Option<String>,

        /// Version targets in the form "file" or "file::pattern" (repeatable).
        /// Same format as `component create --version-target`.
        #[arg(long = "version-target", value_name = "TARGET")]
        version_targets: Vec<String>,

        /// Extension(s) this component uses (e.g., a runtime/framework extension id). Repeatable.
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
    /// Detect runtime environment requirements from the component's source files.
    ///
    /// Reads extension-specific metadata to determine what runtime versions the
    /// component needs. Outputs generic runtime requirement JSON suitable for CI
    /// environment setup.
    Env {
        /// Component ID (optional when --path is provided)
        id: Option<String>,
        /// Discover component from a directory's homeboy.json
        #[arg(long)]
        path: Option<String>,
    },
    /// Prepare a component for build/test: install its declared extensions and
    /// install its dependencies through detected providers.
    ///
    /// Core-owned replacement for hardcoded CI install/refresh + per-ecosystem
    /// dependency setup. The package manager is chosen by detection and manifest
    /// config, never by shell literals. CI calls this instead of orchestrating
    /// the sequence itself.
    Setup {
        /// Component ID (optional when --path is provided)
        id: Option<String>,
        /// Discover component from a directory's homeboy.json
        #[arg(long)]
        path: Option<String>,
        /// Source (git URL or local path) to install the component's configured
        /// extensions from. Omit to skip extension install (deps only).
        #[arg(long)]
        source: Option<String>,
        /// Skip the dependency install step (extensions only).
        #[arg(long)]
        skip_dependencies: bool,
    },
    /// Inspect and optionally repair stale standalone registry local_path data
    Reconcile {
        /// Component ID
        id: String,
        /// Apply a safe discovered repair instead of reporting only
        #[arg(long)]
        apply: bool,
    },
    /// Report or remove declared reconstructable artifacts for a component
    Artifacts {
        /// Component ID (optional when --path is provided or run from a component checkout)
        id: Option<String>,
        /// Discover component from a directory's homeboy.json instead of the registry
        #[arg(long)]
        path: Option<String>,
        /// Remove reported artifact paths instead of dry-run reporting only
        #[arg(long)]
        apply: bool,
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

pub type ComponentOutput = EntityCrudOutput<Value, ComponentExtra>;

pub fn run(
    args: ComponentArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ComponentOutput> {
    match args.command {
        ComponentCommand::Create {
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
            let local_path = local_path.ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
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
            let id = derive_component_id_for_create(repo_path, &local_path)?;
            let mut new_component =
                Component::new(id.clone(), local_path.clone(), remote_path, build_artifact);

            new_component.version_targets = if let Some(json_spec) = version_targets_json {
                let raw = homeboy::core::config::read_json_spec_to_string(&json_spec)?;
                serde_json::from_str::<Vec<homeboy::core::component::VersionTarget>>(&raw)
                    .map_err(|e| {
                        homeboy::core::Error::validation_invalid_json(
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
            // Respect an explicit --changelog-target flag; otherwise auto-detect
            // the actual changelog location on disk so generated homeboy.json
            // files don't ship with a path that doesn't exist. (#1128)
            new_component.changelog_target = changelog_target.or_else(|| {
                homeboy::core::release::changelog::discover_changelog_relative_path(repo_path)
            });

            if !extensions.is_empty() {
                let mut extension_map = std::collections::HashMap::new();
                for extension_id in extensions {
                    extension_map.insert(extension_id, component::ScopedExtensionConfig::default());
                }
                new_component.extensions = Some(extension_map);
            }

            component::write_portable_config(repo_path, &new_component)?;

            // Always persist a standalone registration so the component is
            // discoverable by ID from any directory (#1131). This is a
            // lightweight pointer file in ~/.config/homeboy/components/<id>.json.
            if let Err(e) =
                homeboy::core::component::inventory::write_standalone_registration(&new_component)
            {
                eprintln!("Warning: could not write standalone registration: {}", e);
            }

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
                        "Attach to a project to enable deploy:\n  homeboy project components attach-path {} {}",
                        project_id, local_path
                    ),
                    None => format!(
                        "Component registered. Attach to a project for deploy:\n  homeboy project components attach-path <project> {}",
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
        ComponentCommand::Show { id, path } => show(id.as_deref(), path.as_deref()),
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
        ComponentCommand::Env { id, path } => env::env(id.as_deref(), path.as_deref()),
        ComponentCommand::Setup {
            id,
            path,
            source,
            skip_dependencies,
        } => setup_component(
            id.as_deref(),
            path.as_deref(),
            source.as_deref(),
            skip_dependencies,
        ),
        ComponentCommand::Reconcile { id, apply } => reconcile(&id, apply),
        ComponentCommand::Artifacts { id, path, apply } => {
            artifacts(id.as_deref(), path.as_deref(), apply)
        }
    }
}

fn artifacts(id: Option<&str>, path: Option<&str>, apply: bool) -> CmdResult<ComponentOutput> {
    let component =
        component::resolve_effective(id, path, None).map_err(|e| e.with_contextual_hint())?;
    let report = homeboy::core::component::cleanup_artifact_report(&component, apply)?;
    let updated_fields = if apply && report.applied_count > 0 {
        vec!["cleanup_artifacts".to_string()]
    } else {
        Vec::new()
    };
    let hint = if apply {
        Some(format!(
            "Removed {} reconstructable artifact path(s); source files outside declared artifacts were not touched.",
            report.applied_count
        ))
    } else {
        Some(
            "Dry run only. Re-run with `--apply` to remove existing declared artifacts."
                .to_string(),
        )
    };

    Ok((
        ComponentOutput {
            command: "component.artifacts".to_string(),
            id: Some(report.component_id.clone()),
            entity: Some(serde_json::to_value(&report).map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "component.artifacts",
                    "Failed to serialize artifact cleanup report",
                    Some(error.to_string()),
                    None,
                )
            })?),
            hint,
            updated_fields,
            ..Default::default()
        },
        0,
    ))
}

fn setup_component(
    id: Option<&str>,
    path: Option<&str>,
    source: Option<&str>,
    skip_dependencies: bool,
) -> CmdResult<ComponentOutput> {
    let result = homeboy::core::setup::component_setup(
        id,
        path,
        &homeboy::core::setup::ComponentSetupOptions {
            extension_source: source,
            skip_dependencies,
        },
    )
    .map_err(|e| e.with_contextual_hint())?;

    let component_id = result.component_id.clone();
    let entity = serde_json::to_value(&result).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "component.setup",
            "Failed to serialize component setup result",
            Some(error.to_string()),
            None,
        )
    })?;

    Ok((
        ComponentOutput {
            command: "component.setup".to_string(),
            id: Some(component_id),
            entity: Some(entity),
            ..Default::default()
        },
        0,
    ))
}

fn reconcile(id: &str, apply: bool) -> CmdResult<ComponentOutput> {
    let report = component::reconcile_standalone_registration(id, apply)?;
    let hint = if report.applied {
        Some("Standalone registration repaired".to_string())
    } else if report.repair.is_some() {
        Some(format!(
            "Safe repair available. Run `homeboy component reconcile {} --apply` to update the standalone registration.",
            id
        ))
    } else if report.status == "ok" {
        Some(
            "Standalone registration local_path is present and points to a git checkout"
                .to_string(),
        )
    } else {
        Some(
            "No safe repair path discovered; update the component registration manually"
                .to_string(),
        )
    };

    Ok((
        ComponentOutput {
            command: "component.reconcile".to_string(),
            id: Some(id.to_string()),
            entity: Some(serde_json::to_value(&report).map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "component.reconcile",
                    "Failed to serialize reconcile report",
                    Some(error.to_string()),
                    None,
                )
            })?),
            hint,
            updated_fields: if report.applied {
                vec!["local_path".to_string()]
            } else {
                Vec::new()
            },
            ..Default::default()
        },
        0,
    ))
}

/// Suggest a project for a newly created component based on sibling components.
///
/// Checks whether any existing project has components whose local_path shares the
/// same parent directory as the new component's path. If a project is found with
/// siblings in the same workspace directory, it's the most likely target.
fn suggest_project_for_path(local_path: &str) -> Option<String> {
    let new_parent = Path::new(local_path).parent()?;
    let projects = project::list().ok()?;

    for project in &projects {
        for attachment in &project.components {
            if let Some(existing_parent) = Path::new(&attachment.local_path).parent() {
                if existing_parent == new_parent {
                    return Some(project.id.clone());
                }
            }
        }
    }

    None
}

fn derive_component_id_for_create(
    repo_path: &Path,
    local_path: &str,
) -> homeboy::core::Result<String> {
    if repo_path.join("homeboy.json").exists() {
        return component::infer_portable_component_id(repo_path);
    }

    let dir_name = repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "local_path",
                "Could not derive component ID from local path",
                Some(local_path.to_string()),
                None,
            )
        })?;

    homeboy::core::engine::identifier::slugify_id(dir_name, "component_id")
}

fn show(id: Option<&str>, path: Option<&str>) -> CmdResult<ComponentOutput> {
    let component = component::resolve_target(component::TargetSpec::new(id, path))
        .map_err(|e| {
            if id.is_none() && path.is_none() {
                homeboy::core::Error::validation_missing_argument(vec!["id or --path".to_string()])
            } else {
                e.with_contextual_hint()
            }
        })?
        .component;

    component.validate_supported_build_config()?;

    let resolved_id = component.id.clone();
    let drift_files = homeboy::core::component::drift::drift_file_paths(&component);

    Ok((
        ComponentOutput {
            command: "component.show".to_string(),
            id: Some(resolved_id.clone()),
            entity: Some(component_discovery_value(&component, Some(drift_files))?),
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
    extract_command: Option<String>,
    changelog_target: Option<String>,
}

impl ComponentSetFlags {
    fn has_any(&self) -> bool {
        self.local_path.is_some()
            || self.remote_path.is_some()
            || self.build_artifact.is_some()
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
    let has_dynamic = args.json_spec()?.is_some();
    if !has_dynamic && !flags.has_any() && version_targets.is_empty() && extensions.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "spec",
            "Provide a dedicated flag (e.g., --local-path), --json '<object>', --base64 <encoded-json>, --version-target, or --extension",
            None,
            Some(vec![
                "Arbitrary field updates must use --json or --base64.".to_string(),
                "Example: homeboy component set <id> --json '{\"remote_path\":\"deploy/path\"}'".to_string(),
            ]),
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
            return Err(homeboy::core::Error::validation_invalid_argument(
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
        homeboy::core::MergeOutput::Single(result) => {
            let comp = component::load(&result.id)?;
            Ok((
                ComponentOutput {
                    command: "component.set".to_string(),
                    id: Some(result.id),
                    updated_fields: result.updated_fields,
                    entity: Some({
                        let mut value = serde_json::to_value(&comp).map_err(|error| {
                            homeboy::core::Error::validation_invalid_argument(
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
        homeboy::core::MergeOutput::Bulk(summary) => {
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
            entity: Some({
                let mut value = serde_json::to_value(&component).map_err(|error| {
                    homeboy::core::Error::validation_invalid_argument(
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

fn list() -> CmdResult<ComponentOutput> {
    let components: Vec<Value> = component::inventory()?
        .into_iter()
        .map(|component| component_discovery_value(&component, None))
        .collect::<homeboy::core::Result<Vec<Value>>>()?;

    Ok((
        ComponentOutput {
            command: "component.list".to_string(),
            entities: components,
            ..Default::default()
        },
        0,
    ))
}

fn component_discovery_value(
    component: &Component,
    drift_files: Option<Vec<String>>,
) -> homeboy::core::Result<Value> {
    let mut value = serde_json::to_value(component).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "component",
            "Failed to serialize component",
            Some(error.to_string()),
            None,
        )
    })?;
    if let Value::Object(ref mut map) = value {
        map.insert("id".to_string(), Value::String(component.id.clone()));
        // Always surface remote_owner so missing config is visible (#602).
        map.entry("remote_owner".to_string()).or_insert(Value::Null);
        let local_path_diagnostic = component::inventory::local_path_diagnostic(component);
        if local_path_diagnostic.status != "ok" {
            map.insert(
                "local_path_diagnostic".to_string(),
                serde_json::to_value(local_path_diagnostic).map_err(|error| {
                    homeboy::core::Error::validation_invalid_argument(
                        "component",
                        "Failed to serialize local_path diagnostic",
                        Some(error.to_string()),
                        None,
                    )
                })?,
            );
        }
        summarize_large_component_metadata(map);
        if let Some(drift_files) = drift_files {
            // Computed: extension-declared lockfiles + component extras + the
            // audit baseline. Consumed by homeboy-action to scope direct-push drift.
            map.insert(
                "drift_files".to_string(),
                Value::Array(drift_files.into_iter().map(Value::String).collect()),
            );
        }
    }
    Ok(value)
}

fn summarize_large_component_metadata(map: &mut serde_json::Map<String, Value>) {
    let baselines = map.remove("baselines");
    let audit_rules = map.remove("audit_rules");
    let mut summary = serde_json::Map::new();

    if let Some(Value::Object(baselines)) = baselines {
        summary.insert(
            "baseline_keys".to_string(),
            Value::Array(baselines.keys().cloned().map(Value::String).collect()),
        );
        if let Some(audit) = baselines.get("audit") {
            summary.insert("audit_baseline".to_string(), audit_baseline_summary(audit));
        }
    }

    if let Some(audit_rules) = audit_rules {
        summary.insert("audit_rules".to_string(), value_shape_summary(&audit_rules));
    }

    if !summary.is_empty() {
        map.insert("metadata_summary".to_string(), Value::Object(summary));
    }
}

fn audit_baseline_summary(value: &Value) -> Value {
    let mut summary = serde_json::Map::new();
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            if key == "known_fingerprints" {
                summary.insert(
                    "known_fingerprints_count".to_string(),
                    Value::from(value.as_array().map_or(0, Vec::len)),
                );
            } else {
                summary.insert(key.clone(), value_shape_summary(value));
            }
        }
    }
    Value::Object(summary)
}

fn value_shape_summary(value: &Value) -> Value {
    match value {
        Value::Array(items) => serde_json::json!({ "type": "array", "count": items.len() }),
        Value::Object(object) => serde_json::json!({
            "type": "object",
            "keys": object.keys().cloned().collect::<Vec<_>>()
        }),
        _ => value.clone(),
    }
}

fn projects(id: &str) -> CmdResult<ComponentOutput> {
    let project_ids = component::associated_projects(id)?;

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
        let project_ids = component::associated_projects(component_id)?;
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
    use crate::cli_surface::Cli;
    use clap::CommandFactory;
    use std::fs;

    #[test]
    fn test_component_set_flags_has_any_all_none() {
        let flags = ComponentSetFlags {
            local_path: None,
            remote_path: None,
            build_artifact: None,
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
            extract_command: Some("unzip -o artifact.zip".to_string()),
            changelog_target: Some("CHANGELOG.md".to_string()),
        };

        let mut obj = serde_json::Map::new();
        flags.apply_to(&mut obj);

        assert_eq!(obj.len(), 3);
        assert_eq!(obj["local_path"], serde_json::json!("/new/path"));
        assert_eq!(
            obj["extract_command"],
            serde_json::json!("unzip -o artifact.zip")
        );
        assert_eq!(obj["changelog_target"], serde_json::json!("CHANGELOG.md"));
        assert!(!obj.contains_key("remote_path"));
    }

    #[test]
    fn test_component_set_flags_apply_to_overrides_existing() {
        let flags = ComponentSetFlags {
            local_path: Some("/override".to_string()),
            remote_path: None,
            build_artifact: None,
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

    #[test]
    fn component_help_uses_double_brace_extract_command_placeholder() {
        let mut command = Cli::command();
        let component = command
            .find_subcommand_mut("component")
            .expect("component command");
        let create_help = component
            .find_subcommand_mut("create")
            .expect("component create command")
            .render_long_help()
            .to_string();
        let set_help = component
            .find_subcommand_mut("set")
            .expect("component set command")
            .render_long_help()
            .to_string();
        let help = format!("{create_help}\n{set_help}");

        assert!(
            help.contains("unzip -o {{artifact}} && rm {{artifact}}"),
            "component help should document double-brace placeholders: {help}"
        );
        assert!(
            !help.contains("unzip -o {artifact} && rm {artifact}"),
            "component help should not document single-brace placeholders: {help}"
        );
    }

    #[test]
    fn component_create_id_prefers_existing_portable_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("homeboy@fix-triage-default-workspace");
        fs::create_dir_all(&repo).expect("repo dir");
        fs::write(repo.join("homeboy.json"), r#"{"id":"homeboy"}"#).expect("homeboy.json");

        let id = derive_component_id_for_create(&repo, &repo.to_string_lossy())
            .expect("portable id should be used");

        assert_eq!(id, "homeboy");
    }

    #[test]
    fn component_discovery_value_summarizes_large_metadata() {
        let mut component = Component::new(
            "homeboy".to_string(),
            "/repo/homeboy".to_string(),
            "wp-content/plugins/homeboy".to_string(),
            None,
        );
        component.baselines = Some(std::collections::HashMap::from([(
            "audit".to_string(),
            serde_json::json!({
                "known_fingerprints": ["a", "b", "c"],
                "item_count": 3
            }),
        )]));
        component.audit_rules = Some(serde_json::json!({
            "layer_rules": [{"id":"one"}],
            "source_policies": [{"id":"two"}]
        }));

        let value = component_discovery_value(&component, None).expect("discovery value");

        assert!(value.get("baselines").is_none());
        assert!(value.get("audit_rules").is_none());
        assert_eq!(
            value["metadata_summary"]["audit_baseline"]["known_fingerprints_count"],
            3
        );
        assert_eq!(
            value["metadata_summary"]["baseline_keys"],
            serde_json::json!(["audit"])
        );
    }

    #[test]
    fn component_discovery_value_flags_temp_local_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let checkout = temp.path().join("opencode").join("homeboy-temp");
        fs::create_dir_all(checkout.join(".git")).expect("git dir");
        fs::write(checkout.join("homeboy.json"), r#"{"id":"homeboy"}"#).expect("homeboy.json");
        let component = Component::new(
            "homeboy".to_string(),
            checkout.to_string_lossy().to_string(),
            String::new(),
            None,
        );

        let value = component_discovery_value(&component, None).expect("discovery value");

        assert_eq!(
            value["local_path_diagnostic"]["status"],
            serde_json::json!("temp_checkout")
        );
        assert_eq!(
            value["local_path_diagnostic"]["exists"],
            serde_json::json!(true)
        );
        assert_eq!(
            value["local_path_diagnostic"]["is_git_checkout"],
            serde_json::json!(true)
        );
        assert!(value["local_path_diagnostic"]["warning"]
            .as_str()
            .unwrap_or_default()
            .contains("temporary/opencode checkout"));
        assert!(value["local_path_diagnostic"]["repair_command"]
            .as_str()
            .unwrap_or_default()
            .contains("homeboy component set homeboy --local-path"));
    }
}

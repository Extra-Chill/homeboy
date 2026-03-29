use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
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
    /// Initialize portable component config for a repo
    Create {
        /// JSON input spec for create/update (supports single or bulk)
        #[arg(long)]
        json: Option<String>,

        /// Skip items that already exist (JSON mode only)
        #[arg(long)]
        skip_existing: bool,

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
        /// Extract command to run after upload (e.g., "unzip -o {artifact} && rm {artifact}")
        #[arg(long)]
        extract_command: Option<String>,
        /// Path to changelog file relative to localPath
        #[arg(long)]
        changelog_target: Option<String>,
        /// Extension(s) this component uses (e.g., "wordpress"). Repeatable.
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
    /// Detect runtime environment requirements from the component's source files.
    ///
    /// Reads extension-specific metadata (e.g., WordPress "Requires PHP" header)
    /// to determine what runtime versions the component needs. Outputs JSON
    /// suitable for CI environment setup.
    Env {
        /// Component ID (optional when --path is provided)
        id: Option<String>,
        /// Discover component from a directory's homeboy.json
        #[arg(long)]
        path: Option<String>,
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

pub type ComponentOutput = EntityCrudOutput<Value, ComponentExtra>;

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
        ComponentCommand::Env { id, path } => env(id.as_deref(), path.as_deref()),
        ComponentCommand::AddVersionTarget { id, file, pattern } => {
            add_version_target(&id, &file, &pattern)
        }
    }
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

fn show(id: Option<&str>, path: Option<&str>) -> CmdResult<ComponentOutput> {
    let component = match (id, path) {
        // --path: discover from directory's homeboy.json
        (_, Some(dir)) => {
            let dir_path = std::path::Path::new(dir);
            component::resolve_effective(id, Some(dir), None).map_err(|_| {
                homeboy::Error::validation_invalid_argument(
                    "path",
                    format!(
                        "No homeboy.json found at {} and no registered component matches",
                        dir_path.display()
                    ),
                    None,
                    Some(vec![
                        format!("Create homeboy.json in {}", dir_path.display()),
                        "Or provide a registered component ID".to_string(),
                    ]),
                )
            })?
        }
        // ID only: load from registry
        (Some(comp_id), None) => component::load(comp_id).map_err(|e| e.with_contextual_hint())?,
        // Neither: try CWD discovery
        (None, None) => component::resolve_effective(None, None, None).map_err(|_| {
            homeboy::Error::validation_missing_argument(vec!["id or --path".to_string()])
        })?,
    };

    let resolved_id = component.id.clone();

    Ok((
        ComponentOutput {
            command: "component.show".to_string(),
            id: Some(resolved_id.clone()),
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
                    map.insert("id".to_string(), Value::String(resolved_id));
                }
                value
            }),
            ..Default::default()
        },
        0,
    ))
}

/// Runtime environment requirements detected from the component's source files.
#[derive(Debug, Serialize)]
struct ComponentEnvOutput {
    command: String,
    id: String,
    extension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    php: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<String>,
}

fn env(id: Option<&str>, path: Option<&str>) -> CmdResult<ComponentOutput> {
    let component = match (id, path) {
        // --path with explicit ID
        (Some(comp_id), Some(dir)) => component::resolve_effective(Some(comp_id), Some(dir), None)
            .map_err(|e| e.with_contextual_hint())?,
        // --path without ID: discover from the directory's homeboy.json
        (None, Some(dir)) => {
            let dir_path = Path::new(dir);
            component::portable::discover_from_portable(dir_path).ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "path",
                    format!("No homeboy.json found at {}", dir_path.display()),
                    None,
                    Some(vec![format!(
                        "Create homeboy.json in {}",
                        dir_path.display()
                    )]),
                )
            })?
        }
        // ID only
        (Some(comp_id), None) => component::resolve_effective(Some(comp_id), None, None)
            .map_err(|e| e.with_contextual_hint())?,
        // Neither: try CWD discovery
        (None, None) => component::resolve_effective(None, None, None).map_err(|_| {
            homeboy::Error::validation_missing_argument(vec!["id or --path".to_string()])
        })?,
    };

    let comp_id = component.id.clone();
    let local_path = Path::new(&component.local_path);

    // Determine the primary extension
    let extension_id = component
        .extensions
        .as_ref()
        .and_then(|exts| exts.keys().next().cloned());

    let mut php_version: Option<String> = None;
    let mut node_version: Option<String> = None;

    // Read node version from raw homeboy.json (the Rust struct drops unknown
    // fields like "php" and "node" from the extension config during deserialization).
    if let Some(ref ext_id) = extension_id {
        let config_path = local_path.join("homeboy.json");
        if let Ok(raw) = std::fs::read_to_string(&config_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(ext_obj) = json.get("extensions").and_then(|e| e.get(ext_id.as_str())) {
                    if let Some(v) = ext_obj.get("node").and_then(|v| v.as_str()) {
                        node_version = Some(v.to_string());
                    }
                    // Read php from homeboy.json as fallback (overridden below by header detection)
                    if let Some(v) = ext_obj.get("php").and_then(|v| v.as_str()) {
                        php_version = Some(v.to_string());
                    }
                }
            }
        }
    }

    // For WordPress extensions: detect Requires PHP from plugin/theme header.
    // This takes priority over homeboy.json since the header is the source of truth.
    if extension_id.as_deref() == Some("wordpress") {
        if let Some(detected_php) = detect_wordpress_requires_php(local_path) {
            php_version = Some(detected_php);
        }
    }

    let env_output = ComponentEnvOutput {
        command: "component.env".to_string(),
        id: comp_id.clone(),
        extension: extension_id,
        php: php_version,
        node: node_version,
    };

    let entity = serde_json::to_value(&env_output).map_err(|error| {
        homeboy::Error::validation_invalid_argument(
            "component",
            "Failed to serialize env output",
            Some(error.to_string()),
            None,
        )
    })?;

    Ok((
        ComponentOutput {
            command: "component.env".to_string(),
            id: Some(comp_id),
            entity: Some(entity),
            ..Default::default()
        },
        0,
    ))
}

/// Parse "Requires PHP: X.Y" from a WordPress plugin or theme header.
fn detect_wordpress_requires_php(component_path: &Path) -> Option<String> {
    // Check theme first (style.css)
    let style_css = component_path.join("style.css");
    if style_css.exists() {
        if let Some(version) = grep_header_value(&style_css, "Requires PHP:") {
            if grep_header_value(&style_css, "Theme Name:").is_some() {
                return Some(version);
            }
        }
    }

    // Check plugin (*.php files in root with "Plugin Name:" header)
    if let Ok(entries) = std::fs::read_dir(component_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("php")
                && grep_header_value(&path, "Plugin Name:").is_some()
            {
                if let Some(version) = grep_header_value(&path, "Requires PHP:") {
                    return Some(version);
                }
                // Found plugin file but no Requires PHP header
                return None;
            }
        }
    }

    None
}

/// Read a "Key: Value" header line from a file (first 100 lines only).
fn grep_header_value(file: &Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(file).ok()?;
    for line in content.lines().take(100) {
        if let Some(pos) = line.find(key) {
            let value = line[pos + key.len()..].trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
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

fn list() -> CmdResult<ComponentOutput> {
    let components: Vec<Value> = component::inventory()?
        .into_iter()
        .map(|component| {
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
                // Always surface remote_owner so missing config is visible (#602).
                // Serde skips None fields, but for list output we want explicit null
                // so users can audit which components are missing this critical config.
                map.entry("remote_owner".to_string()).or_insert(Value::Null);
            }
            Ok(value)
        })
        .collect::<homeboy::Result<Vec<Value>>>()?;

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
}

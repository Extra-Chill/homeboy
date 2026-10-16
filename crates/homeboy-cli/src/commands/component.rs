use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::Instant;

use homeboy::core::component::{self, Component};
use homeboy::core::project::{self, Project};
use homeboy::core::EntityCrudOutput;

use super::utils::args::MutationArgs;
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
        /// Accepted for consistency with other structured-output commands.
        #[arg(long, hide = true)]
        json: bool,
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

        /// Owning extension for a contested capability, as
        /// "<capability>=<extension>". Repeatable.
        ///
        /// Resolves the ambiguity reported when several linked extensions
        /// provide the same thing. Accepts the capability labels (build, lint,
        /// test, bench, fuzz, trace, deps) and the non-capability ownership
        /// surfaces (remote_path, since_tag, provides.file_extensions).
        ///
        /// Example: --capability-extension build=wordpress
        #[arg(long = "capability-extension", value_name = "CAPABILITY=EXTENSION")]
        capability_extensions: Vec<String>,
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
    /// List registered components without opening their checkouts
    List {
        /// Enrich every row from its checkout, including portable and Git metadata
        #[arg(long)]
        full: bool,
    },
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
        // Apply a safe discovered repair instead of reporting only.
        // Shared plan-default mutation group (#11139).
        #[command(flatten)]
        mutation: MutationArgs,
    },
    /// Report or remove declared reconstructable artifacts for a component
    Artifacts {
        /// Component ID (optional when --path is provided or run from a component checkout)
        id: Option<String>,
        /// Discover component from a directory's homeboy.json instead of the registry
        #[arg(long)]
        path: Option<String>,
        // Remove reported artifact paths instead of dry-run reporting only.
        // Shared plan-default mutation group (#11139).
        #[command(flatten)]
        mutation: MutationArgs,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<ComponentInventorySummary>,
}

#[derive(Debug, Serialize)]
pub struct ComponentInventorySummary {
    pub status: String,
    pub registered_count: usize,
    pub enriched_count: usize,
    pub omitted_enrichment_count: usize,
    pub phases: Vec<ComponentInventoryPhase>,
    pub continue_command: String,
}

#[derive(Debug, Serialize)]
pub struct ComponentInventoryPhase {
    pub name: String,
    pub elapsed_ms: u128,
}

pub type ComponentOutput = EntityCrudOutput<Value, ComponentExtra>;

pub fn run(args: ComponentArgs) -> CmdResult<ComponentOutput> {
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

            let repo_path = Path::new(&local_path);
            let id = derive_component_id_for_create(repo_path, &local_path)?;

            let explicit_version_targets = if let Some(json_spec) = version_targets_json {
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

            let overlay = CreateOverlay {
                remote_path,
                build_artifact,
                version_targets: explicit_version_targets,
                extract_command,
                changelog_target,
                extensions,
            };

            let new_component = if repo_path.join("homeboy.json").exists() {
                // Existing repo-owned config is the source of truth: load it and
                // overlay only the flags passed explicitly (#16027).
                create_from_existing_portable(repo_path, &local_path, overlay)?
            } else {
                let mut new_component =
                    Component::new(id.clone(), local_path.clone(), String::new(), None);
                overlay.apply(&mut new_component);
                // Respect an explicit --changelog-target flag; otherwise auto-detect
                // the actual changelog location on disk so generated homeboy.json
                // files don't ship with a path that doesn't exist. (#1128)
                if new_component.changelog_target.is_none() {
                    new_component.changelog_target =
                        homeboy_release::release::changelog::discover_changelog_relative_path(
                            repo_path,
                        );
                }
                component::write_portable_config(repo_path, &new_component)?;
                new_component
            };

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
        ComponentCommand::Show { id, path, .. } => show(id.as_deref(), path.as_deref()),
        ComponentCommand::Set {
            args,
            local_path,
            remote_path,
            build_artifact,
            extract_command,
            changelog_target,
            version_targets,
            extensions,
            capability_extensions,
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
            capability_extensions,
        ),
        ComponentCommand::Delete { id } => delete(&id),
        ComponentCommand::Rename { id, new_id } => rename(&id, &new_id),
        ComponentCommand::List { full } => list(full),
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
        ComponentCommand::Reconcile { id, mutation } => reconcile(&id, mutation.is_apply()),
        ComponentCommand::Artifacts { id, path, mutation } => {
            artifacts(id.as_deref(), path.as_deref(), mutation.is_apply())
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

/// Fields explicitly passed to `component create`. `None`/empty means the flag
/// was omitted and the corresponding field must not be touched.
struct CreateOverlay {
    remote_path: Option<String>,
    build_artifact: Option<String>,
    version_targets: Option<Vec<component::VersionTarget>>,
    extract_command: Option<String>,
    changelog_target: Option<String>,
    extensions: Vec<String>,
}

impl CreateOverlay {
    fn apply(self, component: &mut Component) {
        if let Some(remote_path) = self.remote_path {
            component.remote_path = remote_path;
        }
        if let Some(build_artifact) = self.build_artifact {
            component.build_artifact = Some(build_artifact);
        }
        if let Some(version_targets) = self.version_targets {
            component.version_targets = Some(version_targets);
        }
        if let Some(extract_command) = self.extract_command {
            component.extract_command = Some(extract_command);
        }
        if let Some(changelog_target) = self.changelog_target {
            component.changelog_target = Some(changelog_target);
        }
        if !self.extensions.is_empty() {
            let extension_map = component.extensions.get_or_insert_with(Default::default);
            for extension_id in self.extensions {
                extension_map.entry(extension_id).or_default();
            }
        }
    }
}

/// Load an existing `<repo>/homeboy.json`, overlay explicit create flags, and
/// write it back only when the overlay changed the portable config.
fn create_from_existing_portable(
    repo_path: &Path,
    local_path: &str,
    overlay: CreateOverlay,
) -> homeboy::core::Result<Component> {
    let mut existing = component::try_discover_from_portable(repo_path)?.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "local_path",
            format!("No homeboy.json found at {}", repo_path.display()),
            None,
            None,
        )
    })?;
    existing.local_path = local_path.to_string();

    // Discovery may enrich `remote_url` from git; that is not repo-owned
    // config, so keep it out of the change comparison and any write-back.
    let file_has_remote_url = std::fs::read_to_string(repo_path.join("homeboy.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .is_some_and(|value| value.get("remote_url").is_some());

    let mut updated = existing.clone();
    overlay.apply(&mut updated);

    let before = component::portable_json(&existing)?;
    let after = component::portable_json(&updated)?;
    if before != after {
        let mut to_write = updated.clone();
        if !file_has_remote_url {
            to_write.remote_url = None;
        }
        component::write_portable_config(repo_path, &to_write)?;
    }

    Ok(updated)
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
    capability_extensions: Vec<String>,
) -> CmdResult<ComponentOutput> {
    // Check if there's any input at all
    let has_dynamic = args.json_spec()?.is_some();
    if !has_dynamic
        && !flags.has_any()
        && version_targets.is_empty()
        && extensions.is_empty()
        && capability_extensions.is_empty()
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "spec",
            "Provide a dedicated flag (e.g., --local-path), --json '<object>', --base64 <encoded-json>, --version-target, --extension, or --capability-extension",
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

    // Support --capability-extension. This is the runnable command the
    // ownership-ambiguity error now points at (#11120), so it must land in the
    // same `capability_extensions` map that `resolve_owner` reads.
    if !capability_extensions.is_empty() {
        let parsed = component::parse_capability_extensions(&capability_extensions)?;
        if let serde_json::Value::Object(ref mut obj) = merged {
            obj.insert(
                "capability_extensions".to_string(),
                serde_json::json!(parsed),
            );
        } else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "spec",
                "Merged spec must be a JSON object",
                None,
                None,
            ));
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

fn list(full: bool) -> CmdResult<ComponentOutput> {
    let started = Instant::now();
    let components = if full {
        component::inventory()?
    } else {
        component::inventory::registered_base()?
    };
    let discovery_elapsed_ms = started.elapsed().as_millis();
    let component_count = components.len();
    let rendering_started = Instant::now();
    let components: Vec<Value> = components
        .into_iter()
        .map(|component| {
            let mut value = component_discovery_value_with_diagnostics(&component, None, full)?;
            if let Value::Object(ref mut map) = value {
                map.insert(
                    "inventory_enrichment".to_string(),
                    if full {
                        serde_json::json!({ "status": "complete" })
                    } else {
                        serde_json::json!({
                            "status": "omitted",
                            "reason": "default_bounded_inventory",
                            "continue_command": "homeboy component list --full"
                        })
                    },
                );
            }
            Ok(value)
        })
        .collect::<homeboy::core::Result<Vec<Value>>>()?;
    let rendering_elapsed_ms = rendering_started.elapsed().as_millis();

    Ok((
        ComponentOutput {
            command: "component.list".to_string(),
            entities: Some(components),
            hint: (!full).then(|| {
                "Registry rows are shown without checkout enrichment; run `homeboy component list --full` for portable and Git metadata."
                    .to_string()
            }),
            extra: ComponentExtra {
                inventory: Some(ComponentInventorySummary {
                    status: if full { "complete" } else { "partial" }.to_string(),
                    registered_count: component_count,
                    enriched_count: if full { component_count } else { 0 },
                    omitted_enrichment_count: if full { 0 } else { component_count },
                    phases: vec![
                        ComponentInventoryPhase {
                            name: if full { "discovery_and_enrichment" } else { "registry_rows" }
                                .to_string(),
                            elapsed_ms: discovery_elapsed_ms,
                        },
                        ComponentInventoryPhase {
                            name: "render_rows".to_string(),
                            elapsed_ms: rendering_elapsed_ms,
                        },
                    ],
                    continue_command: "homeboy component list --full".to_string(),
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn component_discovery_value(
    component: &Component,
    drift_files: Option<Vec<String>>,
) -> homeboy::core::Result<Value> {
    component_discovery_value_with_diagnostics(component, drift_files, true)
}

fn component_discovery_value_with_diagnostics(
    component: &Component,
    drift_files: Option<Vec<String>>,
    include_diagnostics: bool,
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
        if include_diagnostics {
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
        }
        summarize_large_component_metadata(map);
        if let Some(drift_files) = drift_files {
            // Computed: extension-declared lockfiles + component extras + the
            // baseline file. Consumed by homeboy-action to scope direct-push drift.
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
    let mut summary = serde_json::Map::new();

    if let Some(Value::Object(baselines)) = baselines {
        summary.insert(
            "baseline_keys".to_string(),
            Value::Array(baselines.keys().cloned().map(Value::String).collect()),
        );
    }

    if !summary.is_empty() {
        map.insert("metadata_summary".to_string(), Value::Object(summary));
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
    use clap::{CommandFactory, Parser};
    use std::fs;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        component: ComponentArgs,
    }

    #[test]
    fn parses_component_show_json_flag() {
        let cli = TestCli::try_parse_from(["component", "show", "homeboy", "--json"])
            .expect("component show --json should parse");

        match cli.component.command {
            ComponentCommand::Show { id, json, .. } => {
                assert_eq!(id.as_deref(), Some("homeboy"));
                assert!(json);
            }
            _ => panic!("expected component show command"),
        }
    }

    #[test]
    fn parses_component_list_full_flag() {
        let cli = TestCli::try_parse_from(["component", "list", "--full"])
            .expect("component list --full should parse");

        assert!(matches!(
            cli.component.command,
            ComponentCommand::List { full: true }
        ));
    }

    #[test]
    fn empty_component_list_emits_entities_array() {
        crate::test_support::with_isolated_home(|_| {
            let (output, _) = list(false).expect("component list");
            let value = serde_json::to_value(&output).expect("serialize component list");

            assert_eq!(value["command"], "component.list");
            assert_eq!(value["entities"], serde_json::json!([]));
        });
    }

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

    fn run_create(extra: &[&str], repo: &Path) -> CmdResult<ComponentOutput> {
        let repo_str = repo.to_string_lossy().to_string();
        let mut argv = vec!["component", "create", "--local-path", repo_str.as_str()];
        argv.extend_from_slice(extra);
        let cli = TestCli::try_parse_from(argv).expect("component create should parse");
        run(cli.component)
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("read json")).expect("parse json")
    }

    fn registration_path(id: &str) -> std::path::PathBuf {
        homeboy::core::paths::components()
            .expect("components dir")
            .join(format!("{id}.json"))
    }

    #[test]
    fn component_create_preserves_existing_config_byte_for_byte() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let repo = temp.path().join("my-plugin");
            fs::create_dir_all(&repo).expect("repo dir");
            fs::write(repo.join("CHANGELOG.md"), "# Changelog\n").expect("changelog");
            let original = r#"{
  "id": "my-plugin",
  "remote_path": "wp-content/plugins/my-plugin",
  "extensions": { "wordpress": {} },
  "version_targets": [
    { "file": "my-plugin.php", "pattern": "Version: (.*)" },
    { "file": "package.json" }
  ]
}
"#;
            fs::write(repo.join("homeboy.json"), original).expect("homeboy.json");

            run_create(&[], &repo).expect("create should succeed");

            let after = fs::read_to_string(repo.join("homeboy.json")).expect("read");
            assert_eq!(after, original, "homeboy.json must be unchanged");

            let registration = read_json(&registration_path("my-plugin"));
            assert_eq!(
                registration["local_path"],
                serde_json::json!(repo.to_string_lossy())
            );
        });
    }

    #[test]
    fn component_create_preserves_fields_when_merging_explicit_flags() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let repo = temp.path().join("my-plugin");
            fs::create_dir_all(&repo).expect("repo dir");
            fs::write(
                repo.join("homeboy.json"),
                r#"{
  "id": "my-plugin",
  "remote_path": "wp-content/plugins/my-plugin",
  "extract_command": "unzip -o {{artifact}}",
  "extensions": { "wordpress": { "settings": { "keep": true } } },
  "version_targets": [
    { "file": "my-plugin.php", "pattern": "Version: (.*)" },
    { "file": "package.json" }
  ]
}
"#,
            )
            .expect("homeboy.json");

            run_create(
                &[
                    "--extension",
                    "rust",
                    "--changelog-target",
                    "docs/CHANGELOG.md",
                ],
                &repo,
            )
            .expect("create should succeed");

            let after = read_json(&repo.join("homeboy.json"));
            assert_eq!(after["id"], "my-plugin");
            assert_eq!(after["remote_path"], "wp-content/plugins/my-plugin");
            assert_eq!(after["extract_command"], "unzip -o {{artifact}}");
            assert_eq!(after["changelog_target"], "docs/CHANGELOG.md");
            assert_eq!(
                after["extensions"]["wordpress"]["keep"],
                serde_json::json!(true),
                "existing extension settings must survive: {after}"
            );
            assert!(after["extensions"].get("rust").is_some());
            let targets = after["version_targets"].as_array().expect("targets");
            assert_eq!(targets.len(), 2);
            assert_eq!(targets[0]["file"], "my-plugin.php");
            assert_eq!(targets[1]["file"], "package.json");
            assert!(after.get("local_path").is_none());
            assert!(after.get("remote_url").is_none());
        });
    }

    #[test]
    fn component_create_preserves_invalid_config_and_errors() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let repo = temp.path().join("my-plugin");
            fs::create_dir_all(&repo).expect("repo dir");
            let original = "{ \"id\": \"my-plugin\", \"extensions\": ";
            fs::write(repo.join("homeboy.json"), original).expect("homeboy.json");

            let Err(err) = run_create(&["--extension", "rust"], &repo) else {
                panic!("invalid homeboy.json must error");
            };
            assert!(
                err.to_string().to_lowercase().contains("json"),
                "error should describe invalid JSON: {err}"
            );

            let after = fs::read_to_string(repo.join("homeboy.json")).expect("read");
            assert_eq!(after, original, "invalid homeboy.json must be untouched");
            assert!(!registration_path("my-plugin").exists());
        });
    }

    #[test]
    fn component_create_preserves_remote_path_when_flag_omitted() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let repo = temp.path().join("my-plugin");
            fs::create_dir_all(&repo).expect("repo dir");
            fs::write(
                repo.join("homeboy.json"),
                r#"{"id":"my-plugin","remote_path":"wp-content/plugins/my-plugin"}"#,
            )
            .expect("homeboy.json");

            run_create(&["--build-artifact", "build/my-plugin.zip"], &repo)
                .expect("create should succeed");

            let after = read_json(&repo.join("homeboy.json"));
            assert_eq!(after["remote_path"], "wp-content/plugins/my-plugin");
            assert_eq!(after["build_artifact"], "build/my-plugin.zip");
        });
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
            "lint".to_string(),
            serde_json::json!({
                "known_fingerprints": ["a", "b", "c"],
                "item_count": 3
            }),
        )]));

        let value = component_discovery_value(&component, None).expect("discovery value");

        assert!(value.get("baselines").is_none());
        assert_eq!(
            value["metadata_summary"]["baseline_keys"],
            serde_json::json!(["lint"])
        );
    }

    #[test]
    fn component_discovery_value_flags_temp_local_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let checkout = temp.path().join("tmp").join("homeboy-temp");
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
            .contains("transient workspace checkout"));
        assert!(value["local_path_diagnostic"]["repair_command"]
            .as_str()
            .unwrap_or_default()
            .contains("homeboy component set homeboy --local-path"));
    }
}

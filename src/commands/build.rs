use clap::Args;
use homeboy::core::build;
use homeboy::core::component;
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::extension::ExtensionCapability;
use homeboy::core::project;
use homeboy::core::scope::{self, Scope};

use crate::commands::utils::resolve::resolve_project_components;
use crate::commands::CmdResult;

#[derive(Args)]
pub struct BuildArgs {
    /// JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]}
    #[arg(long)]
    pub json: Option<String>,

    /// Target ID: component ID or project ID (when using --all)
    pub target_id: Option<String>,

    /// Additional component IDs (enables project/component order detection)
    pub component_ids: Vec<String>,

    /// Build all components in the project
    #[arg(long)]
    pub all: bool,

    /// Override local_path for this build (use a workspace clone or temp checkout)
    #[arg(long)]
    pub path: Option<String>,

    /// Ask the build provider to resolve the build scope from files changed since this git ref
    #[arg(long)]
    pub changed_since: Option<String>,
}

pub fn run(
    args: BuildArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<build::BuildResult> {
    // Priority: --json > --all with project > positional args

    // Shared tail: every multi-component build path dispatches the resolved
    // component records through the same changed-since runner with the same
    // `--changed-since` argument, so funnel them through one helper.
    let run_components = |components: &[component::Component]| {
        build::run_components_with_changed_since(components, args.changed_since.as_deref())
    };

    // JSON takes precedence
    if let Some(ref json) = args.json {
        return build::run(json);
    }

    // No target_id: try CWD auto-discovery (registered component or homeboy.json)
    if args.target_id.is_none() && args.component_ids.is_empty() && !args.all {
        let ctx = execution_context::resolve(&ResolveOptions::with_capability(
            // Use empty string for CWD auto-discovery — resolve_effective handles this
            component::resolve(None)?.id.as_str(),
            args.path.clone(),
            ExtensionCapability::Build,
            Vec::new(),
        ))?;
        return build::run_component_with_changed_since(
            &ctx.component,
            args.changed_since.as_deref(),
        );
    }

    let target_id = args.target_id.as_ref().ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "input",
            "Provide component ID, project ID with --all, or JSON spec",
            None,
            Some(vec![
                "Build a single component: homeboy build <component-id>".to_string(),
                "Build all project components: homeboy build <project-id> --all".to_string(),
            ]),
        )
    })?;

    // --all mode: build all components in project
    if args.all {
        let proj = project::load(target_id).map_err(|e| {
            homeboy::core::Error::validation_invalid_argument(
                "project_id",
                format!("'{}' is not a valid project ID", target_id),
                None,
                Some(vec![
                    format!("Error: {}", e),
                    "Use --all only with a project ID: homeboy build <project-id> --all"
                        .to_string(),
                ]),
            )
        })?;

        if proj.components.is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "project_id",
                format!("Project '{}' has no components configured", target_id),
                None,
                Some(vec![format!(
                    "Add components: homeboy project components add {} <component-id> or attach a repo: homeboy project components attach-path {} <component-id> <path>",
                    target_id,
                    target_id
                )]),
            ));
        }

        let components =
            scope::resolve_scope_component_records(&Scope::Project(target_id.clone()))?;
        return run_components(&components);
    }

    // Multiple positional args: use shared resolver
    if !args.component_ids.is_empty() {
        let (project_id, component_ids) =
            resolve_project_components(target_id, &args.component_ids)?;

        // Validate all components belong to this project
        let proj = project::load(&project_id)?;
        let invalid: Vec<_> = component_ids
            .iter()
            .filter(|c| !project::has_component(&proj, c))
            .collect();

        if !invalid.is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "component_ids",
                format!(
                    "Components not in project '{}': {}",
                    project_id,
                    invalid
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None,
                Some(vec![format!(
                    "Project components: {}",
                    project::project_component_ids(&proj).join(", ")
                )]),
            ));
        }

        let project_components =
            scope::resolve_scope_component_records(&Scope::Project(project_id.clone()))?;
        let components: Vec<_> = component_ids
            .iter()
            .filter_map(|id| {
                project_components
                    .iter()
                    .find(|component| component.id == *id)
                    .cloned()
            })
            .collect();

        return run_components(&components);
    }

    // Single target_id: treat as component ID
    if let Some(ref path) = args.path {
        build::run_with_path_changed_since(target_id, path, args.changed_since.as_deref())
    } else {
        build::run_changed_since(target_id, args.changed_since.as_deref())
    }
}

use clap::Args;
use homeboy::build;
use homeboy::component;
use homeboy::project;
use homeboy::resolve::resolve_project_components;

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
}

pub fn run(
    args: BuildArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<build::BuildResult> {
    // Priority: --json > --all with project > positional args

    // JSON takes precedence
    if let Some(ref json) = args.json {
        return build::run(json);
    }

    // No target_id: try CWD auto-discovery (registered component or homeboy.json)
    if args.target_id.is_none() && args.component_ids.is_empty() && !args.all {
        let mut resolved = component::resolve(None)?;
        if let Some(ref path) = args.path {
            resolved.local_path = path.clone();
        }
        return build::run_component(&resolved);
    }

    let target_id = args.target_id.as_ref().ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
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
            homeboy::Error::validation_invalid_argument(
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

        if proj.component_ids.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "project_id",
                format!("Project '{}' has no components configured", target_id),
                None,
                Some(vec![format!(
                    "Add components: homeboy project components add {} <component-id>",
                    target_id
                )]),
            ));
        }

        let json_spec = serde_json::json!({
            "componentIds": proj.component_ids
        })
        .to_string();

        return build::run(&json_spec);
    }

    // Multiple positional args: use shared resolver
    if !args.component_ids.is_empty() {
        let (project_id, component_ids) =
            resolve_project_components(target_id, &args.component_ids)?;

        // Validate all components belong to this project
        let proj = project::load(&project_id)?;
        let invalid: Vec<_> = component_ids
            .iter()
            .filter(|c| !proj.component_ids.contains(c))
            .collect();

        if !invalid.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
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
                    proj.component_ids.join(", ")
                )]),
            ));
        }

        let json_spec = serde_json::json!({
            "componentIds": component_ids
        })
        .to_string();

        return build::run(&json_spec);
    }

    // Single target_id: treat as component ID
    if let Some(ref path) = args.path {
        build::run_with_path(target_id, path)
    } else {
        build::run(target_id)
    }

    pub fn run(args: TransferArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<TransferOutput> {
        todo!("run")
    }
}

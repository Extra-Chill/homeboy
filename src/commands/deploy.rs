use clap::Args;
use serde::Serialize;

use homeboy::deploy::{self, ComponentDeployResult, DeployConfig, DeploySummary};

use super::CmdResult;

#[derive(Args)]
pub struct DeployArgs {
    /// Project ID (or component ID - order is auto-detected)
    pub project_id: String,

    /// Component IDs to deploy (or project ID if first arg is a component)
    pub component_ids: Vec<String>,

    /// Explicit project ID (takes precedence over positional detection)
    #[arg(long, short = 'p')]
    pub project: Option<String>,

    /// JSON input spec for bulk operations
    #[arg(long)]
    pub json: Option<String>,

    /// Deploy all configured components
    #[arg(long)]
    pub all: bool,

    /// Deploy only outdated components
    #[arg(long)]
    pub outdated: bool,

    /// Preview what would be deployed without executing
    #[arg(long)]
    pub dry_run: bool,

    /// Check component status without building or deploying
    #[arg(long, visible_alias = "status")]
    pub check: bool,

    /// Deploy even with uncommitted changes
    #[arg(long)]
    pub force: bool,
}

#[derive(Serialize)]

pub struct DeployOutput {
    pub command: String,
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}

/// Detects whether user provided project-first or component-first order.
/// Supports both `deploy <project> <component>` and `deploy <component> <project>`.
/// When only a component ID is provided, attempts to infer the project from:
/// 1. The component's unique project association
/// 2. The current working directory context
fn resolve_argument_order(
    first: &str,
    rest: &[String],
) -> homeboy::Result<(String, Vec<String>)> {
    let projects = homeboy::project::list_ids().unwrap_or_default();
    let components = homeboy::component::list_ids().unwrap_or_default();

    if projects.contains(&first.to_string()) {
        // Standard order: project first
        Ok((first.to_string(), rest.to_vec()))
    } else if components.contains(&first.to_string()) {
        // Component first - find project in rest or infer from context
        if let Some(project_idx) = rest.iter().position(|r| projects.contains(r)) {
            // Project found in remaining args
            let project = rest[project_idx].clone();
            let mut comps = vec![first.to_string()];
            comps.extend(
                rest.iter()
                    .enumerate()
                    .filter(|(i, _)| *i != project_idx)
                    .map(|(_, s)| s.clone()),
            );
            Ok((project, comps))
        } else {
            // No project in args - try to infer from component's project associations
            let mut all_component_ids = vec![first.to_string()];
            all_component_ids.extend(rest.iter().filter(|r| components.contains(*r)).cloned());

            if let Some(project_id) = infer_project_for_components(&all_component_ids) {
                Ok((project_id, all_component_ids))
            } else {
                // Build helpful error message
                let associated_projects = homeboy::component::projects_using(first)
                    .unwrap_or_default();

                let hint = if associated_projects.is_empty() {
                    format!(
                        "Component '{}' is not associated with any project.\n  \
                        Add it to a project: homeboy project components add <project> {}",
                        first, first
                    )
                } else if associated_projects.len() == 1 {
                    format!(
                        "Component '{}' belongs to project '{}'.\n  \
                        Run: homeboy deploy {} {}",
                        first, associated_projects[0], associated_projects[0], first
                    )
                } else {
                    format!(
                        "Component '{}' belongs to multiple projects: {}.\n  \
                        Specify the project explicitly: homeboy deploy <project> {}",
                        first, associated_projects.join(", "), first
                    )
                };

                Err(homeboy::Error::validation_invalid_argument(
                    "project_id",
                    "No project ID found in arguments and could not be inferred",
                    None,
                    Some(vec![hint]),
                ))
            }
        }
    } else {
        // First arg is neither - provide helpful error
        Err(homeboy::Error::validation_invalid_argument(
            "project_id",
            format!("'{}' is not a known project or component", first),
            None,
            Some(vec![
                format!("Available projects: {}", projects.join(", ")),
                format!("Available components: {}", components.join(", ")),
            ]),
        ))
    }
}

/// Infer project for a set of components.
/// Returns the project ID only if ALL components belong to exactly ONE project.
fn infer_project_for_components(component_ids: &[String]) -> Option<String> {
    if component_ids.is_empty() {
        return None;
    }

    // Get projects for each component
    let mut common_projects: Option<Vec<String>> = None;

    for comp_id in component_ids {
        let projects = homeboy::component::projects_using(comp_id).unwrap_or_default();
        if projects.is_empty() {
            return None; // Component has no project
        }

        match &mut common_projects {
            None => common_projects = Some(projects),
            Some(current) => {
                // Keep only projects that contain all components
                current.retain(|p| projects.contains(p));
                if current.is_empty() {
                    return None; // No common project
                }
            }
        }
    }

    // Return the project only if there's exactly one common project
    common_projects.and_then(|p| {
        if p.len() == 1 {
            Some(p.into_iter().next().unwrap())
        } else {
            None // Ambiguous - multiple projects contain these components
        }
    })
}

pub fn run(mut args: DeployArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DeployOutput> {
    // If --project flag provided, use it directly (first positional becomes component)
    let (project_id, component_ids) = if let Some(ref explicit_project) = args.project {
        let mut comps = vec![args.project_id.clone()];
        comps.extend(args.component_ids.clone());
        (explicit_project.clone(), comps)
    } else {
        // Resolve argument order (supports both project-first and component-first)
        resolve_argument_order(&args.project_id, &args.component_ids)?
    };

    // Update args with resolved values
    args.project_id = project_id.clone();
    args.component_ids = component_ids;

    // Parse JSON input if provided
    if let Some(ref spec) = args.json {
        args.component_ids = deploy::parse_bulk_component_ids(spec)?;
    }

    // Build config and call core orchestration
    let config = DeployConfig {
        component_ids: args.component_ids.clone(),
        all: args.all,
        outdated: args.outdated,
        dry_run: args.dry_run,
        check: args.check,
        force: args.force,
        skip_build: false,
    };

    let result = deploy::run(&project_id, &config).map_err(|e| {
        if e.message.contains("No components configured for project") {
            e.with_hint(format!(
                "Run 'homeboy project components add {} <component-id>' to add components",
                project_id
            ))
            .with_hint("Run 'homeboy init' to see project context and available components")
        } else {
            e
        }
    })?;

    let exit_code = if result.summary.failed > 0 { 1 } else { 0 };

    Ok((
        DeployOutput {
            command: "deploy.run".to_string(),
            project_id: project_id.clone(),
            all: args.all,
            outdated: args.outdated,
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
            results: result.results,
            summary: result.summary,
        },
        exit_code,
    ))
}

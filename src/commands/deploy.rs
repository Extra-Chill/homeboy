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
}

#[derive(Serialize)]

pub struct DeployOutput {
    pub command: String,
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub dry_run: bool,
    pub check: bool,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}

/// Detects whether user provided project-first or component-first order.
/// Supports both `deploy <project> <component>` and `deploy <component> <project>`.
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
        // Reversed order: component first, find project in rest
        if let Some(project_idx) = rest.iter().position(|r| projects.contains(r)) {
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
            Err(homeboy::Error::validation_invalid_argument(
                "project_id",
                "No project ID found in arguments",
                None,
                Some(vec![format!(
                    "Available projects: {}",
                    projects.join(", ")
                )]),
            ))
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

pub fn run(mut args: DeployArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DeployOutput> {
    // Resolve argument order (supports both project-first and component-first)
    let (project_id, component_ids) =
        resolve_argument_order(&args.project_id, &args.component_ids)?;

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
            results: result.results,
            summary: result.summary,
        },
        exit_code,
    ))
}
